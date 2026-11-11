//! Clipboard access: text, and file lists for dragging files across screens.
//! On Wayland we shell out to wl-clipboard (KDE and wlroots support it from the
//! background); elsewhere text goes through arboard.
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

const MAX_LEN: usize = 4 * 1024 * 1024;
/// Largest image we'll move (PNG bytes): a 4K screenshot fits comfortably.
const MAX_IMAGE: usize = 12 * 1024 * 1024;

thread_local! {
    // Kept alive per thread: on X11 the clipboard owner must outlive the copy.
    static BOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

fn with_board<R>(f: impl FnOnce(&mut arboard::Clipboard) -> R) -> Option<R> {
    BOARD.with(|b| {
        let mut g = b.borrow_mut();
        if g.is_none() {
            *g = arboard::Clipboard::new().ok();
        }
        g.as_mut().map(f)
    })
}

#[cfg(target_os = "linux")]
fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).exists()))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() && on_path("wl-paste")
}

pub fn get() -> Option<String> {
    let start = Instant::now();
    #[cfg(target_os = "linux")]
    if wayland() {
        let t = linux::capture("wl-paste", &["--no-newline", "--type", "text"]).and_then(|b| String::from_utf8(b).ok());
        log::debug!(
            "read text via wl-paste: {} ({} ms)",
            t.as_ref().map_or("none".to_string(), |t| format!("{} chars", t.chars().count())),
            start.elapsed().as_millis()
        );
        return t;
    }
    match with_board(|b| b.get_text()) {
        Some(Ok(t)) => {
            log::debug!("read text: {} chars ({} ms)", t.chars().count(), start.elapsed().as_millis());
            (t.len() <= MAX_LEN).then_some(t)
        }
        Some(Err(e)) => {
            log::debug!("no text on the clipboard ({e})");
            None
        }
        None => {
            log::warn!("clipboard unavailable");
            None
        }
    }
}

pub fn set(text: &str) {
    if text.len() > MAX_LEN {
        log::warn!("not setting {} chars of text: over the {} limit", text.len(), crate::files::human(MAX_LEN as u64));
        return;
    }
    log::debug!("setting clipboard text ({} chars)", text.chars().count());
    #[cfg(target_os = "linux")]
    if wayland() {
        linux::feed("wl-copy", &[], text.as_bytes());
        return;
    }
    if let Some(Err(e)) = with_board(|b| b.set_text(text.to_string())) {
        // Clipboard owner may have gone stale; retry with a fresh handle.
        log::debug!("setting text failed ({e}); retrying with a fresh clipboard handle");
        BOARD.with(|b| *b.borrow_mut() = None);
        if let Some(Err(e)) = with_board(|b| b.set_text(text.to_string())) {
            log::warn!("couldn't set clipboard text: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Images (as PNG)
// ---------------------------------------------------------------------------

/// The clipboard image as PNG bytes, if the clipboard holds an image.
pub fn get_image_png() -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    if wayland() {
        let types = linux::capture("wl-paste", &["--list-types"])?;
        if !String::from_utf8_lossy(&types).lines().any(|l| l.trim() == "image/png") {
            return None;
        }
        let png = linux::capture_max("wl-paste", &["--type", "image/png"], MAX_IMAGE)?;
        return (!png.is_empty() && png.len() < MAX_IMAGE).then_some(png);
    }
    let start = Instant::now();
    let img = match with_board(|b| b.get_image()) {
        Some(Ok(img)) => img,
        Some(Err(e)) => {
            log::debug!("no image on the clipboard ({e})");
            return None;
        }
        None => return None,
    };
    let png = encode_png(img.width as u32, img.height as u32, &img.bytes)?;
    log::debug!(
        "read image {}x{} -> PNG {} ({} ms)",
        img.width,
        img.height,
        crate::files::human(png.len() as u64),
        start.elapsed().as_millis()
    );
    if png.len() >= MAX_IMAGE {
        log::warn!("clipboard image is {}; over the {} limit", crate::files::human(png.len() as u64), crate::files::human(MAX_IMAGE as u64));
        return None;
    }
    Some(png)
}

pub fn set_image_png(png: &[u8]) {
    log::debug!("setting clipboard image (PNG {})", crate::files::human(png.len() as u64));
    #[cfg(target_os = "linux")]
    if wayland() {
        linux::feed("wl-copy", &["--type", "image/png"], png);
        return;
    }
    let Some((w, h, rgba)) = decode_png(png) else {
        log::warn!("received image isn't a readable PNG");
        return;
    };
    let img = arboard::ImageData { width: w as usize, height: h as usize, bytes: rgba.into() };
    if with_board(|b| b.set_image(img.clone())).map(|r| r.is_err()).unwrap_or(true) {
        BOARD.with(|b| *b.borrow_mut() = None);
        let _ = with_board(|b| b.set_image(img));
    }
}

#[allow(dead_code)]
fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(rgba).ok()?;
    }
    Some(out)
}

/// PNG -> (width, height, RGBA8).
#[allow(dead_code)]
fn decode_png(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(png));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    // A few bytes of PNG can claim to be millions of pixels each way: don't
    // set aside memory for more than any real screenshot (64 megapixels).
    let (w, h) = (reader.info().width as u64, reader.info().height as u64);
    if w * h > 64_000_000 {
        log::warn!("ignoring a {w}x{h} clipboard image: too large");
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        _ => return None,
    };
    Some((info.width, info.height, rgba))
}

#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn png_round_trip() {
        let (w, h) = (3u32, 2u32);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 256) as u8).collect();
        let png = encode_png(w, h, &rgba).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(decode_png(&png).unwrap(), (w, h, rgba));
    }
}

// ---------------------------------------------------------------------------
// File lists
// ---------------------------------------------------------------------------

/// Files on the clipboard (as a file manager's Copy leaves them).
pub fn get_files() -> Option<Vec<PathBuf>> {
    let files = get_files_inner();
    if let Some(f) = &files {
        log::debug!("clipboard holds {} file(s): {:?}", f.len(), f.iter().take(5).collect::<Vec<_>>());
    }
    files
}

fn get_files_inner() -> Option<Vec<PathBuf>> {
    #[cfg(windows)]
    return crate::win32::clipboard_files();
    #[cfg(target_os = "linux")]
    return linux::get_files();
    #[cfg(target_os = "macos")]
    return crate::mac_files::clipboard_files();
    #[allow(unreachable_code)]
    None
}

/// Put files on the clipboard so the file manager's Paste copies them.
/// Returns the text form that was set (used to avoid echoing it as text).
pub fn set_files(paths: &[PathBuf]) -> Option<String> {
    log::debug!("putting {} file(s) on the clipboard: {:?}", paths.len(), paths.iter().take(5).collect::<Vec<_>>());
    let r = set_files_inner(paths);
    if r.is_none() {
        log::warn!("couldn't put files on the clipboard");
    }
    r
}

fn set_files_inner(paths: &[PathBuf]) -> Option<String> {
    #[cfg(windows)]
    return crate::win32::set_clipboard_files(paths).then(String::new);
    #[cfg(target_os = "linux")]
    return linux::set_files(paths);
    #[cfg(target_os = "macos")]
    return crate::mac_files::set_clipboard_files(paths).then(|| get().unwrap_or_default());
    #[allow(unreachable_code)]
    {
        let _ = paths;
        None
    }
}

/// Detects whether a Copy that we trigger actually put files on the clipboard,
/// and afterwards puts back what the user had: the Copy is only how we learn
/// what was being dragged, it must not change this computer's clipboard.
#[cfg_attr(target_os = "macos", allow(dead_code))] // a Mac reads the drag pasteboard instead
pub struct Probe {
    #[cfg(windows)]
    seq: u32,
    saved_text: Option<String>,
    saved_files: Option<Vec<PathBuf>>,
    /// A copied picture (PNG), when there were neither files nor text.
    saved_image: Option<Vec<u8>>,
}

#[cfg(not(windows))]
#[cfg_attr(target_os = "macos", allow(dead_code))]
const SENTINEL: &str = "\u{2063}edgehop-probe";

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Probe {
    /// Call just before injecting Ctrl+C.
    pub fn start() -> Probe {
        let saved_files = get_files();
        let saved_text = if saved_files.is_some() { None } else { get() };
        // A screenshot or copied picture must survive this too.
        let saved_image = if saved_files.is_some() || saved_text.is_some() { None } else { get_image_png() };
        #[cfg(windows)]
        {
            Probe { seq: unsafe { crate::win32::GetClipboardSequenceNumber() }, saved_text, saved_files, saved_image }
        }
        #[cfg(not(windows))]
        {
            // No change counter on Linux: park a marker so stale files aren't mistaken for new ones.
            let saved_text = saved_text.filter(|t| t != SENTINEL);
            set(SENTINEL);
            Probe { saved_text, saved_files, saved_image }
        }
    }

    /// Wait (up to ~1 s) for the Copy to land; returns the files if it copied files.
    pub fn files(&self) -> Option<Vec<PathBuf>> {
        for _ in 0..25 {
            std::thread::sleep(std::time::Duration::from_millis(40));
            #[cfg(windows)]
            let changed = unsafe { crate::win32::GetClipboardSequenceNumber() } != self.seq;
            #[cfg(not(windows))]
            let changed = get().as_deref() != Some(SENTINEL);
            if changed {
                // Give the file manager a moment to finish publishing every format.
                std::thread::sleep(std::time::Duration::from_millis(60));
                return get_files();
            }
        }
        None
    }

    /// Put back what the user had before our Copy.
    pub fn restore(self) {
        if let Some(files) = self.saved_files {
            set_files(&files);
        } else if let Some(text) = self.saved_text {
            set(&text);
        } else if let Some(png) = self.saved_image {
            set_image_png(&png);
        } else {
            // It was empty: not our marker, and not the files our Copy put
            // there (they'd be offered to the other computer as "copied").
            set("");
        }
    }
}

// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// Run a command and capture stdout (read concurrently so big output can't stall it).
    pub fn capture(cmd: &str, args: &[&str]) -> Option<Vec<u8>> {
        capture_max(cmd, args, MAX_LEN)
    }

    pub fn capture_max(cmd: &str, args: &[&str], max: usize) -> Option<Vec<u8>> {
        let mut child = Command::new(cmd).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let mut out = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.by_ref().take(max as u64).read_to_end(&mut buf);
            buf
        });
        let start = Instant::now();
        let ok = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        // Normal when the clipboard has no data of that type.
                        log::debug!("{cmd} {args:?} exited with {status}");
                    }
                    break status.success();
                }
                Ok(None) if start.elapsed() < Duration::from_millis(1500) => std::thread::sleep(Duration::from_millis(5)),
                _ => {
                    log::warn!("{cmd} {args:?} didn't finish in 1.5 s; gave up");
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        let buf = reader.join().ok()?;
        ok.then_some(buf)
    }

    pub fn feed(cmd: &str, args: &[&str], data: &[u8]) {
        let child = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        if let Err(e) = &child {
            log::warn!("couldn't run {cmd}: {e} (is it installed?)");
        }
        if let Ok(mut child) = child {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(data);
            }
            // wl-copy normally hands the data to a background copy of itself and
            // exits at once. If it can't take the clipboard (e.g. the screen is
            // locked) it waits forever — never let that block us.
            let start = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if status.success() {
                            log::debug!("{cmd} {args:?}: {} in {} ms", crate::files::human(data.len() as u64), start.elapsed().as_millis());
                        } else {
                            log::warn!("{cmd} {args:?} exited with {status}");
                        }
                        return;
                    }
                    Ok(None) if start.elapsed() < Duration::from_secs(2) => std::thread::sleep(Duration::from_millis(10)),
                    _ => {
                        log::warn!("{cmd} didn't finish in 2 s (screen locked?); clipboard not updated");
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                }
            }
        }
    }

    fn uri_encode(path: &std::path::Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        let mut s = String::from("file://");
        for &b in path.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                s.push(b as char);
            } else {
                s.push_str(&format!("%{b:02X}"));
            }
        }
        s
    }

    fn uri_decode(uri: &str) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let rest = uri.strip_prefix("file://")?;
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let b = rest.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }

    pub fn get_files() -> Option<Vec<PathBuf>> {
        let raw = if wayland() {
            capture("wl-paste", &["--no-newline", "--type", "text/uri-list"])?
        } else if on_path("xclip") {
            capture("xclip", &["-selection", "clipboard", "-t", "text/uri-list", "-o"])?
        } else {
            return None;
        };
        let text = String::from_utf8_lossy(&raw);
        let files: Vec<PathBuf> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(uri_decode)
            .filter(|p| p.exists())
            .collect();
        (!files.is_empty()).then_some(files)
    }

    pub fn set_files(paths: &[PathBuf]) -> Option<String> {
        let list: String = paths.iter().map(|p| uri_encode(p) + "\r\n").collect();
        if wayland() {
            feed("wl-copy", &["--type", "text/uri-list"], list.as_bytes());
        } else if on_path("xclip") {
            feed("xclip", &["-selection", "clipboard", "-t", "text/uri-list", "-i"], list.as_bytes());
        } else {
            return None;
        }
        Some(list)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn uri_round_trip() {
            let p = PathBuf::from("/home/j/My Files/ünï #1.txt");
            let u = uri_encode(&p);
            assert!(u.starts_with("file:///home/j/My%20Files/"));
            assert_eq!(uri_decode(&u).unwrap(), p);
        }
    }
}
