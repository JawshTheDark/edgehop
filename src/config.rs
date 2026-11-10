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
            port: net::DEFAULT_PORT,
            screens: Vec::new(),
            trusted_clients: Vec::new(),
            mouse_speed: 1.0,
            edge_resistance: 80,
            server_address: String::new(),
            trusted_server: None,
            screen_override: None,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        if let Some(p) = std::env::var_os("KVMLINK_CONFIG") {
            return PathBuf::from(p);
        }
        directories::ProjectDirs::from("", "", "kvmlink")
            .map(|d| d.config_dir().join("config.json"))
            .unwrap_or_else(|| PathBuf::from("kvmlink-config.json"))
    }

    pub fn load() -> Config {
        let path = Self::path();
        let mut cfg: Config = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut dirty = !path.exists();
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

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_vec_pretty(self) {
            Ok(b) => {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, b).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
            Err(e) => log::error!("could not save config: {e}"),
        }
    }

    pub fn private_key_bytes(&self) -> Vec<u8> {
        net::unhex(&self.private_key).unwrap_or_default()
    }
}
