//! Moving files across a screen edge. The sender walks the dragged paths and
//! streams them as messages; the receiver writes them into a fresh temp folder
//! and puts the result on its clipboard, ready to paste.
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

const CHUNK: usize = 48 * 1024;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "f")]
pub enum FileMsg {
    /// `count` top-level items, `total` bytes overall.
    Begin { from: String, count: u32, total: u64 },
    /// `rel` uses '/' separators and starts with the top-level item's name.
    Entry { rel: String, size: u64, dir: bool },
    Data { b64: String },
    End,
    Abort { reason: String },
}

/// Walk `paths` and emit the whole transfer. Stops at the first send error.
pub fn stream(from: &str, paths: &[PathBuf], mut send: impl FnMut(FileMsg) -> io::Result<()>) -> io::Result<u64> {
    let mut entries = Vec::new();
    for p in paths {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "item".into());
        collect(p, name, &mut entries)?;
    }
    let total: u64 = entries.iter().map(|e| e.3).sum();
    send(FileMsg::Begin { from: from.to_string(), count: paths.len() as u32, total })?;
    let mut buf = vec![0u8; CHUNK];
    for (abs, rel, dir, size) in entries {
        send(FileMsg::Entry { rel, size, dir })?;
        if !dir {
            let mut f = File::open(&abs)?;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                send(FileMsg::Data { b64: B64.encode(&buf[..n]) })?;
            }
        }
    }
    send(FileMsg::End)?;
    Ok(total)
}

fn collect(abs: &Path, rel: String, out: &mut Vec<(PathBuf, String, bool, u64)>) -> io::Result<()> {
    let md = fs::symlink_metadata(abs)?;
    if md.is_dir() {
        out.push((abs.to_path_buf(), rel.clone(), true, 0));
        let mut kids: Vec<_> = fs::read_dir(abs)?.filter_map(|e| e.ok()).collect();
        kids.sort_by_key(|e| e.file_name());
        for e in kids {
            collect(&e.path(), format!("{rel}/{}", e.file_name().to_string_lossy()), out)?;
        }
    } else if md.is_file() {
        out.push((abs.to_path_buf(), rel, false, md.len()));
    }
    // symlinks and special files are skipped
    Ok(())
}

/// Turn a received relative path into a safe path under `root`
/// (no "..", no absolute paths, no drive letters, no reserved characters).
pub fn safe_rel(rel: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        let bad = part.is_empty()
            || part == "."
            || part == ".."
            || part.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
        if bad {
            return None;
        }
        out.push(part);
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Where received files go: a per-app cache folder (not /tmp, which may be RAM).
pub fn received_root() -> PathBuf {
    directories::ProjectDirs::from("", "", "kvmlink")
        .map(|d| d.cache_dir().join("received"))
        .unwrap_or_else(|| std::env::temp_dir().join("kvmlink-received"))
}

/// Delete drops older than a day.
pub fn cleanup_old() {
    let Ok(dir) = fs::read_dir(received_root()) else { return };
    for e in dir.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map_or(false, |age| age > Duration::from_secs(24 * 3600));
        if old {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

pub struct Done {
    pub from: String,
    /// Top-level items, ready for the clipboard.
    pub items: Vec<PathBuf>,
    pub bytes: u64,
}

pub struct Sink {
    root: PathBuf,
    from: String,
    current: Option<File>,
    items: Vec<PathBuf>,
    got: u64,
}

impl Sink {
    pub fn begin(from: &str, _total: u64) -> io::Result<Sink> {
        static N: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let root = received_root().join(format!("{stamp}-{}", N.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root)?;
        Ok(Sink { root, from: from.to_string(), current: None, items: Vec::new(), got: 0 })
    }

    /// Feed one message; returns the finished transfer on `End`.
    pub fn handle(&mut self, msg: FileMsg) -> io::Result<Option<Done>> {
        match msg {
            FileMsg::Begin { .. } => {}
            FileMsg::Entry { rel, dir, .. } => {
                self.current = None;
                let rel_path = safe_rel(&rel).ok_or_else(|| io::Error::other(format!("unsafe path {rel:?}")))?;
                let path = self.root.join(&rel_path);
                if rel_path.components().count() == 1 {
                    self.items.push(path.clone());
                }
                if dir {
                    fs::create_dir_all(&path)?;
                } else {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    self.current = Some(File::create(&path)?);
                }
            }
            FileMsg::Data { b64 } => {
                let bytes = B64.decode(b64.as_bytes()).map_err(io::Error::other)?;
                self.got += bytes.len() as u64;
                self.current.as_mut().ok_or_else(|| io::Error::other("data without a file"))?.write_all(&bytes)?;
            }
            FileMsg::End => {
                self.current = None;
                return Ok(Some(Done { from: self.from.clone(), items: self.items.clone(), bytes: self.got }));
            }
            FileMsg::Abort { reason } => return Err(io::Error::other(reason)),
        }
        Ok(None)
    }

}

/// Feed an incoming message into the current transfer (starting one on `Begin`).
pub fn feed(sink: &mut Option<Sink>, m: FileMsg) -> io::Result<Option<Done>> {
    if let FileMsg::Begin { from, total, .. } = &m {
        *sink = Some(Sink::begin(from, *total)?);
    }
    let Some(s) = sink.as_mut() else { return Ok(None) };
    let r = s.handle(m);
    if !matches!(r, Ok(None)) {
        *sink = None; // finished or failed
    }
    r
}

/// Desktop notification (Linux); elsewhere the app's status line covers it.
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["--app-name=kvmlink", "--icon=kvmlink", title, body])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (title, body);
}

/// Copy received items into `dest`, renaming ("name (2).ext") instead of overwriting.
pub fn copy_into(items: &[PathBuf], dest: &Path) -> io::Result<Vec<PathBuf>> {
    fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
        if from.is_dir() {
            fs::create_dir_all(to)?;
            for e in fs::read_dir(from)? {
                let e = e?;
                copy_tree(&e.path(), &to.join(e.file_name()))?;
            }
            Ok(())
        } else {
            fs::copy(from, to).map(|_| ())
        }
    }
    let mut placed = Vec::new();
    for item in items {
        let Some(name) = item.file_name() else { continue };
        let target = unique_path(&dest.join(name));
        copy_tree(item, &target)?;
        placed.push(target);
    }
    Ok(placed)
}

fn unique_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|i| p.with_file_name(format!("{stem} ({i}){ext}")))
        .find(|c| !c.exists())
        .unwrap()
}

pub fn describe(items: &[PathBuf], bytes: u64) -> String {
    let what = match items {
        [one] => format!("\"{}\"", one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        many => format!("{} items", many.len()),
    };
    format!("{what} ({})", human(bytes))
}

pub fn human(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_paths_are_rejected() {
        for bad in ["../x", "a/../../b", "/etc/passwd", "C:/x", "a\\b", "", "a//b", "a/./b"] {
            assert!(safe_rel(bad).is_none(), "{bad:?} should be rejected");
        }
        assert_eq!(safe_rel("folder/sub/file.txt").unwrap(), PathBuf::from("folder").join("sub").join("file.txt"));
    }

    #[test]
    fn folder_round_trip() {
        let src = std::env::temp_dir().join(format!("kvmlink-test-src-{}", std::process::id()));
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(src.join("photos/nested")).unwrap();
        fs::write(src.join("photos/a.txt"), b"hello").unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(src.join("photos/nested/big.bin"), &big).unwrap();
        fs::write(src.join("single.txt"), b"one").unwrap();

        let mut msgs = Vec::new();
        let total = stream("pc", &[src.join("photos"), src.join("single.txt")], |m| {
            msgs.push(m);
            Ok(())
        })
        .unwrap();
        assert_eq!(total, 5 + 200_000 + 3);

        let mut sink = Sink::begin("pc", total).unwrap();
        let mut done = None;
        for m in msgs {
            if let Some(d) = sink.handle(m).unwrap() {
                done = Some(d);
            }
        }
        let done = done.expect("finished");
        assert_eq!(done.items.len(), 2, "two top-level items for the clipboard");
        let root = done.items[0].parent().unwrap().to_path_buf();
        assert_eq!(fs::read(root.join("photos/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(root.join("photos/nested/big.bin")).unwrap(), big);
        assert_eq!(fs::read(root.join("single.txt")).unwrap(), b"one");
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn hostile_sender_cannot_escape_the_folder() {
        let mut sink = Sink::begin("evil", 0).unwrap();
        let r = sink.handle(FileMsg::Entry { rel: "../../outside.txt".into(), size: 1, dir: false });
        assert!(r.is_err());
        let _ = fs::remove_dir_all(&sink.root);
    }

    #[test]
    fn copy_into_never_overwrites() {
        let base = std::env::temp_dir().join(format!("kvmlink-test-copy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (src, dest) = (base.join("src"), base.join("dest"));
        fs::create_dir_all(src.join("folder")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(src.join("a.txt"), b"new").unwrap();
        fs::write(src.join("folder/b.txt"), b"b").unwrap();
        fs::write(dest.join("a.txt"), b"old").unwrap();
        let placed = copy_into(&[src.join("a.txt"), src.join("folder")], &dest).unwrap();
        assert_eq!(placed[0], dest.join("a (2).txt"));
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"old", "existing file untouched");
        assert_eq!(fs::read(dest.join("a (2).txt")).unwrap(), b"new");
        assert_eq!(fs::read(dest.join("folder/b.txt")).unwrap(), b"b");
        let _ = fs::remove_dir_all(&base);
    }
}
