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

/// A file transfer in progress (shown in the header).
pub struct TransferStatus {
    pub label: String,
    pub total: u64,
    pub done: Arc<std::sync::atomic::AtomicU64>,
    pub started: std::time::Instant,
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
    pub logs: Mutex<VecDeque<LogLine>>,
    pub ctx: Mutex<Option<eframe::egui::Context>>,
    pub engine: Mutex<Option<Engine>>,
    pub transfer: Mutex<Option<TransferStatus>>,
    /// Logs sent by other computers for a diagnostics report.
    pub remote_logs: Mutex<std::collections::HashMap<String, String>>,
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
            transfer: Mutex::new(None),
            remote_logs: Mutex::new(std::collections::HashMap::new()),
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

    /// Start showing a transfer; returns the byte counter to advance.
    pub fn begin_transfer(&self, label: String, total: u64) -> Arc<std::sync::atomic::AtomicU64> {
        let done = Arc::new(std::sync::atomic::AtomicU64::new(0));
        log::info!("{label} ({})", crate::files::human(total));
        *lk(&self.transfer) =
            Some(TransferStatus { label, total, done: done.clone(), started: std::time::Instant::now() });
        self.repaint();
        done
    }

    pub fn end_transfer(&self) {
        if let Some(t) = lk(&self.transfer).take() {
            let secs = t.started.elapsed().as_secs_f64().max(0.001);
            let bytes = t.done.load(std::sync::atomic::Ordering::Relaxed);
            log::info!(
                "transfer finished: {} in {:.1} s ({}/s)",
                crate::files::human(bytes),
                secs,
                crate::files::human((bytes as f64 / secs) as u64)
            );
        }
        self.repaint();
    }

    /// Change the log level here (and, as the server, on every connected computer).
    pub fn set_log_level_everywhere(&self, level: &str) {
        set_log_level(level);
        let l = level.to_string();
        self.update_config(|c| c.log_level = l);
        if let Some(Engine::Server(s)) = lk(&self.engine).as_ref() {
            s.broadcast(crate::proto::ToClient::SetLogLevel { level: level.to_string() });
        }
    }

    /// Ask connected computers for their logs; false if this isn't the server.
    pub fn request_remote_logs(&self) -> bool {
        match lk(&self.engine).as_ref() {
            Some(Engine::Server(s)) => {
                s.broadcast(crate::proto::ToClient::GetLogs);
                true
            }
            _ => false,
        }
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
/// The open log file and how many bytes it holds (for rotation).
static LOGFILE: OnceLock<Mutex<(Option<std::fs::File>, u64)>> = OnceLock::new();
const ROTATE_AT: u64 = 8 * 1024 * 1024;
const KEEP_IN_MEMORY: usize = 3000;

/// One line in the in-app activity log.
#[derive(Clone, Debug)]
pub struct LogLine {
    pub level: log::Level,
    pub text: String,
}

/// edgehop.log next to the received-files cache; the previous one is kept as edgehop.1.log.
pub fn log_path() -> std::path::PathBuf {
    crate::files::received_root().with_file_name("edgehop.log")
}

pub fn previous_log_path() -> std::path::PathBuf {
    log_path().with_file_name("edgehop.1.log")
}

/// "error" | "warn" | "info" | "debug" | "trace" -> filter.
pub fn parse_level(name: &str) -> Option<log::LevelFilter> {
    match name.trim().to_ascii_lowercase().as_str() {
        "error" => Some(log::LevelFilter::Error),
        "warn" | "warning" => Some(log::LevelFilter::Warn),
        "info" => Some(log::LevelFilter::Info),
        "debug" => Some(log::LevelFilter::Debug),
        "trace" => Some(log::LevelFilter::Trace),
        _ => None,
    }
}

/// Change how much is logged, right away.
pub fn set_log_level(name: &str) {
    if let Some(l) = parse_level(name) {
        if log::max_level() != l {
            log::set_max_level(l);
            log::info!("log level is now {}", l.to_string().to_lowercase());
        }
    }
}

pub fn log_level_name() -> String {
    log::max_level().to_string().to_lowercase()
}

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::max_level() && m.target().starts_with("edgehop")
    }
    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let (h, m, sec, ms) = crate::platform::local_clock();
        let thread = std::thread::current();
        let module = r.target().strip_prefix("edgehop::").unwrap_or(r.target());
        let line = format!(
            "{h:02}:{m:02}:{sec:02}.{ms:03} {:5} [{}] {module}: {}",
            r.level(),
            thread.name().unwrap_or("-"),
            r.args()
        );
        eprintln!("{line}");
        if let Some(f) = LOGFILE.get() {
            use std::io::Write;
            let mut g = lk(f);
            if g.1 > ROTATE_AT {
                // Keep one previous file; start a fresh one.
                g.0 = None;
                let _ = std::fs::rename(log_path(), previous_log_path());
                g.0 = std::fs::OpenOptions::new().create(true).append(true).open(log_path()).ok();
                g.1 = 0;
            }
            if let Some(file) = g.0.as_mut() {
                if writeln!(file, "{line}").is_ok() {
                    g.1 += line.len() as u64 + 1;
                }
            }
        }
        if let Some(app) = SINK.get() {
            let mut logs = lk(&app.logs);
            logs.push_back(LogLine { level: r.level(), text: line });
            while logs.len() > KEEP_IN_MEMORY {
                logs.pop_front();
            }
        }
    }
    fn flush(&self) {}
}

/// Start logging. Level: `--trace`/`--debug` flag, else EDGEHOP_LOG, else the setting.
pub fn init_logger(app: Arc<AppState>, args: &[String]) {
    let configured = app.cfg().log_level;
    let _ = SINK.set(app);
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
    let _ = LOGFILE.set(Mutex::new((file, size)));
    let _ = log::set_boxed_logger(Box::new(Logger));
    let level = if args.iter().any(|a| a == "--trace") {
        "trace".to_string()
    } else if args.iter().any(|a| a == "--debug") {
        "debug".to_string()
    } else {
        std::env::var("EDGEHOP_LOG").unwrap_or(configured)
    };
    log::set_max_level(parse_level(&level).unwrap_or(log::LevelFilter::Info));
}
