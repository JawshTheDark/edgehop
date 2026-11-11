//! `edgehop --login-screen` (Linux): started at boot by the edgehop-login
//! system service, so the sharing computer's keyboard and mouse work on the
//! login screen too, before anyone has signed in.
//!
//! It borrows the signed-in user's Edgehop settings (read-only: same pairing,
//! same name), only moves the pointer and types (no clipboard, no window), and
//! steps aside whenever that user's own Edgehop is running, so there's never
//! more than one of them taking input.
use crate::state::{lk, AppState};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// The Edgehop settings to borrow: `--config <path>`, else the most recently
/// changed client settings under /home.
fn find_settings(args: &[String]) -> Option<PathBuf> {
    if let Some(i) = args.iter().position(|a| a == "--config") {
        return args.get(i + 1).map(PathBuf::from);
    }
    let homes = std::fs::read_dir("/home").ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for home in homes.flatten() {
        let path = home.path().join(".config/edgehop/config.json");
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(cfg) = serde_json::from_str::<crate::config::Config>(&text) else { continue };
        if cfg.role != crate::config::Role::Client || cfg.trusted_server.is_none() {
            continue;
        }
        let changed = std::fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map_or(true, |(t, _)| changed > *t) {
            best = Some((changed, path));
        }
    }
    best.map(|(_, p)| p)
}

/// A signed-in user's own Edgehop is running (any `edgehop` process that
/// isn't root, i.e. not this service).
fn users_edgehop_running() -> bool {
    let me = std::process::id().to_string();
    let Ok(procs) = std::fs::read_dir("/proc") else { return false };
    procs.flatten().any(|p| {
        let pid = p.file_name().to_string_lossy().into_owned();
        if pid == me || !pid.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        let dir = p.path();
        let is_edgehop = std::fs::read_to_string(dir.join("comm")).is_ok_and(|c| c.trim() == "edgehop");
        is_edgehop && owner_uid(&dir).is_some_and(|uid| uid != 0)
    })
}

fn owner_uid(proc_dir: &Path) -> Option<u32> {
    let status = std::fs::read_to_string(proc_dir.join("status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// The signed-in user's settings, as this service uses them.
fn borrowed_settings() -> crate::config::Config {
    let mut cfg = crate::config::Config::load();
    cfg.clipboard = false; // the login screen has no clipboard to share
    cfg.autostart = false;
    if cfg.screen_override.is_none() {
        // Before sign-in the desktop can't be asked for its (scaled) size.
        cfg.screen_override = cfg.last_screen;
    }
    cfg
}

pub fn run(args: &[String]) {
    // Not paired yet: wait for it rather than quit (systemd would start this
    // again every few seconds, forever).
    let mut said = false;
    let path = loop {
        if let Some(p) = find_settings(args) {
            break p;
        }
        if !said {
            crate::state::print_err(
                "edgehop --login-screen: no paired Edgehop settings under /home yet; pair this computer in \
                 Edgehop (signed in) and this service picks them up within a minute",
            );
            said = true;
        }
        std::thread::sleep(Duration::from_secs(60));
    };
    // Borrow them: load from there, never write back (we're root).
    std::env::set_var("EDGEHOP_CONFIG", &path);
    crate::config::set_read_only();
    let app = AppState::new(borrowed_settings());
    crate::state::init_logger(app.clone(), args);
    {
        let c = app.cfg();
        log::info!(
            "Edgehop {} at the login screen, as {:?} (settings {}), screen {:?}",
            env!("CARGO_PKG_VERSION"),
            c.name,
            path.display(),
            c.screen_override
        );
    }
    let mut connected = false;
    loop {
        let theirs = users_edgehop_running();
        crate::client::STAND_DOWN.store(theirs, Ordering::Relaxed);
        if theirs && connected {
            log::info!("your own Edgehop is running; stepping aside");
            app.stop_engine();
            connected = false;
        } else if !theirs && !connected {
            log::info!("nobody's Edgehop is running: connecting, so you can type into the login screen");
            // Pairing or screen size may have changed since last time.
            *lk(&app.config) = borrowed_settings();
            app.start_engine();
            connected = true;
        }
        if !connected {
            *lk(&app.status) = "Standing by".into();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
