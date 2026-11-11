use crate::layout::ScreenPos;
use crate::net;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Server,
    Client,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Peer {
    pub name: String,
    /// Hex-encoded X25519 public key.
    pub key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub role: Role,
    pub name: String,
    pub private_key: String,
    pub public_key: String,
    pub clipboard: bool,
    pub autostart: bool,
    /// Server: hide this computer's cursor while the mouse is on another one.
    pub hide_cursor_when_away: bool,
    /// Client: hide the pointer / dim the screen while the mouse is elsewhere.
    pub hide_pointer_when_away: bool,
    pub dim_when_away: bool,
    /// Percent of the normal brightness while dimmed.
    pub dim_level: u8,
    /// Parallel connections per file transfer.
    pub transfer_conns: u8,
    /// Files copied to the clipboard are pulled automatically up to this size.
    pub clipboard_files_max_gb: u32,
    /// "info" normally; "debug" or "trace" when diagnosing a problem.
    pub log_level: String,

    // --- server ---
    pub port: u16,
    /// Client screens, positioned against the server's monitors.
    pub screens: Vec<ScreenPos>,
    pub trusted_clients: Vec<Peer>,
    pub mouse_speed: f32,
    /// Pixels you must keep pushing past a screen edge before switching.
    pub edge_resistance: u32,

    // --- client ---
    /// Empty = find the server automatically.
    pub server_address: String,
    pub trusted_server: Option<Peer>,
    pub screen_override: Option<(i32, i32)>,
    /// Mac: Ctrl from the server's keyboard acts as Command (Ctrl+C copies).
    pub ctrl_is_command: bool,
    /// Sharing computer: game mode turns on by itself while a fullscreen game
    /// or app is in front (Windows).
    pub game_mode_auto: bool,
    /// Client: the screen size last seen in a desktop session, for the
    /// login-screen service (which can't ask the desktop).
    pub last_screen: Option<(i32, i32)>,
    /// Linux client: swap Ctrl and Super for keys from the server, because
    /// this computer swaps them (Super works as Ctrl). None = as detected.
    pub swap_ctrl_super: Option<bool>,
    /// Look for a newer Edgehop on GitHub every few hours.
    pub check_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            role: if cfg!(windows) { Role::Server } else { Role::Client },
            name: gethostname::gethostname().to_string_lossy().into_owned(),
            private_key: String::new(),
            public_key: String::new(),
            clipboard: true,
            autostart: false,
            hide_cursor_when_away: true,
            hide_pointer_when_away: true,
            dim_when_away: true,
            dim_level: 25,
            transfer_conns: crate::transfer::DEFAULT_CONNS,
            clipboard_files_max_gb: 20,
            log_level: "info".into(),
            port: net::DEFAULT_PORT,
            screens: Vec::new(),
            trusted_clients: Vec::new(),
            mouse_speed: 1.0,
            edge_resistance: 80,
            server_address: String::new(),
            trusted_server: None,
            screen_override: None,
            ctrl_is_command: cfg!(target_os = "macos"),
            game_mode_auto: true,
            last_screen: None,
            swap_ctrl_super: None,
            check_updates: true,
        }
    }
}

/// Never write the settings file (the login-screen service runs as root and
/// reads the signed-in user's settings).
static READ_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The settings file couldn't be read or moved aside: don't write over it.
static NO_SAVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LOAD_PROBLEM: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn set_read_only() {
    READ_ONLY.store(true, std::sync::atomic::Ordering::Relaxed);
}

impl Config {
    pub fn path() -> PathBuf {
        if let Some(p) = std::env::var_os("EDGEHOP_CONFIG") {
            return PathBuf::from(p);
        }
        directories::ProjectDirs::from("", "", "edgehop")
            .map(|d| d.config_dir().join("config.json"))
            .unwrap_or_else(|| PathBuf::from("edgehop-config.json"))
    }

    /// Where the app kept its settings before it was renamed to Edgehop.
    fn legacy_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "kvmlink").map(|d| d.config_dir().join("config.json"))
    }

    pub fn load() -> Config {
        let path = Self::path();
        // Carry identity, pairings and layout over from the old name.
        if !path.exists() && std::env::var_os("EDGEHOP_CONFIG").is_none() {
            if let Some(old) = Self::legacy_path().filter(|p| p.exists()) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if std::fs::copy(&old, &path).is_ok() {
                    crate::state::print_err(&format!("migrated settings from {}", old.display()));
                }
            }
        }
        // Only a file that isn't there means "new install". One that can't be
        // read or understood (a typo from editing by hand, a half-written
        // file after a power cut, a newer version's format) is kept aside,
        // never silently replaced: it holds this computer's identity and
        // every pairing.
        let read_only = READ_ONLY.load(std::sync::atomic::Ordering::Relaxed);
        let mut dirty = false;
        let mut cfg: Config = match std::fs::read(&path) {
            Ok(b) => match serde_json::from_slice(&b) {
                Ok(c) => c,
                Err(e) => {
                    Self::set_aside(&path, &format!("can't be understood ({e})"), read_only);
                    Config::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                dirty = true;
                Config::default()
            }
            Err(e) => {
                Self::set_aside(&path, &format!("can't be read ({e})"), read_only);
                Config::default()
            }
        };
        if cfg.private_key.is_empty() || net::unhex(&cfg.private_key).is_none() {
            let (private, public) = net::generate_keypair();
            cfg.private_key = net::hex(&private);
            cfg.public_key = net::hex(&public);
            dirty = true;
        }
        if cfg.name.trim().is_empty() {
            cfg.name = Config::default().name;
            dirty = true;
        }
        if dirty {
            cfg.save();
        }
        cfg
    }

    /// Move a settings file that couldn't be loaded out of the way (so
    /// starting fresh doesn't destroy it), and remember to say so.
    fn set_aside(path: &std::path::Path, why: &str, read_only: bool) {
        let mut msg = format!("the settings file {} {why}", path.display());
        if read_only {
            *LOAD_PROBLEM.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
            return;
        }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let bad = path.with_extension(format!("json.bad-{stamp}"));
        match std::fs::rename(path, &bad) {
            Ok(()) => msg.push_str(&format!(
                "; it was kept as {} and Edgehop started with new settings (computers need pairing again).                  To get the old ones back, quit Edgehop, fix that file and rename it to config.json",
                bad.display()
            )),
            Err(e) => {
                // Can't even move it: don't write over it this run.
                NO_SAVE.store(true, std::sync::atomic::Ordering::Relaxed);
                msg.push_str(&format!("; it couldn't be moved aside either ({e}), so settings won't be saved this time"));
            }
        }
        crate::state::print_err(&msg);
        *LOAD_PROBLEM.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
    }

    /// What went wrong loading the settings, if anything (for the log, once it's up).
    pub fn load_problem() -> Option<String> {
        LOAD_PROBLEM.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn save(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        if READ_ONLY.load(Relaxed) || NO_SAVE.load(Relaxed) {
            return; // the login-screen service only borrows the user's settings
        }
        // One save at a time, each through its own temporary file.
        static SAVING: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
        let mut n = SAVING.lock().unwrap_or_else(|e| e.into_inner());
        *n += 1;
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let bytes = match serde_json::to_vec_pretty(self) {
            Ok(b) => b,
            Err(e) => return log::error!("could not save settings: {e}"),
        };
        let tmp = path.with_extension(format!("json.tmp-{}-{}", std::process::id(), *n));
        let written = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            // It holds this computer's private key: for this user only.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            let mut f = o.open(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?; // on disk before it replaces the old one
            drop(f);
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            log::error!("could not save settings to {}: {e}", path.display());
        }
    }

    pub fn private_key_bytes(&self) -> Vec<u8> {
        net::unhex(&self.private_key).unwrap_or_default()
    }
}
