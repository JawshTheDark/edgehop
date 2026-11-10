//! State shared between the GUI, tray and the network/input engine.
use crate::config::{Config, Role};
use crate::net::Discovered;
use crate::{client, server};
use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

pub fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct PairRequest {
    pub id: u64,
    pub name: String,
    pub key: String,
    pub code: String,
    pub addr: String,
    pub reply: mpsc::Sender<bool>,
}

#[derive(Clone, Debug)]
pub struct ConnectedInfo {
    pub name: String,
    pub addr: String,
    pub os: String,
    pub width: i32,
    pub height: i32,
}

pub enum Engine {
    Server(server::ServerEngine),
    Client(client::ClientEngine),
}

impl Engine {
    fn stop(self) {
        match self {
            Engine::Server(s) => s.stop(),
            Engine::Client(c) => c.stop(),
        }
    }
}

pub struct AppState {
    pub config: Mutex<Config>,
    pub status: Mutex<String>,
    pub connected_ok: Mutex<bool>,
    pub active_screen: Mutex<String>,
    pub connected: Mutex<Vec<ConnectedInfo>>,
    pub pending: Mutex<Vec<PairRequest>>,
    pub discovered: Mutex<Vec<Discovered>>,
    pub pairing_code: Mutex<Option<String>>,
    pub key_mismatch: Mutex<Option<String>>,
    pub logs: Mutex<VecDeque<String>>,
    pub ctx: Mutex<Option<eframe::egui::Context>>,
    pub engine: Mutex<Option<Engine>>,
}

impl AppState {
    pub fn new(cfg: Config) -> Arc<AppState> {
        Arc::new(AppState {
            config: Mutex::new(cfg),
            status: Mutex::new("Stopped".into()),
            connected_ok: Mutex::new(false),
            active_screen: Mutex::new(String::new()),
            connected: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            discovered: Mutex::new(Vec::new()),
            pairing_code: Mutex::new(None),
            key_mismatch: Mutex::new(None),
            logs: Mutex::new(VecDeque::new()),
            ctx: Mutex::new(None),
            engine: Mutex::new(None),
        })
    }

    pub fn repaint(&self) {
        if let Some(ctx) = lk(&self.ctx).as_ref() {
            ctx.request_repaint();
        }
    }

    pub fn set_status(&self, s: impl Into<String>, ok: bool) {
        let s = s.into();
        let changed = *lk(&self.status) != s;
        if changed {
            log::info!("status: {s}");
        }
        *lk(&self.status) = s;
        *lk(&self.connected_ok) = ok;
        self.repaint();
    }

    pub fn cfg(&self) -> Config {
        lk(&self.config).clone()
    }

    pub fn update_config(&self, f: impl FnOnce(&mut Config)) {
        let snapshot = {
            let mut c = lk(&self.config);
            f(&mut c);
            c.clone()
        };
        snapshot.save();
        if let Some(Engine::Server(s)) = lk(&self.engine).as_ref() {
            s.apply_config(&snapshot);
        }
        self.repaint();
    }

    pub fn start_engine(self: &Arc<Self>) {
        self.stop_engine();
        let role = lk(&self.config).role;
        let engine = match role {
            Role::Server => match server::ServerEngine::start(self.clone()) {
                Ok(e) => Engine::Server(e),
                Err(e) => {
                    log::error!("{e}");
                    self.set_status(e, false);
                    return;
                }
            },
            Role::Client => Engine::Client(client::ClientEngine::start(self.clone())),
        };
        *lk(&self.engine) = Some(engine);
    }

    pub fn stop_engine(&self) {
        let e = lk(&self.engine).take();
        if let Some(e) = e {
            e.stop();
        }
        lk(&self.connected).clear();
        for p in lk(&self.pending).drain(..) {
            let _ = p.reply.send(false);
        }
        *lk(&self.pairing_code) = None;
        *lk(&self.active_screen) = String::new();
        self.set_status("Stopped", false);
    }

    pub fn is_running(&self) -> bool {
        lk(&self.engine).is_some()
    }

    /// Bring the main window forward (e.g. for a pairing request).
    pub fn attention(&self) {
        crate::platform::show_window(self);
    }

    pub fn quit(&self) -> ! {
        self.stop_engine();
        std::process::exit(0);
    }
}

// ---------------------------------------------------------------------------
// Logging into a ring buffer shown in the GUI.
// ---------------------------------------------------------------------------

static SINK: OnceLock<Arc<AppState>> = OnceLock::new();
static LOGFILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

/// edgehop.log next to the received-files cache (truncated when it grows past 5 MB).
pub fn log_path() -> std::path::PathBuf {
    crate::files::received_root().with_file_name("edgehop.log")
}

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info && m.target().starts_with("edgehop")
    }
    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!(
            "{:02}:{:02}:{:02} {:5} {}",
            (secs / 3600) % 24,
            (secs / 60) % 60,
            secs % 60,
            r.level(),
            r.args()
        );
        eprintln!("{line}");
        if let Some(f) = LOGFILE.get() {
            use std::io::Write;
            let _ = writeln!(lk(f), "{line}");
        }
        if let Some(app) = SINK.get() {
            let mut logs = lk(&app.logs);
            logs.push_back(line);
            while logs.len() > 400 {
                logs.pop_front();
            }
        }
    }
    fn flush(&self) {}
}

pub fn init_logger(app: Arc<AppState>) {
    let _ = SINK.set(app);
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let big = std::fs::metadata(&path).map(|m| m.len() > 5 * 1024 * 1024).unwrap_or(false);
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(!big).write(true).truncate(big).open(&path) {
        let _ = LOGFILE.set(Mutex::new(f));
    }
    let _ = log::set_boxed_logger(Box::new(Logger));
    log::set_max_level(log::LevelFilter::Info);
}
