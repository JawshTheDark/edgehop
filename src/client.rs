//! Client: connects to the server and replays keyboard/mouse events locally.
use crate::files;
use crate::recovery::now_ms;
use crate::inject::{self, Injector};
use crate::net;
use crate::proto::*;
use crate::state::{lk, AppState};
use std::collections::HashSet;
use std::io;
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct ClientEngine {
    stop: Arc<AtomicBool>,
    current: Arc<Mutex<Option<TcpStream>>>,
}

/// Lets the watchdog see whether the connection loop is still making progress.
struct Liveness {
    /// `recovery::now_ms()` when the loop last handled a message.
    progress: AtomicU64,
    /// Paired and receiving pings (so silence means something is stuck).
    connected: AtomicBool,
}

impl ClientEngine {
    pub fn start(app: Arc<AppState>) -> ClientEngine {
        let stop = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(None));
        let live = Arc::new(Liveness { progress: AtomicU64::new(now_ms()), connected: AtomicBool::new(false) });
        let (st, cur, lv) = (stop.clone(), current.clone(), live.clone());
        thread::Builder::new()
            .name("client".into())
            .spawn(move || run(app, st, cur, lv))
            .expect("spawn client thread");
        let (st, cur) = (stop.clone(), current.clone());
        thread::Builder::new()
            .name("client-watchdog".into())
            .spawn(move || watchdog(st, cur, live))
            .expect("spawn watchdog");
        ClientEngine { stop, current }
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = lk(&self.current).take() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

/// The server pings every second. If the connection loop stops handling
/// messages, first clear stuck helper processes and force a reconnect; if it
/// is still stuck after that, restart kvmlink.
fn watchdog(stop: Arc<AtomicBool>, current: Arc<Mutex<Option<TcpStream>>>, live: Arc<Liveness>) {
    let mut nudged = false;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
        let stuck_ms = now_ms().saturating_sub(live.progress.load(Ordering::Relaxed));
        if !live.connected.load(Ordering::Relaxed) || stuck_ms < 8000 {
            nudged = false;
            continue;
        }
        if !nudged {
            log::warn!("connection loop stuck for {} s; clearing stuck helpers and reconnecting", stuck_ms / 1000);
            crate::recovery::kill_child_helpers();
            if let Some(s) = lk(&current).as_ref() {
                let _ = s.shutdown(Shutdown::Both);
            }
            nudged = true;
        } else if stuck_ms > 25_000 {
            crate::recovery::restart_self("client connection loop stuck");
        }
    }
}

fn sleep_unless_stopped(stop: &AtomicBool, d: Duration) {
    let mut left = d;
    while !stop.load(Ordering::Relaxed) && !left.is_zero() {
        let step = left.min(Duration::from_millis(100));
        thread::sleep(step);
        left -= step;
    }
}

fn run(app: Arc<AppState>, stop: Arc<AtomicBool>, current: Arc<Mutex<Option<TcpStream>>>, live: Arc<Liveness>) {
    // Desktop integration: KDE/KWin on Linux, Explorer + cursor on Windows.
    // Away/back (D-Bus calls that can be slow) run in order on their own
    // thread so they never hold up input or the connection.
    let hooks = crate::platform::client_hooks();
    let (away_tx, away_rx) = std::sync::mpsc::channel::<Option<(bool, Option<u8>)>>();
    if let Some(h) = hooks.clone() {
        thread::spawn(move || {
            for cmd in away_rx {
                crate::recovery::guard("desktop hooks", || match cmd {
                    Some((hide, dim)) => h.away(hide, dim),
                    None => h.back(),
                });
            }
        });
    }
    let mut injector = match inject::create() {
        Ok(i) => i,
        Err(e) => {
            log::error!("{e}");
            app.set_status(e, false);
            return;
        }
    };
    // Keys and buttons we pressed on this computer; released whenever a
    // connection ends, however it ended, so nothing stays stuck down.
    let mut held = Held::default();
    while !stop.load(Ordering::Relaxed) {
        let result = session(&app, &stop, &current, injector.as_mut(), hooks.as_deref(), &away_tx, &mut held, &live);
        live.connected.store(false, Ordering::Relaxed);
        for k in held.keys.drain() {
            injector.key(k, false);
        }
        for b in held.buttons.drain() {
            injector.button(b, false);
        }
        let wait = match result {
            Ok(wait) => wait,
            Err(e) => {
                if !stop.load(Ordering::Relaxed) {
                    log::info!("connection: {e}");
                    let s = lk(&app.status).clone();
                    if !s.starts_with("Server identity") && !s.starts_with("Found") && !s.starts_with("Searching") {
                        app.set_status(format!("Disconnected ({e}) — retrying"), false);
                    }
                }
                Duration::from_secs(2)
            }
        };
        *lk(&app.pairing_code) = None;
        *lk(&current) = None;
        let _ = away_tx.send(None);
        sleep_unless_stopped(&stop, wait);
    }
}

#[derive(Default)]
struct Held {
    keys: HashSet<u16>,
    buttons: HashSet<u8>,
}

fn resolve(addr: &str) -> io::Result<SocketAddr> {
    let a = addr.trim();
    let with_port = if a.parse::<SocketAddr>().is_ok() || (a.contains(':') && !a.contains("::") && a.rsplit(':').next().map_or(false, |p| p.parse::<u16>().is_ok())) {
        a.to_string()
    } else if a.contains(':') && !a.starts_with('[') {
        format!("[{a}]:{}", net::DEFAULT_PORT)
    } else {
        format!("{a}:{}", net::DEFAULT_PORT)
    };
    with_port
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::other(format!("can't resolve {a}")))
}

/// Returns how long to wait before reconnecting.
fn session(
    app: &Arc<AppState>,
    stop: &AtomicBool,
    current: &Mutex<Option<TcpStream>>,
    inj: &mut dyn Injector,
    hooks: Option<&dyn crate::platform::DesktopHooks>,
    away_tx: &std::sync::mpsc::Sender<Option<(bool, Option<u8>)>>,
    held: &mut Held,
    live: &Liveness,
) -> io::Result<Duration> {
    let cfg = app.cfg();

    // 1. Find the server.
    let addr = if !cfg.server_address.trim().is_empty() {
        app.set_status(format!("Connecting to {}…", cfg.server_address.trim()), false);
        resolve(&cfg.server_address)?
    } else {
        if lk(&app.discovered).is_empty() {
            app.set_status("Searching for a server on your network…", false);
        }
        let found = net::discover(Duration::from_millis(1500));
        *lk(&app.discovered) = found.clone();
        app.repaint();
        let pick = match &cfg.trusted_server {
            Some(t) => found.iter().find(|d| d.info.key == t.key),
            None if found.len() == 1 => found.first(),
            None => None,
        };
        match pick {
            Some(d) => d.addr,
            None => {
                let msg = match (&cfg.trusted_server, found.len()) {
                    (Some(t), _) => format!("Searching for {}… (or enter its address)", t.name),
                    (None, 0) => "Searching for a server on your network…".to_string(),
                    (None, n) => format!("Found {n} servers — choose one below"),
                };
                app.set_status(msg, false);
                return Ok(Duration::from_secs(2));
            }
        }
    };

    // 2. Connect + authenticate.
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(4))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    *lk(current) = Some(stream.try_clone()?);
    if stop.load(Ordering::Relaxed) {
        return Ok(Duration::ZERO);
    }
    let net::Handshaked { mut tx, mut rx, remote_key, code } =
        net::connect_handshake(stream, &cfg.private_key_bytes())?;

    if let Some(t) = &cfg.trusted_server {
        if t.key != remote_key {
            *lk(&app.key_mismatch) = Some(net::fingerprint(&remote_key));
            app.set_status(
                format!("Server identity changed at {addr}! Click \"Forget server\" only if you reinstalled it."),
                false,
            );
            return Ok(Duration::from_secs(10));
        }
    }
    *lk(&app.key_mismatch) = None;

    let (w, h) = cfg.screen_override.unwrap_or_else(|| inj.screen_size());
    tx.send(&ToServer::Hello {
        name: cfg.name.clone(),
        width: w,
        height: h,
        os: std::env::consts::OS.to_string(),
        version: PROTOCOL_VERSION,
    })?;

    // 3. Pairing (if new) then the event loop. The sender is shared with
    // background threads that answer drags and stream files.
    let tx = Arc::new(Mutex::new(tx));
    rx.set_read_timeout(None)?;
    let mut sink: Option<files::Sink> = None;
    let mut drop_into: Option<std::path::PathBuf> = None;
    let Held { keys, buttons } = held;
    // Shared with the background clipboard threads.
    let last_clip: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut server_name = String::from("server");

    loop {
        let msg = rx.recv::<ToClient>()?;
        live.progress.store(now_ms(), Ordering::Relaxed);
        match msg {
            ToClient::PairPending { code: c } => {
                *lk(&app.pairing_code) = Some(c.clone());
                app.set_status(format!("Waiting for approval on the server — code {c}"), false);
            }
            ToClient::PairRejected => {
                *lk(&app.pairing_code) = None;
                app.set_status("The server declined pairing", false);
                return Ok(Duration::from_secs(15));
            }
            ToClient::Welcome { server_name: n } => {
                *lk(&app.pairing_code) = None;
                server_name = n.clone();
                if app.cfg().trusted_server.as_ref().map(|t| &t.key) != Some(&remote_key) {
                    let (n2, k) = (n.clone(), remote_key.clone());
                    app.update_config(|c| c.trusted_server = Some(crate::config::Peer { name: n2, key: k }));
                    log::info!("paired with server {n} (code {code})");
                }
                rx.set_read_timeout(Some(Duration::from_secs(12)))?;
                live.connected.store(true, Ordering::Relaxed);
                app.set_status(format!("Connected to {n} ({w}×{h})"), true);
            }
            ToClient::Enter { x, y } => {
                let _ = away_tx.send(None);
                inj.move_to(x, y);
                *lk(&app.active_screen) = cfg.name.clone();
                app.repaint();
            }
            ToClient::Move { x, y } => inj.move_to(x, y),
            ToClient::Button { button, down } => {
                if down {
                    buttons.insert(button);
                } else {
                    buttons.remove(&button);
                }
                inj.button(button, down);
            }
            ToClient::Wheel { dx, dy } => inj.wheel(dx, dy),
            ToClient::Key { code, down } => {
                if down {
                    keys.insert(code);
                } else {
                    keys.remove(&code);
                }
                inj.key(code, down);
            }
            ToClient::Leave => {
                for k in keys.drain() {
                    inj.key(k, false);
                }
                for b in buttons.drain() {
                    inj.button(b, false);
                }
                *lk(&app.active_screen) = server_name.clone();
                app.repaint();
                let now = app.cfg();
                if now.hide_pointer_when_away {
                    inj.move_to(w - 1, h - 1); // park it out of the way
                }
                let _ = away_tx.send(Some((
                    now.hide_pointer_when_away,
                    now.dim_when_away.then_some(now.dim_level.clamp(5, 90)),
                )));
                if now.clipboard {
                    // Clipboard helpers can be slow (or stuck while the screen is
                    // locked): read it off the input thread.
                    let (tx2, last) = (tx.clone(), last_clip.clone());
                    thread::spawn(move || {
                        if let Some(t) = crate::clipboard::get() {
                            let fresh = lk(&last).as_deref().map(str::trim) != Some(t.trim());
                            if fresh && !t.is_empty() && !t.contains("kvmlink-probe") {
                                *lk(&last) = Some(t.clone());
                                let _ = lk(&tx2).send(&ToServer::Clipboard { text: t });
                            }
                        }
                    });
                }
            }
            ToClient::Clipboard { text } => {
                if app.cfg().clipboard {
                    *lk(&last_clip) = Some(text.clone());
                    thread::spawn(move || crate::clipboard::set(&text));
                }
            }
            ToClient::Ping => lk(&tx).send(&ToServer::Pong)?,
            ToClient::FilesGrab => {
                // A drag here was pushed off the edge. Cancel it in the file
                // manager (Esc) and copy the selection it was dragging (Ctrl+C).
                if let Some(false) = hooks.and_then(|h| h.drag_source_ok()) {
                    log::info!("drag off the edge isn't from a file manager; not carrying it");
                    lk(&tx).send(&ToServer::FilesGrabbed { files: Vec::new() })?;
                    continue;
                }
                let probe = crate::clipboard::Probe::start();
                // Esc cancels the drag; give the file manager a moment, then Ctrl+C.
                inj.key(1, true);
                inj.key(1, false);
                thread::sleep(Duration::from_millis(200));
                for (code, down) in [(29, true), (46, true), (46, false), (29, false)] {
                    inj.key(code, down);
                }
                let tx2 = tx.clone();
                thread::spawn(move || {
                    let found = probe.files();
                    probe.restore();
                    let files: Vec<String> =
                        found.unwrap_or_default().iter().map(|p| p.to_string_lossy().into_owned()).collect();
                    log::info!("drag off the edge carries {} item(s)", files.len());
                    let _ = lk(&tx2).send(&ToServer::FilesGrabbed { files });
                });
            }
            ToClient::FilesSend { paths } => {
                let (tx2, app2, me) = (tx.clone(), app.clone(), cfg.name.clone());
                let to = server_name.clone();
                thread::spawn(move || {
                    let paths: Vec<std::path::PathBuf> = paths.into_iter().map(Into::into).collect();
                    app2.set_status(format!("Sending files to {to}…"), true);
                    let r = files::stream(&me, &paths, |m| lk(&tx2).send(&ToServer::File { m }));
                    match r {
                        Ok(bytes) => app2.set_status(format!("Sent {} to {to}", files::human(bytes)), true),
                        Err(e) => {
                            let m = files::FileMsg::Abort { reason: e.to_string() };
                            let _ = lk(&tx2).send(&ToServer::File { m });
                            app2.set_status(format!("Sending files failed: {e}"), false);
                        }
                    }
                });
            }
            ToClient::File { m } => {
                if let files::FileMsg::Begin { from, total, .. } = &m {
                    app.set_status(format!("Receiving {} from {from}…", files::human(*total)), true);
                    // The drop just happened: remember which folder is under the pointer.
                    drop_into = hooks.and_then(|h| h.drop_folder());
                }
                match files::feed(&mut sink, m) {
                    Ok(Some(done)) => {
                        let what = files::describe(&done.items, done.bytes);
                        log::info!("received {what} from {}", done.from);
                        let placed = drop_into.take().map(|dir| (files::copy_into(&done.items, &dir), dir));
                        let (on_clip, msg) = match placed {
                            Some((Ok(paths), dir)) => {
                                log::info!("dropped into {}", dir.display());
                                (paths, format!("{what} dropped into {}", dir.display()))
                            }
                            Some((Err(e), dir)) => {
                                log::warn!("couldn't copy into {}: {e}", dir.display());
                                (done.items.clone(), format!("{what} is on the clipboard. Paste with Ctrl+V."))
                            }
                            None => (done.items.clone(), format!("{what} is on the clipboard. Paste with Ctrl+V.")),
                        };
                        if let Some(text) = crate::clipboard::set_files(&on_clip) {
                            *lk(&last_clip) = Some(text); // don't echo the file list back as text
                        }
                        files::notify(&format!("Files from {}", done.from), &msg);
                        app.set_status(msg, true);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("receiving files failed: {e}");
                        app.set_status(format!("Receiving files failed: {e}"), false);
                    }
                }
            }
        }
    }
}
