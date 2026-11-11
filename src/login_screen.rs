//! `edgehop --login-screen` (Linux): started at boot by the edgehop-login
//! system service, so the sharing computer's keyboard and mouse work on the
//! login screen too, before anyone has signed in.
//!
//! It borrows one user's Edgehop settings (read-only: same pairing, same
//! name), only moves the pointer and types (no clipboard, no files, no
//! window), and steps aside whenever a user's own Edgehop is running, so
//! there's never more than one of them taking input.
//!
//! Whose settings: the user who switched it on (`edgehop --login-service on`,
//! which Settings runs through pkexec, records that in /etc/edgehop). It runs
//! as root and types into whatever is on screen, so it must not pick up
//! settings another account on this computer left for it to find.
use crate::config::{Config, Role};
use crate::state::{lk, AppState};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Root-owned: the settings file this service uses.
const BOUND: &str = "/etc/edgehop/login-screen";

/// A user's settings file, read as root without trusting what's at that
/// path: no following a link at the end of it, no waiting on a pipe, no
/// reading something huge, and only a regular file.
fn read_settings(path: &Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > 1 << 20 {
        return None;
    }
    let mut text = String::new();
    f.by_ref().take(1 << 20).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Those settings as this service uses them; None unless they're a client
/// paired with a server (anything else, and the service stands by).
fn borrowed_settings(path: &Path) -> Option<Config> {
    let mut cfg: Config = serde_json::from_str(&read_settings(path)?).ok()?;
    if cfg.role != Role::Client || cfg.trusted_server.is_none() || cfg.private_key_bytes().is_empty() {
        return None;
    }
    cfg.clipboard = false; // the login screen has no clipboard to share
    cfg.autostart = false;
    cfg.check_updates = false;
    if cfg.screen_override.is_none() {
        // Before sign-in the desktop can't be asked for its (scaled) size.
        cfg.screen_override = cfg.last_screen;
    }
    Some(cfg)
}

/// The settings to borrow: `--config <path>`, else the ones recorded when the
/// service was switched on. Switched on by hand (`systemctl enable`) with no
/// record: the one user here who has paired Edgehop, never a pick among several.
fn find_settings(args: &[String], said: &mut bool) -> Option<PathBuf> {
    if let Some(i) = args.iter().position(|a| a == "--config") {
        return args.get(i + 1).map(PathBuf::from);
    }
    if let Ok(p) = std::fs::read_to_string(BOUND) {
        let p = PathBuf::from(p.trim());
        return p.is_absolute().then_some(p);
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir("/home")
        .ok()?
        .flatten()
        .map(|home| home.path().join(".config/edgehop/config.json"))
        .filter(|p| borrowed_settings(p).is_some())
        .collect();
    if found.len() > 1 && !*said {
        *said = true;
        crate::state::print_err(
            "edgehop --login-screen: more than one account here has paired Edgehop settings, and it wasn't said \
             whose to use. From the right account, switch it on in Edgehop's Settings, or run: \
             pkexec edgehop --login-service on",
        );
    }
    (found.len() == 1).then(|| found.remove(0))
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

pub fn run(args: &[String]) {
    // Keyboard and mouse only: as root, never read or write files for a server.
    crate::client::INPUT_ONLY.store(true, Ordering::Relaxed);
    // Borrowed settings are never written back (we're root).
    crate::config::set_read_only();
    // Not paired yet: wait for it rather than quit (systemd would start this
    // again every few seconds, forever).
    let (mut said, mut said_several) = (false, false);
    let (path, cfg) = loop {
        if let Some(p) = find_settings(args, &mut said_several) {
            if let Some(cfg) = borrowed_settings(&p) {
                break (p, cfg);
            }
        }
        if !said && !said_several {
            crate::state::print_err(
                "edgehop --login-screen: no paired Edgehop settings to use yet; pair this computer in Edgehop \
                 (signed in) and this service picks them up within a minute",
            );
            said = true;
        }
        std::thread::sleep(Duration::from_secs(60));
    };
    std::env::set_var("EDGEHOP_CONFIG", &path);
    let app = AppState::new(cfg);
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
    let mut unpaired = false;
    loop {
        let theirs = users_edgehop_running();
        crate::client::STAND_DOWN.store(theirs, Ordering::Relaxed);
        if theirs && connected {
            log::info!("your own Edgehop is running; stepping aside");
            app.stop_engine();
            connected = false;
        } else if !theirs && !connected {
            // Pairing or screen size may have changed since last time; and if
            // these are no longer a paired client's settings, stand by.
            match borrowed_settings(&path) {
                Some(cfg) => {
                    unpaired = false;
                    log::info!("nobody's Edgehop is running: connecting, so you can type into the login screen");
                    *lk(&app.config) = cfg;
                    app.start_engine();
                    connected = true;
                }
                None if !unpaired => {
                    unpaired = true;
                    log::warn!("{} is no longer a paired client's settings; standing by", path.display());
                }
                None => {}
            }
        }
        if !connected {
            *lk(&app.status) = "Standing by".into();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `edgehop --login-service on|off`, run as root (Settings does it through
/// pkexec): record whose settings the service uses, then switch it on or off.
pub fn service(on: bool) -> i32 {
    if unsafe { libc::geteuid() } != 0 {
        crate::state::print_err("this needs administrator rights: run it through pkexec or sudo");
        return 1;
    }
    if on {
        // The account that asked (pkexec and sudo both say who).
        let uid = ["PKEXEC_UID", "SUDO_UID"].iter().find_map(|v| std::env::var(v).ok()?.parse::<u32>().ok());
        let Some(home) = uid.filter(|&u| u != 0).and_then(home_of) else {
            crate::state::print_err("run this from your own account through pkexec or sudo (not from a root shell)");
            return 1;
        };
        let path = home.join(".config/edgehop/config.json");
        if borrowed_settings(&path).is_none() {
            crate::state::print_err(&format!(
                "{} isn't a paired client's settings: pair this computer with the sharing one first",
                path.display()
            ));
            return 1;
        }
        let saved = std::fs::create_dir_all("/etc/edgehop")
            .and_then(|()| std::fs::write(BOUND, format!("{}\n", path.display())));
        if let Err(e) = saved {
            crate::state::print_err(&format!("couldn't write {BOUND}: {e}"));
            return 1;
        }
    }
    let ok = std::process::Command::new("systemctl")
        .args([if on { "enable" } else { "disable" }, "--now", "edgehop-login.service"])
        .status()
        .is_ok_and(|s| s.success());
    if on && ok {
        // Already running with someone else's (or older) settings? Start afresh.
        let _ = std::process::Command::new("systemctl").args(["restart", "edgehop-login.service"]).status();
    }
    if !on {
        let _ = std::fs::remove_file(BOUND);
    }
    if ok {
        0
    } else {
        1
    }
}

fn home_of(uid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir).to_bytes();
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir)))
    }
}
