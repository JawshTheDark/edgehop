//! Clipboard access: text, and file lists for dragging files across screens.
//! On Wayland we shell out to wl-clipboard (KDE and wlroots support it from the
//! background); elsewhere text goes through arboard.
use std::cell::RefCell;
use std::path::PathBuf;

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
    #[cfg(target_os = "linux")]
    if wayland() {
        let b = linux::capture("wl-paste", &["--no-newline", "--type", "text"])?;
        return String::from_utf8(b).ok();
    }
    let t = with_board(|b| b.get_text().ok()).flatten()?;
    (t.len() <= MAX_LEN).then_some(t)
}

pub fn set(text: &str) {
    if text.len() > MAX_LEN {
        return;
    }
    #[cfg(target_os = "linux")]
    if wayland() {
        linux::feed("wl-copy", &[], text.as_bytes());
        return;
    }
    if with_board(|b| b.set_text(text.to_string())).map(|r| r.is_err()).unwrap_or(true) {
        // Clipboard owner may have gone stale; retry with a fresh handle.
        BOARD.with(|b| *b.borrow_mut() = None);
        let _ = with_board(|b| b.set_text(text.to_string()));
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
    let img = with_board(|b| b.get_image().ok()).flatten()?;
    let png = encode_png(img.width as u32, img.height as u32, &img.bytes)?;
    (png.len() < MAX_IMAGE).then_some(png)
}

pub fn set_image_png(png: &[u8]) {
    #[cfg(target_os = "linux")]
    if wayland() {
        linux::feed("wl-copy", &["--type", "image/png"], png);
        return;
    }
    let Some((w, h, rgba)) = decode_png(png) else { return };
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
    #[cfg(windows)]
    return crate::win32::clipboard_files();
    #[cfg(target_os = "linux")]
    return linux::get_files();
    #[allow(unreachable_code)]
    None
}

/// Put files on the clipboard so the file manager's Paste copies them.
/// Returns the text form that was set (used to avoid echoing it as text).
pub fn set_files(paths: &[PathBuf]) -> Option<String> {
    #[cfg(windows)]
    return crate::win32::set_clipboard_files(paths).then(String::new);
    #[cfg(target_os = "linux")]
    return linux::set_files(paths);
    #[allow(unreachable_code)]
    {
        let _ = paths;
        None
    }
}

/// Detects whether a Copy that we trigger actually put files on the clipboard,
/// and afterwards puts back what the user had: the Copy is only how we learn
/// what was being dragged, it must not change this computer's clipboard.
pub struct Probe {
    #[cfg(windows)]
    seq: u32,
    saved_text: Option<String>,
    saved_files: Option<Vec<PathBuf>>,
}

#[cfg(not(windows))]
const SENTINEL: &str = "\u{2063}edgehop-probe";

impl Probe {
    /// Call just before injecting Ctrl+C.
    pub fn start() -> Probe {
        let saved_files = get_files();
        let saved_text = if saved_files.is_some() { None } else { get() };
        #[cfg(windows)]
        {
            Probe { seq: unsafe { crate::win32::GetClipboardSequenceNumber() }, saved_text, saved_files }
        }
        #[cfg(not(windows))]
        {
            // No change counter on Linux: park a marker so stale files aren't mistaken for new ones.
            let saved_text = saved_text.filter(|t| t != SENTINEL);
            set(SENTINEL);
            Probe { saved_text, saved_files }
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
        } else {
            #[cfg(not(windows))]
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
                Ok(Some(status)) => break status.success(),
                Ok(None) if start.elapsed() < Duration::from_millis(1500) => std::thread::sleep(Duration::from_millis(5)),
                _ => {
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
        if let Ok(mut child) =
            Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(data);
            }
            // wl-copy normally hands the data to a background copy of itself and
            // exits at once. If it can't take the clipboard (e.g. the screen is
            // locked) it waits forever — never let that block us.
            let start = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return,
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
