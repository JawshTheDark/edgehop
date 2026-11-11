//! File helpers: walking folders, safe received paths, temp folders, unique
//! names, notifications. The transfer itself is in transfer.rs.
use base64::Engine;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub fn collect(abs: &Path, rel: String, out: &mut Vec<(PathBuf, String, bool, u64)>) -> io::Result<()> {
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
        let bad = part.is_empty() || part == "." || part == ".." || part.chars().any(|c| c.is_control() || c == '\\');
        if bad {
            return None;
        }
        // Characters and names Windows can't have in a file name are fine
        // elsewhere; here they'd fail the whole transfer (or open a device).
        if cfg!(windows) {
            let stem = part.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
            let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit());
            if device
                || part.ends_with('.')
                || part.ends_with(' ')
                || part.chars().any(|c| matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
            {
                return None;
            }
        }
        out.push(part);
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Where received files go: a per-app cache folder (not /tmp, which may be RAM).
pub fn received_root() -> PathBuf {
    directories::ProjectDirs::from("", "", "edgehop")
        .map(|d| d.cache_dir().join("received"))
        .unwrap_or_else(|| std::env::temp_dir().join("edgehop-received"))
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

/// Desktop notification (Linux); elsewhere the app's status line covers it.
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["--app-name=Edgehop", "--icon=edgehop", title, body])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (title, body);
}

pub fn unique_path(p: &Path) -> PathBuf {
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

pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

pub fn unb64(s: &str) -> Option<Vec<u8>> {
    B64.decode(s.as_bytes()).ok()
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

}
