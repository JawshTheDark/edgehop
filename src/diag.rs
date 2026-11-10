//! Diagnostics report: everything needed to understand a problem, from every
//! connected computer, in one text file.
use crate::state::{lk, AppState};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The last `max_lines` lines of a text file.
pub fn tail(path: &Path, max_lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else { return String::new() };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(max_lines)..].join("\n")
}

/// This computer's recent log (previous file first, then the current one).
pub fn local_log(max_lines: usize) -> String {
    let prev = tail(&crate::state::previous_log_path(), max_lines);
    let cur = tail(&crate::state::log_path(), max_lines);
    let mut all: Vec<&str> = prev.lines().chain(cur.lines()).collect();
    let start = all.len().saturating_sub(max_lines);
    all.drain(..start);
    all.join("\n")
}

fn summary(app: &AppState) -> String {
    let cfg = app.cfg();
    let mut v = serde_json::to_value(&cfg).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.remove("private_key");
        o.insert("public_key".into(), crate::net::fingerprint(&cfg.public_key).into());
        if let Some(list) = o.get_mut("trusted_clients").and_then(|l| l.as_array_mut()) {
            for p in list {
                if let Some(k) = p.get("key").and_then(|k| k.as_str()).map(crate::net::fingerprint) {
                    p["key"] = k.into();
                }
            }
        }
        if let Some(k) = o.get("trusted_server").and_then(|t| t.get("key")).and_then(|k| k.as_str()).map(crate::net::fingerprint) {
            o["trusted_server"]["key"] = k.into();
        }
    }
    let mut s = String::new();
    s.push_str(&format!("Edgehop {}\n", env!("CARGO_PKG_VERSION")));
    s.push_str(&format!("Created: {}\n", crate::platform::local_datetime()));
    s.push_str(&format!("OS: {} {}\n", std::env::consts::OS, std::env::consts::ARCH));
    s.push_str(&format!("System: {}\n", crate::platform::environment()));
    s.push_str(&format!("Log level: {}\n", crate::state::log_level_name()));
    s.push_str(&format!("Status: {}\n", lk(&app.status)));
    s.push_str(&format!("Mouse on: {}\n", lk(&app.active_screen)));
    for c in lk(&app.connected).iter() {
        s.push_str(&format!("Connected: {} at {} ({}, {}x{})\n", c.name, c.addr, c.os, c.width, c.height));
    }
    for m in crate::platform::monitors() {
        s.push_str(&format!(
            "Monitor: {} {:?} {}x{} at ({}, {}){}\n",
            if m.label.is_empty() { "Display" } else { &m.label },
            m.label,
            m.rect.w() as i32,
            m.rect.h() as i32,
            m.rect.l as i32,
            m.rect.t as i32,
            if m.primary { " primary" } else { "" }
        ));
    }
    s.push_str("\nSettings (keys shortened to fingerprints):\n");
    s.push_str(&serde_json::to_string_pretty(&v).unwrap_or_default());
    s.push('\n');
    s
}

/// Gather logs from every computer and write one report. Blocks up to ~4 s
/// while connected computers send theirs.
pub fn save(app: &Arc<AppState>) -> io::Result<PathBuf> {
    lk(&app.remote_logs).clear();
    let expected: Vec<String> = lk(&app.connected).iter().map(|c| c.name.clone()).collect();
    if app.request_remote_logs() {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(4) {
            if expected.iter().all(|n| lk(&app.remote_logs).contains_key(n)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let name = app.cfg().name;
    let mut out = String::new();
    out.push_str("==================== EDGEHOP DIAGNOSTICS ====================\n");
    out.push_str(&summary(app));
    out.push_str(&format!("\n==================== {name} (this computer) log ====================\n"));
    out.push_str(&local_log(6000));
    out.push('\n');
    out.push_str(&format!("\n==================== {name} (this computer) input ====================\n"));
    out.push_str(&crate::flight::dump());
    for (other, text) in lk(&app.remote_logs).iter() {
        out.push_str(&format!("\n==================== {other} log ====================\n"));
        out.push_str(text);
        out.push('\n');
    }
    for missing in expected.iter().filter(|n| !lk(&app.remote_logs).contains_key(*n)) {
        out.push_str(&format!("\n({missing} didn't send its log in time)\n"));
    }

    let stamp = crate::platform::local_datetime().replace([':', ' '], "-"); // 2026-11-10-14-05-22
    let dir = directories::UserDirs::new()
        .and_then(|u| u.desktop_dir().map(Path::to_path_buf))
        .unwrap_or_else(|| crate::state::log_path().parent().map(Path::to_path_buf).unwrap_or_default());
    let path = dir.join(format!("edgehop-diagnostics-{stamp}.txt"));
    std::fs::write(&path, out)?;
    log::info!("diagnostics saved to {}", path.display());
    Ok(path)
}
