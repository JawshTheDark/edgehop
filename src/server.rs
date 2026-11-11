// Sharing works on Windows, macOS and Linux; elsewhere this code is unused.
#![cfg_attr(not(any(windows, target_os = "macos", target_os = "linux")), allow(dead_code, unused_imports))]
//! Server: owns the physical keyboard/mouse, decides which screen is active,
//! and forwards input to the active client.
use crate::config::{Config, Peer};
use crate::layout::{Monitor, Rect, ScreenPos};
use crate::net::{self, Announce};
use crate::files;
use crate::net::{FrameReceiver, FrameSender};
use crate::transfer::{self, Manifest};
use std::path::{Path, PathBuf};
use crate::recovery::{self, now_ms};
use crate::proto::*;
use crate::state::{lk, AppState, ConnectedInfo, PairRequest};
use std::collections::{HashMap, VecDeque};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    fn opposite(self) -> Dir {
        match self {
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
        }
    }
    fn delta(self) -> (f64, f64) {
        match self {
            Dir::Left => (-1.0, 0.0),
            Dir::Right => (1.0, 0.0),
            Dir::Up => (0.0, -1.0),
            Dir::Down => (0.0, 1.0),
        }
    }
}

/// How far (server pixels) we look past an edge for the next screen, so small
/// gaps in the layout still connect.
const GAP: i32 = 96;

/// Access to the server's own desktop (abstracted so the switching logic is testable).
pub trait Desktop: Send {
    fn monitors(&self) -> Vec<Monitor>;
    /// Where the cursor is parked while a client is active.
    fn center(&self) -> (i32, i32);
    fn set_cursor(&mut self, x: i32, y: i32);
    fn cursor(&self) -> (i32, i32) {
        (0, 0)
    }
    fn set_cursor_visible(&mut self, _visible: bool) {}
    /// A file drag is pushed against this edge segment: catch it there (l, t, w, h).
    fn show_drop_strip(&mut self, _r: (i32, i32, i32, i32)) {}
    fn hide_drop_strip(&mut self) {}
    /// The drag continues on another computer: end it here without dropping anywhere.
    fn end_os_drag(&mut self) {}
    /// The mouse moved to a client (true) or came back (false). Linux grabs
    /// the input devices for itself while a client is active.
    fn captured(&mut self, _remote: bool) {}
    /// Game mode: stop (false) or resume (true) watching the mouse. Windows
    /// removes its mouse hook entirely, so games get untouched input.
    fn set_mouse_capture(&mut self, _on: bool) {}
}

pub enum ClipJob {
    ReadLocal,
    SetLocal(Clip),
}

/// Work that must not run inside the input hook.
pub enum FileJob {
    /// A drag from this computer was dropped on client `name`.
    SendToClient { name: String, paths: Vec<String> },
}

/// Large messages (clipboard images) for one client, sent between input messages.
pub struct Bulk {
    q: Mutex<VecDeque<ToClient>>,
    cv: Condvar,
    closed: AtomicBool,
}

impl Bulk {
    pub fn new() -> Arc<Bulk> {
        Arc::new(Bulk { q: Mutex::new(VecDeque::new()), cv: Condvar::new(), closed: AtomicBool::new(false) })
    }
    /// Queue without waiting for room (rare, single large messages).
    pub fn push_now(&self, m: ToClient) {
        if !self.closed.load(Ordering::Relaxed) {
            lk(&self.q).push_back(m);
        }
    }
    fn pop(&self) -> Option<ToClient> {
        let m = lk(&self.q).pop_front();
        if m.is_some() {
            self.cv.notify_all();
        }
        m
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        lk(&self.q).clear();
        self.cv.notify_all();
    }
}

#[derive(Clone, Debug)]
struct Pending {
    from: String,
    hit: Hit,
    dir: Dir,
}

/// Files being dragged across screens with the left button held.
#[derive(Clone, Debug)]
struct Carry {
    from: String,
    files: Vec<String>,
}

pub struct Slot {
    pub id: u64,
    pub tx: mpsc::Sender<ToClient>,
    pub bulk: Arc<Bulk>,
    /// When we last heard anything from this client (`recovery::now_ms`).
    pub last_seen: Arc<AtomicU64>,
    pub w: i32,
    pub h: i32,
    pub clip_gen: u64,
}

#[derive(Clone, Debug)]
enum Hit {
    Server(i32, i32),
    Client(String, f64, f64),
}

pub struct Core {
    pub me: String,
    mons: Vec<Monitor>,
    screens: Vec<ScreenPos>,
    pub clients: HashMap<String, Slot>,
    pub active: String,
    /// Virtual cursor on the active client, in client pixels.
    cx: f64,
    cy: f64,
    local_buttons: u32,
    remote_buttons: u32,
    speed: f64,
    hide_cursor: bool,
    /// Edge resistance: how far (client px) you must push past an edge to switch.
    resistance: f64,
    push: f64,
    push_dir: Option<Dir>,
    push_at: Instant,
    /// Until the hook reports a position near the parked spot, deltas are stale.
    parked: bool,
    clipboard: bool,
    clip: Option<Clip>,
    clip_gen: u64,
    server_clip_gen: u64,
    pub clip_jobs: Option<mpsc::Sender<ClipJob>>,
    pub file_jobs: Option<mpsc::Sender<FileJob>>,
    left_local: bool,
    left_remote: bool,
    edge_pushes: u32,
    grab_pending: Option<Pending>,
    grab_tried: bool,
    carry: Option<Carry>,
    /// Where a drag from a client was released on this computer.
    pub drop_point: Option<(i32, i32)>,
    desk: Box<dyn Desktop>,
    app: Option<Arc<AppState>>,
    /// macOS server: Command is sent as Ctrl (and Ctrl as Command).
    command_is_ctrl: bool,
    /// Just came back through this edge, at this spot, at this time.
    reentry: Option<(Dir, i32, i32, Instant)>,
    /// When the pointer last switched screens (to notice it bouncing).
    switches: VecDeque<Instant>,
    bounce_reported: Option<Instant>,
    /// Recent pointer reports that made no sense while away (to notice a storm).
    stale: VecDeque<Instant>,
    /// When each computer's connection was last replaced by a new one from it.
    replacements: HashMap<String, VecDeque<Instant>>,
    stale_reported: Option<Instant>,
    /// Why game mode is wanted (shown and logged when it takes effect).
    game_why: String,
    /// Scroll Lock is held (its auto-repeat must not toggle again and again).
    scroll_held: bool,
    /// Keys that went down on this computer and haven't come up yet: their
    /// release belongs here even if the mouse has moved on meanwhile.
    local_keys: Vec<u16>,
    /// A client this computer asked to send dropped files (it may now offer them).
    pub drop_from: Option<String>,
    /// Game mode turned on (Some(true)) or off (Some(false)) by hand; None:
    /// automatic. Off while a game is in front lasts until that game closes.
    game_manual: Option<bool>,
    /// Turn game mode on by itself while a fullscreen app is in front.
    game_auto_enabled: bool,
    /// The fullscreen app in front, if any (Windows).
    game_auto: Option<String>,
    /// Game mode is on: the mouse stays on this computer.
    game_on: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
const VK_SCROLL: u32 = 0x91;
/// After coming back through an edge, it stays shut until the pointer is
/// this far from it, or for this long.
const REENTRY_PX: i32 = 6;
const REENTRY_TIME: Duration = Duration::from_millis(500);
/// This many nonsense pointer reports within 2 s means parking isn't working.
const STALE_STORM: usize = 20;
/// This many switches within the window looks like the pointer bouncing.
const BOUNCE_SWITCHES: usize = 6;
const BOUNCE_WINDOW: Duration = Duration::from_secs(3);
/// Scroll Lock (F14 on a Mac keyboard): always returns the mouse home.
const KEY_SCROLLLOCK: u16 = 70;
/// A client silent for this long is treated as unresponsive (pinged every second).
const UNRESPONSIVE_MS: u64 = 3000;

impl Core {
    pub fn new(cfg: &Config, desk: Box<dyn Desktop>, app: Option<Arc<AppState>>) -> Core {
        let mut c = Core {
            me: cfg.name.clone(),
            mons: desk.monitors(),
            screens: Vec::new(),
            clients: HashMap::new(),
            active: cfg.name.clone(),
            cx: 0.0,
            cy: 0.0,
            local_buttons: 0,
            remote_buttons: 0,
            speed: 1.0,
            hide_cursor: true,
            resistance: 0.0,
            push: 0.0,
            push_dir: None,
            push_at: Instant::now(),
            parked: true,
            clipboard: true,
            clip: None,
            clip_gen: 0,
            server_clip_gen: 0,
            clip_jobs: None,
            file_jobs: None,
            left_local: false,
            left_remote: false,
            edge_pushes: 0,
            grab_pending: None,
            grab_tried: false,
            carry: None,
            drop_point: None,
            desk,
            app,
            command_is_ctrl: false,
            reentry: None,
            switches: VecDeque::new(),
            bounce_reported: None,
            stale: VecDeque::new(),
            stale_reported: None,
            replacements: HashMap::new(),
            game_manual: None,
            game_auto_enabled: true,
            game_auto: None,
            game_on: false,
            game_why: String::new(),
            scroll_held: false,
            local_keys: Vec::new(),
            drop_from: None,
        };
        c.apply_config(cfg);
        c
    }

    pub fn apply_config(&mut self, cfg: &Config) {
        self.screens = cfg.screens.clone();
        self.speed = cfg.mouse_speed.clamp(0.2, 5.0) as f64;
        self.hide_cursor = cfg.hide_cursor_when_away;
        self.resistance = cfg.edge_resistance.min(1000) as f64;
        self.clipboard = cfg.clipboard;
        self.command_is_ctrl = cfg!(target_os = "macos") && cfg.ctrl_is_command;
        if self.game_auto_enabled != cfg.game_mode_auto {
            self.game_auto_enabled = cfg.game_mode_auto;
            self.update_game("the automatic setting changed");
        }
    }

    // ---- game mode ----

    fn game_wanted(&self) -> bool {
        match self.game_manual {
            Some(on) => on,
            None => self.game_auto_enabled && self.game_auto.is_some(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn game_mode_on(&self) -> bool {
        self.game_on
    }

    fn update_game(&mut self, why: &str) {
        let on = self.game_wanted();
        if !why.is_empty() {
            self.game_why = why.to_string();
        }
        if on == self.game_on {
            return;
        }
        if on && self.is_remote() {
            // The mouse is on another computer: turning it on now would take
            // the mouse hook away from under it (the pointer over there would
            // freeze while the mouse moved this computer's hidden cursor).
            // It takes effect when the mouse comes back.
            log::info!("game mode will turn on when the mouse is back on {}: {why}", self.me);
            return;
        }
        let why = if why.is_empty() { self.game_why.clone() } else { why.to_string() };
        let why = why.as_str();
        self.game_on = on;
        log::info!("game mode {}: {why}", if on { "on" } else { "off" });
        crate::flight::note("game mode", format!("{} ({why})", if on { "on" } else { "off" }));
        self.desk.set_mouse_capture(!on);
        if let Some(app) = &self.app {
            *lk(&app.game_mode) = on.then(|| why.to_string());
            app.repaint();
        }
    }

    /// Turn game mode on or off by hand (Settings, the tray, Scroll Lock).
    pub fn set_game_mode(&mut self, on: bool, why: &str) {
        // "Off" only needs remembering while a game would turn it back on.
        self.game_manual = if on || self.game_auto.is_some() { Some(on) } else { None };
        if on && self.is_remote() {
            self.go_home();
        }
        self.update_game(why);
    }

    pub fn toggle_game_mode(&mut self, why: &str) {
        let on = !self.game_on;
        self.set_game_mode(on, why);
    }

    /// The fullscreen app in front changed (None: there isn't one). Windows
    /// only, for now.
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub fn set_fullscreen_app(&mut self, app: Option<String>) {
        if app == self.game_auto {
            return;
        }
        let closed = self.game_auto.take();
        self.game_auto = app;
        let why = match (&self.game_auto, closed) {
            (Some(name), _) => format!("{name} is fullscreen"),
            (None, Some(name)) => {
                // Turned off for that game: back to automatic for the next one.
                if self.game_manual == Some(false) {
                    self.game_manual = None;
                }
                format!("{name} isn't fullscreen any more")
            }
            (None, None) => String::new(),
        };
        self.update_game(&why);
    }

    pub fn is_remote(&self) -> bool {
        self.active != self.me
    }

    /// Updated from a background thread: never query the OS inside the input hook.
    pub fn set_monitors(&mut self, mons: Vec<Monitor>) {
        if !mons.is_empty() {
            self.mons = mons;
        }
    }

    /// Placement of a connected client: (rect in server pixels, scale).
    fn client_rect(&self, name: &str) -> Option<(Rect, f64)> {
        self.clients.get(name)?;
        let p = self.screens.iter().find(|p| p.name == name)?;
        Some((p.rect(), p.scale))
    }

    fn client_size(&self, name: &str) -> (f64, f64) {
        self.clients
            .get(name)
            .map(|s| (s.w.max(1) as f64, s.h.max(1) as f64))
            .unwrap_or((1920.0, 1080.0))
    }

    /// Walk outward from (ox, oy) and report the first screen we land on.
    fn cast(&self, ox: f64, oy: f64, dir: Dir, from_client: Option<&str>) -> Option<Hit> {
        let (dx, dy) = dir.delta();
        for d in 1..=GAP {
            let (px, py) = (ox + dx * d as f64, oy + dy * d as f64);
            if self.mons.iter().any(|m| m.rect.contains(px, py)) {
                // From the server itself another monitor continues the desktop: no switch.
                return from_client.map(|_| Hit::Server(px.floor() as i32, py.floor() as i32));
            }
            for p in &self.screens {
                if Some(p.name.as_str()) != from_client
                    && self.responsive(&p.name)
                    && p.rect().contains(px, py)
                {
                    return Some(Hit::Client(p.name.clone(), px, py));
                }
            }
        }
        None
    }

    /// Send to a client; false if it isn't connected.
    pub fn send_to(&self, name: &str, msg: ToClient) -> bool {
        self.clients.get(name).map_or(false, |s| s.tx.send(msg).is_ok())
    }

    fn send(&self, name: &str, msg: ToClient) {
        if let Some(s) = self.clients.get(name) {
            let _ = s.tx.send(msg);
        }
    }

    fn job(&self, j: ClipJob) {
        if let Some(tx) = &self.clip_jobs {
            let _ = tx.send(j);
        }
    }

    fn push_clip(&mut self, name: &str) {
        if !self.clipboard {
            return;
        }
        let gen = self.clip_gen;
        if let (Some(clip), Some(slot)) = (self.clip.clone(), self.clients.get_mut(name)) {
            if slot.clip_gen != gen {
                slot.clip_gen = gen;
                log::debug!("clipboard -> {name}: {}", clip.describe());
                match clip {
                    Clip::Text { text } => {
                        let _ = slot.tx.send(ToClient::Clipboard { text });
                    }
                    // Images can be megabytes: send them in the gaps between input.
                    Clip::Image { png } => slot.bulk.push_now(ToClient::ClipboardImage { png }),
                    // Only the file list; the client pulls the contents itself.
                    Clip::Files { manifest } => {
                        if manifest.from != name {
                            let _ = slot.tx.send(ToClient::ClipboardFiles { manifest });
                        }
                    }
                }
            }
        }
    }

    fn file_job(&self, j: FileJob) {
        if let Some(tx) = &self.file_jobs {
            let _ = tx.send(j);
        }
    }

    /// A client screen just past the edge of the monitor under (x, y), if any.
    ///
    /// The low-level hook reports where the mouse *would* go, before Windows
    /// pins it to the screen, so pushing against an edge gives points just
    /// outside every monitor. Those are snapped back onto the nearest monitor.
    fn edge_hit(&self, x: i32, y: i32) -> Option<(Hit, Dir, Rect)> {
        let (px, py) = (x as f64, y as f64);
        let gap = |m: &Rect| (m.l - px).max(px - (m.r - 1.0)).max(0.0) + (m.t - py).max(py - (m.b - 1.0)).max(0.0);
        let m = self
            .mons
            .iter()
            .map(|m| m.rect)
            .min_by(|a, b| gap(a).total_cmp(&gap(b)))
            .filter(|m| gap(m) <= GAP as f64)?;
        let (cx, cy) = (px.clamp(m.l, m.r - 1.0), py.clamp(m.t, m.b - 1.0));
        let (fx, fy) = (cx + 0.5, cy + 0.5);
        let edges = [
            (Dir::Left, x as f64 <= m.l),
            (Dir::Right, x as f64 >= m.r - 1.0),
            (Dir::Up, y as f64 <= m.t),
            (Dir::Down, y as f64 >= m.b - 1.0),
        ];
        for (dir, at_edge) in edges {
            if at_edge {
                if let Some(hit @ Hit::Client(..)) = self.cast(fx, fy, dir, None) {
                    return Some((hit, dir, m));
                }
            }
        }
        None
    }

    /// The part of monitor `m`'s edge that leads to the client in `hit`, as a
    /// thin strip (l, t, w, h) under the pinned cursor.
    fn strip_for(&self, hit: &Hit, dir: Dir, m: &Rect) -> Option<(i32, i32, i32, i32)> {
        let Hit::Client(name, ..) = hit else { return None };
        let r = self.screens.iter().find(|p| &p.name == name)?.rect();
        const T: f64 = 3.0;
        let (l, t, rr, b) = match dir {
            Dir::Down => (m.l.max(r.l), m.b - T, m.r.min(r.r), m.b),
            Dir::Up => (m.l.max(r.l), m.t, m.r.min(r.r), m.t + T),
            Dir::Right => (m.r - T, m.t.max(r.t), m.r, m.b.min(r.b)),
            Dir::Left => (m.l, m.t.max(r.t), m.l + T, m.b.min(r.b)),
        };
        (rr > l && b > t).then(|| (l as i32, t as i32, (rr - l) as i32, (b - t) as i32))
    }

    fn cross(&mut self, hit: Hit, dir: Dir) {
        match hit {
            Hit::Server(sx, sy) => {
                self.enter_server(sx, sy);
                // We land on the very edge we came through: don't go straight
                // back out until the pointer has moved off it.
                self.reentry = Some((dir.opposite(), sx, sy, Instant::now()));
            }
            Hit::Client(name, px, py) => self.enter_client(name, dir, px, py),
        }
    }

    /// Just came back through an edge: a touch against that same edge (a
    /// trackpad wobbling as you lift your finger, a late or bogus report) must
    /// not send the pointer straight back out.
    /// Record a switch; if the pointer keeps flipping between screens, say so
    /// loudly and write out the input flight recorder.
    fn note_switch(&mut self, to: &str, what: String) {
        crate::flight::note("switch", what);
        let now = Instant::now();
        self.switches.push_back(now);
        while self.switches.front().is_some_and(|t| now.duration_since(*t) > BOUNCE_WINDOW) {
            self.switches.pop_front();
        }
        let recently = self.bounce_reported.is_some_and(|t| now.duration_since(t) < Duration::from_secs(60));
        if self.switches.len() >= BOUNCE_SWITCHES && !recently {
            self.bounce_reported = Some(now);
            let mut layout = String::new();
            for m in &self.mons {
                layout.push_str(&format!(
                    "\n  monitor {:?} {}x{} at ({}, {}){}",
                    m.label,
                    m.rect.w(),
                    m.rect.h(),
                    m.rect.l,
                    m.rect.t,
                    if m.primary { " main" } else { "" }
                ));
            }
            for p in &self.screens {
                let r = p.rect();
                layout.push_str(&format!(
                    "\n  screen {} {}x{} placed at ({}, {}) to ({}, {}), scale {:.3}{}",
                    p.name,
                    p.w,
                    p.h,
                    r.l,
                    r.t,
                    r.r,
                    r.b,
                    p.scale,
                    if self.clients.contains_key(&p.name) { "" } else { " (offline)" }
                ));
            }
            log::warn!(
                "the pointer switched screens {} times in {} s (now to {to}): it may be bouncing. Layout:{layout}",
                self.switches.len(),
                BOUNCE_WINDOW.as_secs()
            );
            // Off the input path: writing a couple of thousand lines takes a moment.
            let _ = thread::Builder::new()
                .name("flight-dump".into())
                .spawn(|| crate::flight::log_dump("the pointer is bouncing between screens"));
        }
    }

    /// Called for every pointer position on this computer.
    fn track_reentry(&mut self, x: i32, y: i32) {
        let Some((edge, ex, ey, at)) = self.reentry else { return };
        let moved_off = match edge {
            Dir::Left | Dir::Right => (x - ex).abs() >= REENTRY_PX,
            Dir::Up | Dir::Down => (y - ey).abs() >= REENTRY_PX,
        };
        if moved_off || at.elapsed() > REENTRY_TIME {
            self.reentry = None;
        }
    }

    /// We crossed while the physical left button is still held down.
    fn hold_left_here(&mut self) {
        if self.is_remote() {
            self.remote_buttons = LEFT_ONLY;
            self.left_remote = true;
        } else {
            self.local_buttons = LEFT_ONLY;
            self.left_local = true;
        }
    }

    fn publish(&self) {
        if let Some(app) = &self.app {
            *lk(&app.active_screen) = self.active.clone();
            app.repaint();
        }
    }

    // ---- input from the physical devices; return true to swallow the event ----

    pub fn on_mouse_move(&mut self, x: i32, y: i32) -> bool {
        if !self.is_remote() {
            if self.game_on {
                return false; // game mode: the mouse stays here
            }
            self.track_reentry(x, y);
            let hit = self.edge_hit(x, y);
            if self.local_buttons > 0 {
                // A left-drag pushed against an edge with a screen beyond: maybe files.
                let can_grab = self.left_local
                    && self.local_buttons == LEFT_ONLY
                    && self.carry.is_none()
                    && self.grab_pending.is_none()
                    && !self.grab_tried
                    && self.file_jobs.is_some();
                if can_grab {
                    match hit {
                        Some((hit, dir, m)) => {
                            self.edge_pushes += 1;
                            if self.edge_pushes >= 2 {
                                let strip = self.strip_for(&hit, dir, &m);
                                self.grab_pending = Some(Pending { from: self.me.clone(), hit, dir });
                                log::debug!("drag pushed against the {dir:?} edge; drop catcher at {strip:?}");
                                if let Some(r) = strip {
                                    self.desk.show_drop_strip(r);
                                }
                            }
                        }
                        None => self.edge_pushes = 0,
                    }
                }
                return false; // otherwise never switch mid-drag
            }
            if let Some((Hit::Client(name, px, py), dir, _)) = hit {
                if self.reentry.is_some_and(|(edge, ..)| edge == dir) {
                    log::debug!("not leaving through the {dir:?} edge at ({x}, {y}): just came back through it");
                    crate::flight::note("held", format!("{dir:?} edge at ({x}, {y}): just came back through it"));
                    return false;
                }
                self.enter_client(name, dir, px, py);
                return true;
            }
            return false;
        }

        let (cx0, cy0) = self.desk.center();
        let (dx, dy) = (x - cx0, y - cy0);
        if dx == 0 && dy == 0 {
            self.parked = true;
            return true;
        }
        // A real mouse never moves hundreds of pixels in one report. A position
        // far from the parked spot is stale (from before parking): re-park, skip.
        let limit = if self.parked { 600 } else { 200 };
        if dx.abs() > limit || dy.abs() > limit {
            let now = Instant::now();
            self.stale.push_back(now);
            while self.stale.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(2)) {
                self.stale.pop_front();
            }
            if self.stale.len() <= 3 {
                log::info!("ignoring stale mouse position ({x}, {y}); parked at ({cx0}, {cy0})");
            } else {
                log::debug!("ignoring stale mouse position ({x}, {y}); parked at ({cx0}, {cy0})");
            }
            if self.stale.len() >= STALE_STORM && self.stale_reported.map_or(true, |t| now.duration_since(t) > Duration::from_secs(60)) {
                self.stale_reported = Some(now);
                log::error!(
                    "{} pointer reports in 2 s were far from where the pointer was parked at ({cx0}, {cy0}), \
                     latest ({x}, {y}): this computer isn't keeping the pointer parked, so movement on {} is being dropped",
                    self.stale.len(),
                    self.active
                );
                let _ = thread::Builder::new()
                    .name("flight-dump".into())
                    .spawn(|| crate::flight::log_dump("pointer reports keep making no sense"));
            }
            crate::flight::rec("stale", "x y dx dy limit", &[x as i64, y as i64, dx as i64, dy as i64, limit as i64]);
            self.desk.set_cursor(cx0, cy0);
            return true;
        }
        self.parked = true;
        self.cx += dx as f64 * self.speed;
        self.cy += dy as f64 * self.speed;
        let active = self.active.clone();
        let (w, h) = self.client_size(&active);
        let over = [
            (Dir::Left, -self.cx),
            (Dir::Right, self.cx - (w - 1.0)),
            (Dir::Up, -self.cy),
            (Dir::Down, self.cy - (h - 1.0)),
        ];
        let out = over
            .iter()
            .filter(|(_, o)| *o > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .copied();
        // Edge resistance: the pointer stops at the edge, and only keeps-pushing
        // past it (without pausing or backing off) switches screens.
        match out {
            Some((dir, amount)) => {
                if self.push_dir != Some(dir) || self.push_at.elapsed() > Duration::from_millis(600) {
                    self.push = 0.0;
                }
                self.push += amount;
                self.push_dir = Some(dir);
                self.push_at = Instant::now();
            }
            None => {
                self.push = 0.0;
                self.push_dir = None;
            }
        }
        let (ax, ay) = (self.cx.clamp(0.0, w - 1.0), self.cy.clamp(0.0, h - 1.0));
        let pushed_enough = self.push >= self.resistance;
        if let Some((dir, _)) = out {
            log::trace!(
                "pushing {dir:?} at ({:.0}, {:.0}) on {active}: {:.0}/{:.0} px{}",
                ax,
                ay,
                self.push,
                self.resistance,
                if self.remote_buttons > 0 { " (button held)" } else { "" }
            );
        }
        if let (Some((dir, _)), true) = (out, pushed_enough) {
            if let Some((r, scale)) = self.client_rect(&active) {
                // The same spot in server pixels, on the edge we're leaving through.
                let gx = r.l + (ax + 0.5) * scale;
                let gy = r.t + (ay + 0.5) * scale;
                let (ox, oy) = match dir {
                    Dir::Left => (r.l + 0.5, gy),
                    Dir::Right => (r.r - 0.5, gy),
                    Dir::Up => (gx, r.t + 0.5),
                    Dir::Down => (gx, r.b - 0.5),
                };
                let carrying = self.carry.is_some() && self.left_remote && self.remote_buttons == LEFT_ONLY;
                match self.cast(ox, oy, dir, Some(&active)) {
                    Some(hit) if self.remote_buttons == 0 => {
                        log::info!(
                            "leaving {active} through its {dir:?} edge at ({ax:.0}, {ay:.0}): pushed {:.0} of {:.0} px, last move ({dx}, {dy})",
                            self.push,
                            self.resistance
                        );
                        self.cross(hit, dir);
                        return true;
                    }
                    Some(hit) if carrying => {
                        // Carrying files on to another screen.
                        let c = self.carry.take();
                        self.cross(hit, dir);
                        self.hold_left_here();
                        self.carry = c.filter(|c| c.from != self.active);
                        return true;
                    }
                    Some(hit)
                        if self.left_remote
                            && self.remote_buttons == LEFT_ONLY
                            && self.grab_pending.is_none()
                            && !self.grab_tried =>
                    {
                        // A drag on the client pushed off its edge: ask what it carries.
                        self.grab_pending = Some(Pending { from: active.clone(), hit, dir });
                        self.send(&active, ToClient::FilesGrab);
                    }
                    _ => {}
                }
            }
        }
        self.cx = ax;
        self.cy = ay;
        crate::flight::rec(
            "away",
            "x y dx dy push",
            &[ax as i64, ay as i64, dx as i64, dy as i64, self.push as i64],
        );
        self.send(&active, ToClient::Move { x: ax as i32, y: ay as i32 });
        // Keep the real cursor parked so the next delta is measured from the center.
        self.desk.set_cursor(cx0, cy0);
        true
    }

    pub fn on_button(&mut self, button: u8, down: bool) -> bool {
        let remote = self.is_remote();
        // Which buttons are down, one bit each: a release that never arrives
        // (a click into an administrator's window, say) is healed by the next
        // click of that button, where a count would stay off by one forever
        // and the pointer could never leave this screen again.
        let held = if remote { &mut self.remote_buttons } else { &mut self.local_buttons };
        let bit = 1u32 << (button as u32 & 31);
        *held = if down { *held | bit } else { *held & !bit };
        if button == BTN_LEFT {
            if remote {
                self.left_remote = down;
            } else {
                self.left_local = down;
            }
            if !down {
                if !remote {
                    self.desk.hide_drop_strip();
                }
                self.edge_pushes = 0;
                self.grab_pending = None;
                self.grab_tried = false;
                if let Some(c) = self.carry.take() {
                    self.drop_carry(c);
                    // This release ends our drag; on a client it must not reach an app.
                    return remote;
                }
            }
        }
        if remote {
            self.send(&self.active, ToClient::Button { button, down });
            true
        } else {
            false
        }
    }

    /// A carried drag was released on the active screen.
    fn drop_carry(&mut self, c: Carry) {
        if c.from == self.active {
            return;
        }
        if c.from == self.me {
            log::info!("dropping {} item(s) on {}", c.files.len(), self.active);
            self.file_job(FileJob::SendToClient { name: self.active.clone(), paths: c.files });
        } else if !self.is_remote() {
            log::info!("dropping {} item(s) from {} here", c.files.len(), c.from);
            self.drop_point = Some(self.desk.cursor());
            self.drop_from = Some(c.from.clone());
            self.send(&c.from, ToClient::FilesSend { paths: c.files });
        } else {
            log::info!("dragging files between two other computers isn't supported yet");
        }
    }

    /// The drag pushed against the edge carries these files (from the drop target).
    pub fn local_grab_result(&mut self, files: Vec<String>) {
        let Some(p) = self.grab_pending.take() else { return };
        self.grab_tried = true;
        if files.is_empty() || !self.left_local || self.is_remote() || p.from != self.me {
            return;
        }
        if let Hit::Client(to, ..) = &p.hit {
            if !self.clients.contains_key(to) {
                return; // it disconnected while the drag was held at the edge
            }
        }
        log::info!("carrying {} item(s) across", files.len());
        self.cross(p.hit, p.dir);
        // Windows saw the press but its release now goes to the other screen:
        // end the drag here, over our own invisible window.
        self.desk.end_os_drag();
        self.local_buttons = 0;
        self.left_local = false;
        self.hold_left_here();
        self.carry = Some(Carry { from: self.me.clone(), files });
    }

    /// A client answered `FilesGrab`.
    pub fn client_grab_result(&mut self, name: &str, files: Vec<String>) {
        // Only the computer that was asked may answer.
        if self.grab_pending.as_ref().map_or(true, |p| p.from != name) {
            return;
        }
        let Some(p) = self.grab_pending.take() else { return };
        self.grab_tried = true;
        if files.is_empty() || self.active != name || !self.left_remote {
            return;
        }
        if let Hit::Client(to, ..) = &p.hit {
            if !self.clients.contains_key(to) {
                return;
            }
        }
        log::info!("carrying {} item(s) from {name}", files.len());
        self.cross(p.hit, p.dir);
        self.hold_left_here();
        self.carry = Some(Carry { from: name.to_string(), files });
    }

    pub fn on_wheel(&mut self, dx: i32, dy: i32) -> bool {
        if self.is_remote() {
            self.send(&self.active, ToClient::Wheel { dx, dy });
            true
        } else {
            false
        }
    }

    /// A key from a Windows keyboard hook.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn on_key(&mut self, vk: u32, scan: u32, extended: bool, down: bool) -> bool {
        log::trace!(
            "key vk={vk:#04x} scan={scan:#04x}{} {} -> {:?} on {}",
            if extended { " ext" } else { "" },
            if down { "down" } else { "up" },
            crate::keymap::win_to_evdev(scan, extended, vk),
            self.active
        );
        if vk == VK_SCROLL {
            return self.on_evdev_key(KEY_SCROLLLOCK, down);
        }
        match crate::keymap::win_to_evdev(scan, extended, vk) {
            Some(code) => self.on_evdev_key(code, down),
            None => self.is_remote(),
        }
    }

    /// Track keys pressed on this computer. True if this event belongs to
    /// this computer: every key while the mouse is here, and afterwards the
    /// release of a key that went down here (or this computer would see it
    /// held until pressed again: a stuck Ctrl, Shift or Alt on coming back).
    fn key_is_local(&mut self, code: u16, down: bool) -> bool {
        if !self.is_remote() {
            self.local_keys.retain(|&k| k != code);
            if down {
                self.local_keys.push(code);
            }
            return true;
        }
        if !down {
            if let Some(i) = self.local_keys.iter().position(|&k| k == code) {
                self.local_keys.swap_remove(i);
                return true;
            }
        }
        false
    }

    /// A key as a Linux (evdev) key code: from macOS and Linux capture, and from on_key.
    pub fn on_evdev_key(&mut self, code: u16, down: bool) -> bool {
        if code == KEY_SCROLLLOCK {
            // Once per press: held, the key repeats, and each repeat would
            // toggle again.
            let pressed = down && !self.scroll_held;
            self.scroll_held = down;
            if pressed && self.is_remote() {
                log::info!("Scroll Lock pressed: returning to {}", self.me);
                self.go_home();
            } else if pressed {
                // Scroll Lock (F14 on a Mac) on this computer: game mode on/off.
                self.toggle_game_mode("Scroll Lock");
            }
            return true;
        }
        let here = self.key_is_local(code, down);
        if !self.is_remote() {
            return false;
        }
        log::trace!("key {code} {} on {}{}", if down { "down" } else { "up" }, self.active, if here { " (was pressed here)" } else { "" });
        // A Mac sharing its keyboard: Command does what Ctrl does elsewhere.
        let code = if self.command_is_ctrl {
            match code {
                125 => 29,
                29 => 125,
                126 => 97,
                97 => 126,
                c => c,
            }
        } else {
            code
        };
        self.send(&self.active, ToClient::Key { code, down });
        // The release of a key pressed here goes to both: this computer
        // must see it, and the other may have seen the key repeat.
        !here
    }

    // ---- switching ----

    fn leave_current(&mut self) {
        if self.is_remote() {
            let prev = self.active.clone();
            self.send(&prev, ToClient::Leave);
        } else if self.clipboard {
            self.job(ClipJob::ReadLocal);
        }
        self.remote_buttons = 0;
        self.left_remote = false;
        self.push = 0.0;
        self.push_dir = None;
    }

    /// The system says no mouse button is down: forget any we still think is
    /// held on this computer (its release never reached us).
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub fn no_buttons_down(&mut self) {
        if !self.is_remote() && self.local_buttons != 0 {
            log::debug!("no mouse button is down any more (missed a release); buttons {:#b} cleared", self.local_buttons);
            self.local_buttons = 0;
            self.left_local = false;
            self.edge_pushes = 0;
            self.grab_pending = None;
            self.grab_tried = false;
            self.desk.hide_drop_strip();
        }
    }

    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub fn thinks_a_button_is_down(&self) -> bool {
        !self.is_remote() && self.local_buttons != 0
    }

    /// Enter client `name` at server-space point (px, py), travelling in `dir`.
    fn enter_client(&mut self, name: String, dir: Dir, px: f64, py: f64) {
        let Some((r, scale)) = self.screens.iter().find(|p| p.name == name).map(|p| (p.rect(), p.scale)) else {
            return;
        };
        let (w, h) = self.client_size(&name);
        let mut cx = ((px - r.l) / scale).clamp(0.0, w - 1.0);
        let mut cy = ((py - r.t) / scale).clamp(0.0, h - 1.0);
        match dir {
            Dir::Right => cx = 0.0,
            Dir::Left => cx = w - 1.0,
            Dir::Down => cy = 0.0,
            Dir::Up => cy = h - 1.0,
        }
        let from_server = !self.is_remote();
        log::info!(
            "switch {} -> {name} at ({}, {}): through the {dir:?} edge at ({px:.0}, {py:.0}) on the desktop",
            self.active,
            cx as i32,
            cy as i32
        );
        self.note_switch(&name, format!("{} -> {name} at ({}, {}) via {dir:?} edge", self.active, cx as i32, cy as i32));
        self.leave_current();
        if from_server {
            self.desk.captured(true);
            let (x, y) = self.desk.center();
            self.desk.set_cursor(x, y);
            self.parked = false;
            if self.hide_cursor {
                self.desk.set_cursor_visible(false);
            }
        }
        self.active = name.clone();
        self.cx = cx;
        self.cy = cy;
        self.send(&name, ToClient::Enter { x: cx as i32, y: cy as i32 });
        self.push_clip(&name);
        self.publish();
    }

    fn enter_server(&mut self, x: i32, y: i32) {
        log::info!("switch {} -> {} at ({x}, {y})", self.active, self.me);
        let me = self.me.clone();
        self.note_switch(&me, format!("{} -> {me} at ({x}, {y})", self.active));
        self.leave_current();
        self.active = self.me.clone();
        self.desk.captured(false);
        self.desk.set_cursor(x, y);
        self.desk.set_cursor_visible(true);
        self.desk.hide_drop_strip();
        self.sync_server_clipboard();
        self.publish();
        // Game mode asked for while the mouse was away starts now.
        self.update_game("");
    }

    fn sync_server_clipboard(&mut self) {
        if self.clipboard && self.clip_gen != self.server_clip_gen {
            if let Some(c) = self.clip.clone() {
                self.job(ClipJob::SetLocal(c));
            }
            self.server_clip_gen = self.clip_gen;
        }
    }

    /// Return control to the server (hotkey, disconnect, shutdown).
    pub fn go_home(&mut self) {
        if !self.is_remote() {
            return;
        }
        self.carry = None;
        let (x, y) = self.desk.center();
        self.enter_server(x, y);
    }

    // ---- clients & clipboard ----

    /// Register a client; returns a replaced older connection from the same computer.
    pub fn add_client(&mut self, name: &str, slot: Slot) -> Option<Slot> {
        let old = self.clients.insert(name.to_string(), slot);
        if old.is_some() {
            // Two copies of Edgehop on one computer take turns replacing each other.
            let now = Instant::now();
            let times = self.replacements.entry(name.to_string()).or_default();
            times.push_back(now);
            while times.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
                times.pop_front();
            }
            if times.len() == 3 {
                log::warn!(
                    "{name} connected again 3 times in a minute, each time replacing its last connection: two \
                     copies of Edgehop are probably running on {name} (an older one too?)"
                );
            }
        }
        old
    }

    /// Make sure a connected client has a place next to a monitor and that its
    /// stored size is current. Returns the placement to save if it changed.
    pub fn ensure_placed(&mut self, name: &str) -> Option<ScreenPos> {
        let (w, h) = self.clients.get(name).map(|s| (s.w, s.h))?;
        if self.mons.is_empty() {
            // Normally kept current by the watchdog, off the input path.
            self.mons = self.desk.monitors();
        }
        let updated = match self.screens.iter().find(|p| p.name == name) {
            Some(p) if p.w == w && p.h == h => return None,
            Some(p) => crate::layout::resize(p, &self.mons, w, h),
            None => {
                let others: Vec<Rect> = self.screens.iter().map(|p| p.rect()).collect();
                crate::layout::auto_place(&self.mons, &others, name, w, h)
            }
        };
        self.screens.retain(|p| p.name != name);
        self.screens.push(updated.clone());
        Some(updated)
    }

    pub fn remove_client(&mut self, name: &str, id: u64) {
        if self.clients.get(name).map(|s| s.id) == Some(id) {
            if self.carry.as_ref().map_or(false, |c| c.from == name) {
                self.carry = None;
            }
            if let Some(s) = self.clients.get(name) {
                s.bulk.close();
            }
            if self.active == name {
                self.go_home();
            }
            self.clients.remove(name);
        }
    }

    pub fn client_clipboard(&mut self, name: &str, clip: Clip) {
        log::debug!("clipboard from {name}: {}", clip.describe());
        if !self.clipboard {
            log::debug!("clipboard sharing is off; ignoring it");
            return;
        }
        if self.clip.as_ref() != Some(&clip) {
            self.clip = Some(clip);
            self.clip_gen += 1;
        }
        let gen = self.clip_gen;
        if let Some(s) = self.clients.get_mut(name) {
            s.clip_gen = gen;
        }
        if self.active == self.me {
            self.sync_server_clipboard();
        } else if self.active != name {
            let a = self.active.clone();
            self.push_clip(&a);
        }
    }

    pub fn local_clipboard(&mut self, clip: Clip) {
        if self.clip.as_ref() != Some(&clip) {
            self.clip = Some(clip);
            self.clip_gen += 1;
        }
        self.server_clip_gen = self.clip_gen;
        if self.is_remote() {
            let a = self.active.clone();
            self.push_clip(&a);
        }
    }

    /// Connected and answering within the last few seconds?
    fn responsive(&self, name: &str) -> bool {
        self.clients
            .get(name)
            .map_or(false, |s| now_ms().saturating_sub(s.last_seen.load(Ordering::Relaxed)) < UNRESPONSIVE_MS)
    }

    /// If the computer the mouse is on stops answering, take the mouse back.
    pub fn check_responsive(&mut self) {
        if self.is_remote() && !self.responsive(&self.active.clone()) {
            log::warn!("{} isn't responding; bringing the mouse back to {}", self.active, self.me);
            self.go_home();
        }
    }

    pub fn ping_all(&self) {
        for s in self.clients.values() {
            let _ = s.tx.send(ToClient::Ping);
        }
    }
}

// ---------------------------------------------------------------------------
// Engine: networking + capture threads
// ---------------------------------------------------------------------------

pub struct Shared {
    pub app: Arc<AppState>,
    pub core: Mutex<Core>,
    streams: Mutex<HashMap<u64, TcpStream>>,
    /// Transfers we're pulling from a client, waiting for its data connections.
    inbound: Mutex<HashMap<u64, mpsc::Sender<(FrameSender, FrameReceiver)>>>,
    next_id: AtomicU64,
    private_key: Vec<u8>,
    stop: Arc<AtomicBool>,
}

pub struct ServerEngine {
    shared: Arc<Shared>,
    port: u16,
    /// Input capture's handle: thread id (Windows), run loop (macOS).
    #[allow(dead_code)]
    hook_thread: usize,
}

impl ServerEngine {
    pub fn start(app: Arc<AppState>) -> Result<ServerEngine, String> {
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = app;
            Err("Sharing a keyboard and mouse isn't available on this system. Use the other role.".into())
        }
        #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
        {
            let cfg = app.cfg();
            let listener = {
                let mut tries = 0;
                loop {
                    match TcpListener::bind(("0.0.0.0", cfg.port)) {
                        Ok(l) => break l,
                        Err(_) if tries < 20 => {
                            tries += 1;
                            thread::sleep(Duration::from_millis(250));
                        }
                        Err(e) => return Err(format!("Can't listen on port {}: {e}", cfg.port)),
                    }
                }
            };
            let stop = Arc::new(AtomicBool::new(false));
            for m in crate::platform::monitors() {
                log::debug!(
                    "monitor {:?} {}x{} at ({}, {}){}",
                    m.label,
                    m.rect.w() as i32,
                    m.rect.h() as i32,
                    m.rect.l as i32,
                    m.rect.t as i32,
                    if m.primary { " (main)" } else { "" }
                );
            }
            for p in &cfg.screens {
                log::debug!("screen {} placed at ({}, {}) scale {:.3}{}", p.name, p.x, p.y, p.scale, if p.fit { " stretched" } else { "" });
            }
            let (clip_tx, clip_rx) = mpsc::channel();
            let mut core = Core::new(&cfg, capture::desktop(), Some(app.clone()));
            core.clip_jobs = Some(clip_tx);
            let (file_tx, file_rx) = mpsc::channel::<FileJob>();
            core.file_jobs = Some(file_tx);
            let shared = Arc::new(Shared {
                app: app.clone(),
                core: Mutex::new(core),
                streams: Mutex::new(HashMap::new()),
                inbound: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                private_key: cfg.private_key_bytes(),
                stop: stop.clone(),
            });

            // Files a drag carries, reported by the drop target at the screen edge.
            let (drag_tx, drag_rx) = mpsc::channel::<Vec<String>>();
            let s = shared.clone();
            thread::Builder::new().name("drag".into()).spawn(move || {
                for files in drag_rx {
                    recovery::guard("drag handler", || lk(&s.core).local_grab_result(files));
                }
            }).map_err(|e| e.to_string())?;

            // Input capture thread (low-level hooks need their own message loop).
            let (tid_tx, tid_rx) = mpsc::channel();
            let s = shared.clone();
            thread::Builder::new()
                .name("hooks".into())
                .spawn(move || capture::run(s, tid_tx, drag_tx))
                .map_err(|e| e.to_string())?;
            let hook_thread = match tid_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(0) => {
                    shared.stop.store(true, Ordering::Relaxed);
                    return Err(capture::failure());
                }
                Ok(h) => h,
                Err(_) => {
                    // Whatever was started (accept loop, a slow capture thread) must end too.
                    shared.stop.store(true, Ordering::Relaxed);
                    return Err("Input capture failed to start".into());
                }
            };

            // File worker: grabbing a dragged selection and sending files.
            let s = shared.clone();
            thread::Builder::new().name("files".into()).spawn(move || {
                for job in file_rx {
                    recovery::guard("file worker", || match job {
                        FileJob::SendToClient { name, paths } => send_files(&s, name, paths),
                    });
                }
            }).map_err(|e| e.to_string())?;

            // Clipboard worker (keeps clipboard I/O out of the hook callback).
            let s = shared.clone();
            thread::Builder::new().name("clipboard".into()).spawn(move || {
                // Clipboard change counter when we last read or wrote it.
                let mut ours: Option<u32> = None;
                for job in clip_rx {
                    recovery::guard("clipboard worker", || match job {
                        ClipJob::ReadLocal => {
                            // Unchanged since we last read or wrote it: nothing new to share.
                            #[cfg(windows)]
                            {
                                let seq = unsafe { crate::win32::GetClipboardSequenceNumber() };
                                if ours == Some(seq) {
                                    log::debug!("clipboard unchanged since last read (seq {seq})");
                                    return;
                                }
                                ours = Some(seq);
                            }
                            let clip = if let Some(paths) = crate::clipboard::get_files() {
                                if transfer::is_received(&paths) {
                                    log::debug!("clipboard holds files we received; not sending them back");
                                    None // files we just received: nothing new
                                } else {
                                    let me = lk(&s.core).me.clone();
                                    match transfer::outbox().offer(&me, &paths) {
                                        Ok(manifest) => Some(Clip::Files { manifest }),
                                        Err(e) => {
                                            log::warn!("couldn't read the copied files: {e}");
                                            None
                                        }
                                    }
                                }
                            } else {
                                match crate::clipboard::get().filter(|t| !t.is_empty()) {
                                    Some(text) => Some(Clip::Text { text }),
                                    None => crate::clipboard::get_image_png().map(|p| Clip::Image { png: files::b64(&p) }),
                                }
                            };
                            match clip {
                                Some(c) => {
                                    log::debug!("local clipboard: {}", c.describe());
                                    lk(&s.core).local_clipboard(c);
                                }
                                None => log::debug!("local clipboard: nothing to share"),
                            }
                        }
                        ClipJob::SetLocal(c) => {
                            log::debug!("updating local clipboard: {}", c.describe());
                            match c {
                                Clip::Text { text } => crate::clipboard::set(&text),
                                Clip::Image { png } => {
                                    if let Some(bytes) = files::unb64(&png) {
                                        crate::clipboard::set_image_png(&bytes);
                                        log::info!("clipboard image received ({})", files::human(bytes.len() as u64));
                                    }
                                }
                                Clip::Files { manifest } => {
                                    if manifest.from != lk(&s.core).me {
                                        receive_clipboard_files(s.clone(), manifest);
                                    }
                                }
                            }
                            #[cfg(windows)]
                            {
                                ours = Some(unsafe { crate::win32::GetClipboardSequenceNumber() });
                            }
                        }
                    });
                    let _ = &mut ours;
                }
            }).map_err(|e| e.to_string())?;

            // Accept loop.
            let s = shared.clone();
            thread::Builder::new().name("accept".into()).spawn(move || {
                for conn in listener.incoming() {
                    if s.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = conn else {
                        thread::sleep(Duration::from_millis(200)); // e.g. out of handles: don't spin
                        continue;
                    };
                    let s2 = s.clone();
                    let _ = thread::Builder::new().name("conn".into()).spawn(move || {
                        let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                        recovery::guard("connection handler", || {
                            if let Err(e) = handle_client(&s2, stream) {
                                log::info!("client {peer}: {e}");
                            }
                        });
                    });
                }
            }).map_err(|e| e.to_string())?;

            // LAN discovery.
            let announce = Announce { name: cfg.name.clone(), port: cfg.port, key: cfg.public_key.clone() };
            let st = stop.clone();
            let _ = thread::Builder::new().name("discovery".into()).spawn(move || net::run_discovery_responder(st, announce));

            // Watchdog + keepalive, every second:
            // - ping clients; take the mouse back from one that stops answering;
            // - if the switching state stays locked (a bug), restart cleanly;
            // - if Windows silently dropped our input hooks, reinstall them.
            let s = shared.clone();
            thread::Builder::new()
                .name("watchdog".into())
                .spawn(move || {
                    let mut last_rehook: Option<Instant> = None;
                    let mut last_dump: Option<Instant> = None;
                    while !s.stop.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_secs(1));
                        let mons = crate::platform::monitor_rects();
                        // Asked before taking the lock the input hook waits on.
                        #[cfg(windows)]
                        let fullscreen = crate::win32::fullscreen_app();
                        let start = Instant::now();
                        let mut core = loop {
                            match s.core.try_lock() {
                                Ok(g) => break g,
                                Err(std::sync::TryLockError::Poisoned(e)) => break e.into_inner(),
                                Err(std::sync::TryLockError::WouldBlock) if start.elapsed() < Duration::from_secs(5) => {
                                    thread::sleep(Duration::from_millis(20))
                                }
                                Err(std::sync::TryLockError::WouldBlock) => {
                                    recovery::restart_self("switching state stuck for 5 s")
                                }
                            }
                        };
                        core.set_monitors(mons);
                        core.ping_all();
                        core.check_responsive();
                        #[cfg(windows)]
                        core.set_fullscreen_app(fullscreen);
                        #[cfg(windows)]
                        if core.thinks_a_button_is_down() && !crate::win32::any_mouse_button_down() {
                            core.no_buttons_down();
                        }
                        drop(core);
                        if capture::look_lost() && last_rehook.map_or(true, |t| t.elapsed() > Duration::from_secs(10)) {
                            #[cfg(windows)]
                            log::warn!("the system dropped our input capture; reinstalling it. {}", capture::loss_report());
                            #[cfg(not(windows))]
                            log::warn!("the system dropped our input capture; reinstalling it");
                            capture::reinstall(hook_thread);
                            // The minutes before matter: write out the flight recorder
                            // (at most every 5 minutes, off this thread).
                            if last_dump.map_or(true, |t: Instant| t.elapsed() > Duration::from_secs(300)) {
                                last_dump = Some(Instant::now());
                                let _ = thread::Builder::new()
                                    .name("flight-dump".into())
                                    .spawn(|| crate::flight::log_dump("input capture was dropped"));
                            }
                            last_rehook = Some(Instant::now());
                        }
                    }
                })
                .map_err(|e| e.to_string())?;

            *lk(&app.active_screen) = cfg.name.clone();
            app.set_status(format!("Sharing on port {} — waiting for computers", cfg.port), true);
            Ok(ServerEngine { shared, port: cfg.port, hook_thread })
        }
    }

    /// Send a message to every connected client.
    pub fn broadcast(&self, msg: ToClient) {
        let core = lk(&self.shared.core);
        for s in core.clients.values() {
            let _ = s.tx.send(msg.clone());
        }
    }

    pub fn apply_config(&self, cfg: &Config) {
        lk(&self.shared.core).apply_config(cfg);
    }

    pub fn set_game_mode(&self, on: bool, why: &str) {
        lk(&self.shared.core).set_game_mode(on, why);
    }

    pub fn toggle_game_mode(&self, why: &str) {
        lk(&self.shared.core).toggle_game_mode(why);
    }

    pub fn stop(self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        lk(&self.shared.core).go_home();
        crate::platform::restore_cursor();
        #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
        capture::stop(self.hook_thread);
        let _ = TcpStream::connect_timeout(
            &([127, 0, 0, 1], self.port).into(),
            Duration::from_millis(300),
        );
        for (_, s) in lk(&self.shared.streams).drain() {
            let _ = s.shutdown(Shutdown::Both);
        }
        let mut core = lk(&self.shared.core);
        core.clients.clear();
        // Closing these channels lets the clipboard and file workers end
        // (they'd otherwise keep this engine's state alive for good).
        core.clip_jobs = None;
        core.file_jobs = None;
    }
}

/// Fail-safe for input capture: never wait long for the switching state. If
/// it is stuck (a bug), input goes straight to this computer instead of freezing.
fn try_core(s: &Shared) -> Option<std::sync::MutexGuard<'_, Core>> {
    use std::sync::TryLockError;
    let start = std::time::Instant::now();
    loop {
        match s.core.try_lock() {
            Ok(g) => return Some(g),
            Err(TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(TryLockError::WouldBlock) if start.elapsed() < Duration::from_millis(100) => {
                thread::sleep(Duration::from_millis(1))
            }
            Err(TryLockError::WouldBlock) => {
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, Ordering::Relaxed) {
                    log::warn!("input capture: switching state busy >100 ms; passing input through");
                }
                return None;
            }
        }
    }
}

/// Button state with only the left button down.
const LEFT_ONLY: u32 = 1 << BTN_LEFT;

fn sanitize_name(n: &str) -> String {
    let n: String = n.chars().filter(|c| !c.is_control()).take(40).collect();
    if n.trim().is_empty() {
        "computer".into()
    } else {
        n.trim().to_string()
    }
}

#[allow(dead_code)]
fn handle_client(s: &Arc<Shared>, stream: TcpStream) -> std::io::Result<()> {
    let addr = stream.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let raw = stream.try_clone()?;
    let net::Handshaked { mut tx, mut rx, remote_key, code } = net::accept_handshake(stream, &s.private_key)?;
    log::debug!("handshake with {addr} ok, key {}", net::fingerprint(&remote_key));
    // Nothing is known about this peer yet: its first message must be small.
    let first = rx.recv_limited::<ToServer>(16 * 1024)?;
    if let ToServer::DataHello { id } = first {
        log::debug!("{addr} opened a transfer connection for {id:#x}");
        return data_connection(s, tx, rx, &remote_key, id);
    }
    let ToServer::Hello { name, width, height, os, .. } = first else {
        return Err(std::io::Error::other("expected hello"));
    };
    log::debug!("hello from {addr}: name {name:?}, screen {width}x{height}, {os}");
    let mut name = sanitize_name(&name);
    let app = &s.app;
    let cfg = app.cfg();
    if name == cfg.name {
        name = format!("{name}-client");
    }
    // A name belongs to the computer (key) paired under it: another computer
    // calling itself the same gets a name of its own, instead of taking over
    // the first one's connection, pairing and place on the desktop.
    let taken = |n: &str| cfg.trusted_clients.iter().any(|p| p.name == n && p.key != remote_key);
    if taken(&name) {
        let base = name.clone();
        name = (2..).map(|i| format!("{base}-{i}")).find(|n| !taken(n)).unwrap_or(base);
        log::info!("another computer is already paired as {:?}; calling this one {name:?}", sanitize_name(&name));
    }

    match cfg.trusted_clients.iter().find(|p| p.key == remote_key) {
        Some(p) if p.name != name => {
            // Known machine, new hostname: follow the rename.
            let old = p.name.clone();
            let new = name.clone();
            app.update_config(|c| {
                for t in c.trusted_clients.iter_mut().filter(|t| t.key == remote_key) {
                    t.name = new.clone();
                }
                for p in c.screens.iter_mut().filter(|p| p.name == old) {
                    p.name = new.clone();
                }
            });
        }
        Some(_) => {}
        None => {
            {
                // One request per address, and only a handful at once: anyone on
                // the network can ask.
                let pending = lk(&app.pending);
                if pending.len() >= 6 || pending.iter().any(|p| p.addr == addr) {
                    log::info!("pairing request from {name} ({addr}) refused: too many waiting");
                    drop(pending);
                    let _ = tx.send(&ToClient::PairRejected);
                    return Ok(());
                }
            }
            log::info!("pairing request from {name} ({addr}), code {code}");
            let (reply_tx, reply_rx) = mpsc::channel();
            let id = s.next_id.fetch_add(1, Ordering::Relaxed);
            lk(&app.pending).push(PairRequest {
                id,
                name: name.clone(),
                key: remote_key.clone(),
                code: code.clone(),
                addr: addr.clone(),
                reply: reply_tx,
            });
            app.attention();
            tx.send(&ToClient::PairPending { code })?;
            let approved = reply_rx.recv_timeout(Duration::from_secs(180)).unwrap_or(false);
            lk(&app.pending).retain(|p| p.id != id);
            app.repaint();
            if !approved {
                log::info!("pairing with {name} rejected");
                let _ = tx.send(&ToClient::PairRejected);
                return Ok(());
            }
            let (n, k) = (name.clone(), remote_key.clone());
            app.update_config(|c| {
                c.trusted_clients.retain(|p| p.key != k);
                c.trusted_clients.push(Peer { name: n.clone(), key: k });
            });
            log::info!("paired with {name}");
        }
    }

    tx.send(&ToClient::Welcome { server_name: cfg.name.clone() })?;
    // Clients log as much as the server does (debug mode is set in one place).
    tx.send(&ToClient::SetLogLevel { level: crate::state::log_level_name() })?;

    let id = s.next_id.fetch_add(1, Ordering::Relaxed);
    let (mtx, mrx) = mpsc::channel::<ToClient>();
    lk(&s.streams).insert(id, raw.try_clone()?);
    let bulk = Bulk::new();
    let last_seen = Arc::new(AtomicU64::new(now_ms()));
    let slot = Slot { id, tx: mtx, bulk: bulk.clone(), last_seen: last_seen.clone(), w: width, h: height, clip_gen: 0 };
    let replaced = lk(&s.core).add_client(&name, slot);
    let placed = lk(&s.core).ensure_placed(&name);
    if let Some(pos) = placed {
        log::info!("{name} placed at ({}, {}) scale {:.3}", pos.x, pos.y, pos.scale);
        app.update_config(|c| {
            c.screens.retain(|p| p.name != pos.name);
            c.screens.push(pos);
        });
    }
    if let Some(old) = replaced {
        log::info!("{name} connected again; closing its previous connection");
        // Tell the old copy why, then close it once that's had a moment to go out.
        let _ = old.tx.send(ToClient::Replaced);
        if let Some(st) = lk(&s.streams).remove(&old.id) {
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(300));
                let _ = st.shutdown(Shutdown::Both);
            });
        }
    }
    {
        let mut c = lk(&app.connected);
        c.retain(|i| i.name != name);
        c.push(ConnectedInfo { name: name.clone(), addr: addr.clone(), os, width, height });
    }
    log::info!("{name} connected from {addr} ({width}x{height})");
    app.set_status(format!("{} computer(s) connected", lk(&app.connected).len()), true);

    // Writer thread: input first (coalescing mouse moves), file data in the gaps.
    let writer_stream = raw.try_clone()?;
    let writer_bulk = bulk.clone();
    thread::spawn(move || {
        let mut held: Option<ToClient> = None;
        loop {
            let next = match held.take() {
                Some(m) => Some(m),
                None => match mrx.try_recv() {
                    Ok(m) => Some(m),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => break,
                },
            };
            let mut msg = match next {
                Some(m) => m,
                None => match writer_bulk.pop() {
                    Some(chunk) => chunk,
                    None => match mrx.recv_timeout(Duration::from_millis(25)) {
                        Ok(m) => m,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    },
                },
            };
            if matches!(msg, ToClient::Move { .. }) {
                while let Ok(next) = mrx.try_recv() {
                    if matches!(next, ToClient::Move { .. }) {
                        msg = next;
                    } else {
                        held = Some(next);
                        break;
                    }
                }
            }
            if tx.send(&msg).is_err() {
                let _ = writer_stream.shutdown(Shutdown::Both);
                break;
            }
        }
        writer_bulk.close();
    });

    rx.set_read_timeout(Some(Duration::from_secs(12)))?;
    let result = loop {
        let msg = rx.recv::<ToServer>();
        if let Ok(m) = &msg {
            last_seen.store(now_ms(), Ordering::Relaxed);
            if !matches!(m, ToServer::Pong) {
                log::trace!("<- {name}: {}", crate::proto::brief(m));
            }
        }
        match msg {
            Ok(ToServer::Clipboard { text }) => lk(&s.core).client_clipboard(&name, Clip::Text { text }),
            Ok(ToServer::ClipboardImage { png }) => lk(&s.core).client_clipboard(&name, Clip::Image { png }),
            Ok(ToServer::FilesGrabbed { files }) => lk(&s.core).client_grab_result(&name, files),
            Ok(ToServer::ClipboardFiles { mut manifest }) => {
                // Who it's from is who sent it, under the name known here.
                manifest.from = name.clone();
                lk(&s.core).client_clipboard(&name, Clip::Files { manifest })
            }
            Ok(ToServer::DropOffer { mut manifest }) => {
                manifest.from = name.clone();
                // Only files this computer asked that client for (a drag dropped here).
                let asked = lk(&s.core).drop_from.take();
                if asked.as_deref() == Some(name.as_str()) {
                    receive_drop(s.clone(), manifest)
                } else {
                    log::warn!("{name} offered dropped files nobody asked for; ignoring them");
                }
            }
            Ok(ToServer::Logs { text }) => {
                log::debug!("received {}'s log ({} lines)", name, text.lines().count());
                lk(&app.remote_logs).insert(name.clone(), text);
            }
            Ok(_) => {}
            // A message this version doesn't know (a newer Edgehop on the other side).
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                log::debug!("{name} sent a message this version doesn't understand: {e}");
            }
            Err(e) => break e,
        }
    };

    lk(&s.core).remove_client(&name, id);
    lk(&s.streams).remove(&id);
    let _ = raw.shutdown(Shutdown::Both);
    {
        let still_there = lk(&s.core).clients.contains_key(&name);
        if !still_there {
            lk(&app.connected).retain(|i| i.name != name);
        }
    }
    if !s.stop.load(Ordering::Relaxed) {
        let n = lk(&app.connected).len();
        app.set_status(
            if n == 0 { "Sharing — waiting for computers".to_string() } else { format!("{n} computer(s) connected") },
            true,
        );
        log::info!("{name} disconnected: {result}");
    }
    Ok(())
}

/// A client opened a file-transfer connection for transfer `id`.
fn data_connection(
    s: &Arc<Shared>,
    mut tx: FrameSender,
    mut rx: FrameReceiver,
    remote_key: &str,
    id: u64,
) -> std::io::Result<()> {
    if !s.app.cfg().trusted_clients.iter().any(|p| p.key == remote_key) {
        return Err(std::io::Error::other("transfer connection from an unpaired computer"));
    }
    rx.set_read_timeout(Some(Duration::from_secs(60)))?;
    if let Some(offered) = transfer::outbox().get(id) {
        // We have the files: answer the client's piece requests.
        let start = Instant::now();
        let sent = transfer::serve(&mut tx, &mut rx, &offered)?;
        log::debug!(
            "transfer connection done: sent {} in {:.1} s",
            files::human(sent),
            start.elapsed().as_secs_f64()
        );
        return Ok(());
    }
    let waiting = lk(&s.inbound).get(&id).cloned();
    match waiting {
        Some(chan) => {
            let _ = chan.send((tx, rx));
            Ok(())
        }
        None => Err(std::io::Error::other("unknown transfer")),
    }
}

/// Pull files from the client that has them, over parallel connections.
fn fetch_from_client(s: &Arc<Shared>, m: &Manifest, dest: &Path) -> std::io::Result<Vec<PathBuf>> {
    let n = s.app.cfg().transfer_conns.clamp(1, 16);
    let (ctx, crx) = mpsc::channel();
    lk(&s.inbound).insert(m.id, ctx);
    let asked = lk(&s.core).send_to(&m.from, ToClient::OpenData { id: m.id, count: n });
    let mut conns = Vec::new();
    if asked {
        let deadline = Instant::now() + Duration::from_secs(5);
        while conns.len() < n as usize {
            match crx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(c) => conns.push(c),
                Err(_) => break,
            }
        }
    }
    lk(&s.inbound).remove(&m.id);
    log::debug!("{} opened {}/{n} transfer connection(s)", m.from, conns.len());
    if conns.is_empty() {
        return Err(std::io::Error::other(format!("{} didn't open a transfer connection", m.from)));
    }
    let done = s.app.begin_transfer(format!("Receiving {} from {}", m.describe(), m.from), m.total);
    let r = transfer::fetch(m, conns, dest, &done);
    s.app.end_transfer();
    r
}

/// Files copied on a client: pull them here and put them on the clipboard.
fn receive_clipboard_files(s: Arc<Shared>, m: Manifest) {
    let app = s.app.clone();
    let limit = app.cfg().clipboard_files_max_gb as u64 * (1 << 30);
    if m.total > limit {
        log::info!("{} copied on {} is over the clipboard limit; drag it instead", m.describe(), m.from);
        app.set_status(format!("{} on {} is too big for the clipboard; drag it instead", m.describe(), m.from), true);
        return;
    }
    if let Some(paths) = transfer::cached(&m) {
        crate::clipboard::set_files(&paths);
        transfer::mark_received(&paths);
        return;
    }
    if !transfer::begin_fetch(&m) {
        return;
    }
    thread::spawn(move || {
        let r = fetch_from_client(&s, &m, &transfer::new_drop_dir());
        transfer::end_fetch(&m);
        match r {
            Ok(paths) => {
                transfer::remember(&m, &paths);
                crate::clipboard::set_files(&paths);
                transfer::mark_received(&paths);
                app.set_status(format!("{} from {} is ready to paste", m.describe(), m.from), true);
            }
            Err(e) => {
                log::warn!("copying {} from {} failed: {e}", m.describe(), m.from);
                app.set_status(format!("Copying files from {} failed: {e}", m.from), false);
            }
        }
    });
}

/// Files dragged from a client and dropped here: into the folder under the
/// drop point (desktop / Explorer window), otherwise onto the clipboard.
fn receive_drop(s: Arc<Shared>, m: Manifest) {
    let point = lk(&s.core).drop_point.take();
    thread::spawn(move || {
        // Asking the file manager can take a moment (or a permission prompt).
        let folder = point.and_then(|(x, y)| crate::platform::folder_at(x, y));
        let dest = folder.clone().unwrap_or_else(transfer::new_drop_dir);
        match fetch_from_client(&s, &m, &dest) {
            Ok(paths) => {
                crate::clipboard::set_files(&paths);
                transfer::mark_received(&paths);
                let msg = match &folder {
                    Some(dir) => format!("{} from {} dropped into {}", m.describe(), m.from, dir.display()),
                    None => format!("{} from {} is ready to paste", m.describe(), m.from),
                };
                s.app.set_status(msg, true);
            }
            Err(e) => {
                log::warn!("receiving dropped files failed: {e}");
                s.app.set_status(format!("Receiving files from {} failed: {e}", m.from), false);
            }
        }
    });
}

/// Files dragged from here and dropped on client `name`: offer them; the
/// client pulls them over parallel connections.
fn send_files(s: &Arc<Shared>, name: String, paths: Vec<String>) {
    let me = lk(&s.core).me.clone();
    let paths: Vec<PathBuf> = paths.into_iter().map(Into::into).collect();
    match transfer::outbox().offer(&me, &paths) {
        Ok(manifest) => {
            log::info!("offering {} to {name}", manifest.describe());
            lk(&s.core).send_to(&name, ToClient::DropOffer { manifest });
        }
        Err(e) => log::warn!("couldn't read the dragged files: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Windows low-level hooks
// ---------------------------------------------------------------------------

#[cfg(windows)]
use hooks as capture;
#[cfg(target_os = "macos")]
#[path = "server_mac.rs"]
pub mod capture;
#[cfg(target_os = "linux")]
#[path = "server_linux.rs"]
pub mod capture;

#[cfg(windows)]
mod hooks {
    use super::*;
    use crate::win32::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    static ACTIVE: Mutex<Option<Arc<Shared>>> = Mutex::new(None);
    pub fn failure() -> String {
        "Could not install keyboard/mouse hooks".into()
    }

    pub struct WinDesktop;

    impl Desktop for WinDesktop {
        fn monitors(&self) -> Vec<Monitor> {
            crate::platform::monitor_rects()
        }
        fn center(&self) -> (i32, i32) {
            primary_center()
        }
        fn set_cursor(&mut self, x: i32, y: i32) {
            unsafe {
                let mut p = POINT::default();
                if GetCursorPos(&mut p) == 0 || p.x != x || p.y != y {
                    SetCursorPos(x, y);
                }
            }
        }
        fn show_drop_strip(&mut self, r: (i32, i32, i32, i32)) {
            crate::dragdrop::show_strip(r.0, r.1, r.2, r.3);
        }
        fn hide_drop_strip(&mut self) {
            crate::dragdrop::hide();
        }
        fn end_os_drag(&mut self) {
            crate::dragdrop::end_drag_here();
        }
        fn cursor(&self) -> (i32, i32) {
            cursor_pos()
        }
        fn set_cursor_visible(&mut self, visible: bool) {
            crate::platform::set_cursor_visible(visible);
        }
        fn set_mouse_capture(&mut self, on: bool) {
            MOUSE_OFF.store(!on, Ordering::Relaxed);
            let thread = HOOK_THREAD.load(Ordering::Relaxed);
            if thread != 0 {
                unsafe { PostThreadMessageW(thread, WM_APP_MOUSE_HOOK, 0, 0) };
            }
        }
    }

    fn shared() -> Option<Arc<Shared>> {
        lk(&ACTIVE).clone()
    }

    pub fn desktop() -> Box<dyn Desktop> {
        Box::new(WinDesktop)
    }

    fn handle_mouse(msg: u32, ms: &MSLLHOOKSTRUCT) -> bool {
        let took = handle_mouse_inner(msg, ms);
        crate::flight::rec("win mouse", "msg x y data took", &[msg as i64, ms.pt.x as i64, ms.pt.y as i64, (ms.mouseData >> 16) as i16 as i64, took as i64]);
        took
    }

    fn handle_mouse_inner(msg: u32, ms: &MSLLHOOKSTRUCT) -> bool {
        let Some(s) = shared() else { return false };
        let Some(mut core) = try_core(&s) else { return false };
        let hi = (ms.mouseData >> 16) as u16;
        match msg {
            WM_MOUSEMOVE => core.on_mouse_move(ms.pt.x, ms.pt.y),
            WM_LBUTTONDOWN => core.on_button(BTN_LEFT, true),
            WM_LBUTTONUP => core.on_button(BTN_LEFT, false),
            WM_RBUTTONDOWN => core.on_button(BTN_RIGHT, true),
            WM_RBUTTONUP => core.on_button(BTN_RIGHT, false),
            WM_MBUTTONDOWN => core.on_button(BTN_MIDDLE, true),
            WM_MBUTTONUP => core.on_button(BTN_MIDDLE, false),
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                let b = if hi == 1 { BTN_BACK } else { BTN_FORWARD };
                core.on_button(b, msg == WM_XBUTTONDOWN)
            }
            WM_MOUSEWHEEL => core.on_wheel(0, hi as i16 as i32),
            WM_MOUSEHWHEEL => core.on_wheel(hi as i16 as i32, 0),
            _ => core.is_remote(),
        }
    }

    fn handle_key(msg: u32, kb: &KBDLLHOOKSTRUCT) -> bool {
        let Some(s) = shared() else { return false };
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if !down && !up {
            return false;
        }
        let Some(mut core) = try_core(&s) else { return false };
        let took = core.on_key(kb.vkCode, kb.scanCode, kb.flags & LLKHF_EXTENDED != 0, down);
        // Which key, only for keys that don't type anything (see flight::types_text).
        let typing = crate::keymap::win_to_evdev(kb.scanCode, kb.flags & LLKHF_EXTENDED != 0, kb.vkCode)
            .is_some_and(crate::flight::types_text);
        let (vk, scan) = if typing { (0, 0) } else { (kb.vkCode as i64, kb.scanCode as i64) };
        crate::flight::rec("win key", "vk scan flags down took", &[vk, scan, kb.flags as i64, down as i64, took as i64]);
        took
    }

    /// Tick of the last event our hooks saw (to notice if Windows drops them).
    static LAST_HOOK_TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static LAST_MOUSE_TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static LAST_KEY_TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    const WM_APP_REHOOK: u32 = 0x8000 + 1;
    /// Install or remove the mouse hook to match MOUSE_OFF (game mode).
    const WM_APP_MOUSE_HOOK: u32 = 0x8000 + 2;
    /// Game mode: the mouse hook is removed, so games get untouched input.
    static MOUSE_OFF: AtomicBool = AtomicBool::new(false);
    static HOOK_THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// Windows saw recent input that our hooks never got: they were removed.
    pub fn look_lost() -> bool {
        if MOUSE_OFF.load(Ordering::Relaxed) {
            return false; // game mode: mouse input never reaches us, on purpose
        }
        let last_hook = LAST_HOOK_TICK.load(Ordering::Relaxed);
        if last_hook == 0 {
            return false;
        }
        let input = last_input_tick();
        let now = unsafe { GetTickCount() };
        let recent_input = now.wrapping_sub(input) < 3000;
        let missed = input.wrapping_sub(last_hook) as i32 > 1500;
        recent_input && missed
    }

    /// What the watchdog saw when it decided the hooks were gone, for the log.
    pub fn loss_report() -> String {
        let now = unsafe { GetTickCount() };
        let age = |t: u32| if t == 0 { "never".to_string() } else { format!("{} ms ago", now.wrapping_sub(t)) };
        format!(
            "Windows saw input {}; our mouse hook last ran {}, keyboard hook {}; in front: {}",
            age(last_input_tick()),
            age(LAST_MOUSE_TICK.load(Ordering::Relaxed)),
            age(LAST_KEY_TICK.load(Ordering::Relaxed)),
            crate::win32::foreground_description()
        )
    }

    pub fn reinstall(thread_id: usize) {
        unsafe {
            PostThreadMessageW(thread_id as u32, WM_APP_REHOOK, 0, 0);
        }
    }

    unsafe fn install() -> (HANDLE, HANDLE) {
        let hmod = GetModuleHandleW(std::ptr::null());
        (install_mouse(), SetWindowsHookExW(WH_KEYBOARD_LL, key_proc, hmod, 0))
    }

    /// The mouse hook, unless game mode has it off (then a null handle).
    unsafe fn install_mouse() -> HANDLE {
        if MOUSE_OFF.load(Ordering::Relaxed) {
            return std::ptr::null_mut();
        }
        SetWindowsHookExW(WH_MOUSE_LL, mouse_proc, GetModuleHandleW(std::ptr::null()), 0)
    }

    /// Windows only waits so long for a hook: past that it delivers the
    /// event to this computer anyway (a key then reaches both computers).
    fn note_slow(what: &str, id: u32, start: std::time::Instant) {
        let ms = start.elapsed().as_millis() as i64;
        if ms >= 50 {
            log::warn!("input hook took {ms} ms for a {what} event ({id:#x}); Windows lets input through after about 300 ms");
            crate::flight::rec("slow hook", "ms id", &[ms, id as i64]);
        }
    }

    unsafe extern "system" fn mouse_proc(code: i32, w: usize, l: isize) -> isize {
        let tick = GetTickCount();
        LAST_HOOK_TICK.store(tick, Ordering::Relaxed);
        LAST_MOUSE_TICK.store(tick, Ordering::Relaxed);
        if code >= 0 {
            let ms = &*(l as *const MSLLHOOKSTRUCT);
            if ms.flags & LLMHF_INJECTED == 0 {
                let start = std::time::Instant::now();
                let block = catch_unwind(AssertUnwindSafe(|| handle_mouse(w as u32, ms))).unwrap_or(false);
                note_slow("mouse", w as u32, start);
                if block {
                    return 1;
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, w, l)
    }

    unsafe extern "system" fn key_proc(code: i32, w: usize, l: isize) -> isize {
        let tick = GetTickCount();
        LAST_HOOK_TICK.store(tick, Ordering::Relaxed);
        LAST_KEY_TICK.store(tick, Ordering::Relaxed);
        if code >= 0 {
            let kb = &*(l as *const KBDLLHOOKSTRUCT);
            if kb.flags & LLKHF_INJECTED == 0 {
                let start = std::time::Instant::now();
                let block = catch_unwind(AssertUnwindSafe(|| handle_key(w as u32, kb))).unwrap_or(false);
                note_slow("key", kb.vkCode, start);
                if block {
                    return 1;
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, w, l)
    }

    pub fn run(s: Arc<Shared>, ready: mpsc::Sender<usize>, drags: mpsc::Sender<Vec<String>>) {
        *lk(&ACTIVE) = Some(s);
        // A new engine starts with game mode off: watch the mouse again.
        MOUSE_OFF.store(false, Ordering::Relaxed);
        // The drop target lives on this thread: it needs the message loop below.
        crate::dragdrop::init(drags);
        if !crate::win32::raise_thread_priority() {
            log::warn!("couldn't raise the input thread's priority");
        }
        unsafe {
            HOOK_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
            let (mut mh, mut kh) = install();
            if (mh.is_null() && !MOUSE_OFF.load(Ordering::Relaxed)) || kh.is_null() {
                log::error!("SetWindowsHookEx failed");
                if !mh.is_null() {
                    UnhookWindowsHookEx(mh);
                }
                if !kh.is_null() {
                    UnhookWindowsHookEx(kh);
                }
                *lk(&ACTIVE) = None;
                let _ = ready.send(0);
                return;
            }
            let _ = ready.send(GetCurrentThreadId() as usize);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.message == WM_APP_REHOOK {
                    if !mh.is_null() {
                        UnhookWindowsHookEx(mh);
                    }
                    UnhookWindowsHookEx(kh);
                    (mh, kh) = install();
                    if (mh.is_null() && !MOUSE_OFF.load(Ordering::Relaxed)) || kh.is_null() {
                        log::error!("reinstalling input hooks failed");
                    }
                    continue;
                }
                if msg.message == WM_APP_MOUSE_HOOK {
                    let off = MOUSE_OFF.load(Ordering::Relaxed);
                    if off && !mh.is_null() {
                        UnhookWindowsHookEx(mh);
                        mh = std::ptr::null_mut();
                        log::info!("game mode: mouse hook removed; the mouse goes straight to Windows");
                    } else if !off && mh.is_null() {
                        mh = install_mouse();
                        if mh.is_null() {
                            log::error!("couldn't put the mouse hook back after game mode");
                        } else {
                            log::info!("mouse hook back");
                        }
                    }
                    continue;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if !mh.is_null() {
                UnhookWindowsHookEx(mh);
            }
            UnhookWindowsHookEx(kh);
        }
        HOOK_THREAD.store(0, Ordering::Relaxed);
        *lk(&ACTIVE) = None;
        log::info!("input capture stopped");
    }

    pub fn stop(thread_id: usize) {
        unsafe {
            PostThreadMessageW(thread_id as u32, WM_QUIT, 0, 0);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests for the switching logic (no OS hooks involved)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tests::user_monitors;

    struct FakeDesk {
        mons: Vec<Monitor>,
        cursor: Arc<Mutex<(i32, i32)>>,
    }

    // Per test thread, so tests running in parallel don't see each other's counts.
    thread_local! {
        static STRIPS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        static ENDED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    impl Desktop for FakeDesk {
        fn monitors(&self) -> Vec<Monitor> {
            self.mons.clone()
        }
        fn center(&self) -> (i32, i32) {
            (960, 540)
        }
        fn set_cursor(&mut self, x: i32, y: i32) {
            *self.cursor.lock().unwrap() = (x, y);
        }
        fn show_drop_strip(&mut self, r: (i32, i32, i32, i32)) {
            assert!(r.2 > 0 && r.3 > 0, "strip has an area: {r:?}");
            STRIPS.with(|c| c.set(c.get() + 1));
        }
        fn end_os_drag(&mut self) {
            ENDED.with(|c| c.set(c.get() + 1));
        }
    }

    /// The user's real setup with the Surface (1520x1014) below monitor 1.
    fn setup(fit: bool) -> (Core, mpsc::Receiver<ToClient>, Arc<Mutex<(i32, i32)>>) {
        let mut cfg = Config::default();
        cfg.name = "pc".into();
        let mons = user_monitors();
        let (x, y, scale) =
            crate::layout::attach(&mons[1].rect, crate::layout::Side::Bottom, 1520, 1014, fit, None);
        cfg.screens = vec![ScreenPos { name: "surface".into(), x, y, scale, w: 1520, h: 1014, fit }];
        let cursor = Arc::new(Mutex::new((0, 0)));
        let mut core = Core::new(&cfg, Box::new(FakeDesk { mons, cursor: cursor.clone() }), None);
        let (tx, rx) = mpsc::channel();
        core.add_client("surface", Slot { id: 1, tx, bulk: Bulk::new(), last_seen: Arc::new(AtomicU64::new(now_ms())), w: 1520, h: 1014, clip_gen: 0 });
        (core, rx, cursor)
    }

    fn enter_pos(rx: &mpsc::Receiver<ToClient>) -> (i32, i32) {
        for m in rx.try_iter() {
            if let ToClient::Enter { x, y } = m {
                return (x, y);
            }
        }
        panic!("no Enter sent");
    }

    #[test]
    fn corners_map_to_corners_when_fitted() {
        let (mut core, rx, _) = setup(true);
        assert!(core.on_mouse_move(0, 1079), "bottom-left corner of monitor 1");
        assert_eq!(enter_pos(&rx), (0, 0));
        core.go_home();
        assert!(core.on_mouse_move(1919, 1079), "bottom-right corner of monitor 1");
        assert_eq!(enter_pos(&rx), (1519, 0));
        core.go_home();
        assert!(core.on_mouse_move(960, 1079), "middle");
        let (x, _) = enter_pos(&rx);
        assert!((758..=761).contains(&x), "middle maps to middle, got {x}");
    }

    #[test]
    fn actual_size_maps_pixel_for_pixel() {
        let (mut core, rx, _) = setup(false); // centered under monitor 1: x 200..1720
        assert!(core.on_mouse_move(500, 1079));
        assert_eq!(enter_pos(&rx), (300, 0));
        core.go_home();
        assert!(!core.on_mouse_move(100, 1079), "nothing below x=100");
    }

    #[test]
    fn other_monitor_edges_do_not_switch() {
        let (mut core, _rx, _) = setup(true);
        assert!(!core.on_mouse_move(1919, 500), "monitor 2 continues to the right");
        assert!(!core.on_mouse_move(2500, 1073), "bottom of portrait monitor 2: nothing below");
        assert!(!core.on_mouse_move(-1000, 1081), "bottom of monitor 3: nothing below");
        assert!(!core.on_mouse_move(500, 0), "top of monitor 1");
        assert_eq!(core.active, "pc");
    }

    #[test]
    fn coming_back_lands_where_you_left() {
        let (mut core, rx, cursor) = setup(true);
        assert!(core.on_mouse_move(1919, 1079));
        let _ = enter_pos(&rx);
        assert_eq!(*cursor.lock().unwrap(), (960, 540), "cursor parked");
        // Move left 760 client px, then push up past the top edge.
        for _ in 0..8 {
            assert!(core.on_mouse_move(960 - 95, 540)); // 8 × 95 = 760 px, in realistic steps
        }
        assert!(core.on_mouse_move(960, 540 - 5));
        assert_eq!(core.active, "surface", "a nudge against the edge stays (edge resistance)");
        assert!(core.on_mouse_move(960, 540 - 40));
        assert!(core.on_mouse_move(960, 540 - 40));
        assert_eq!(core.active, "pc", "a firm push crosses over");
        assert!(rx.try_iter().any(|m| matches!(m, ToClient::Leave)));
        let (x, y) = *cursor.lock().unwrap();
        assert_eq!(y, 1079, "bottom row of monitor 1");
        assert!((958..=962).contains(&x), "same spot horizontally, got {x}");
    }

    #[test]
    fn sides_of_the_surface_lead_nowhere() {
        let (mut core, rx, _) = setup(true);
        core.on_mouse_move(1919, 1079);
        let _ = enter_pos(&rx);
        for _ in 0..6 {
            core.on_mouse_move(960 + 10, 540 + 50); // right + down inside
        }
        for _ in 0..8 {
            assert!(core.on_mouse_move(960 + 100, 540)); // shove right past the edge
        }
        assert_eq!(core.active, "surface", "no monitor right of the Surface");
        for _ in 0..20 {
            assert!(core.on_mouse_move(960, 540 + 100)); // down past the bottom
        }
        assert_eq!(core.active, "surface");
        let sent: Vec<_> = rx.try_iter().collect();
        assert!(
            sent.iter().any(|m| matches!(m, ToClient::Move { x: 1519, y: 1013 })),
            "pinned in the bottom-right corner"
        );
    }

    #[test]
    fn game_mode_keeps_the_mouse_here() {
        let (mut core, rx, _) = setup(true);
        core.set_game_mode(true, "test");
        assert!(!core.on_mouse_move(960, 1079), "pushing the edge passes through");
        assert!(!core.on_mouse_move(960, 1085));
        assert_eq!(core.active, "pc", "and the mouse stays on the PC");
        assert!(rx.try_recv().is_err(), "nothing goes to the Surface");
        core.set_game_mode(false, "test");
        assert!(core.on_mouse_move(960, 1079));
        assert_eq!(core.active, "surface");
    }

    #[test]
    fn scroll_lock_toggles_game_mode_here_and_goes_home_from_away() {
        let (mut core, rx, _) = setup(true);
        assert!(core.on_key(0x91, 0x46, false, true), "Scroll Lock here is Edgehop's");
        assert!(core.game_mode_on());
        core.on_key(0x91, 0x46, false, false);
        core.on_key(0x91, 0x46, false, true);
        assert!(!core.game_mode_on(), "pressed again: off");
        core.on_key(0x91, 0x46, false, false);
        // On the other computer it still brings the mouse home (no game mode).
        core.on_mouse_move(960, 1079);
        let _ = enter_pos(&rx);
        core.on_key(0x91, 0x46, false, true);
        assert_eq!(core.active, "pc");
        assert!(!core.game_mode_on());
    }

    #[test]
    fn holding_scroll_lock_toggles_once() {
        let (mut core, _rx, _) = setup(true);
        for _ in 0..5 {
            core.on_key(0x91, 0x46, false, true); // the key repeating
        }
        assert!(core.game_mode_on(), "one press, however long it's held");
        core.on_key(0x91, 0x46, false, false);
        assert!(core.game_mode_on());
    }

    #[test]
    fn a_game_starting_while_away_waits_for_the_mouse_to_come_home() {
        let (mut core, rx, _) = setup(true);
        core.on_mouse_move(960, 1079);
        let _ = enter_pos(&rx);
        core.set_fullscreen_app(Some("game.exe".into()));
        assert!(!core.game_mode_on(), "not while the mouse is on the Surface: its pointer would freeze");
        assert!(core.on_mouse_move(960 + 5, 540), "the mouse still drives the Surface");
        core.go_home();
        assert!(core.game_mode_on(), "on once the mouse is back");
    }

    #[test]
    fn a_missed_button_release_doesnt_trap_the_pointer() {
        let (mut core, _rx, _) = setup(true);
        // A press whose release never arrives (clicked into an admin window)...
        core.on_button(BTN_LEFT, true);
        assert!(!core.on_mouse_move(960, 1079), "held: no switching mid-drag");
        // ...is healed by the next click of that button.
        core.on_button(BTN_LEFT, true);
        core.on_button(BTN_LEFT, false);
        assert!(core.on_mouse_move(960, 1079), "the pointer can leave again");
        assert_eq!(core.active, "surface");
    }

    #[test]
    fn the_system_saying_no_button_is_down_clears_a_stuck_one() {
        let (mut core, _rx, _) = setup(true);
        core.on_button(BTN_RIGHT, true);
        assert!(core.thinks_a_button_is_down());
        core.no_buttons_down();
        assert!(core.on_mouse_move(960, 1079));
        assert_eq!(core.active, "surface");
    }

    #[test]
    fn a_key_held_while_crossing_is_released_on_both_computers() {
        let (mut core, rx, _) = setup(true);
        assert!(!core.on_evdev_key(29, true), "Ctrl goes down here");
        core.on_mouse_move(960, 1079);
        let _ = enter_pos(&rx);
        while rx.try_recv().is_ok() {}
        // Its release must reach this computer too (or Ctrl stays stuck here)...
        assert!(!core.on_evdev_key(29, false), "the release is let through here");
        // ...and the other one, which may have seen the key repeat.
        assert!(rx.try_iter().any(|m| matches!(m, ToClient::Key { code: 29, down: false })));
        // Keys pressed over there stay over there.
        assert!(core.on_evdev_key(30, true));
        assert!(core.on_evdev_key(30, false));
    }

    #[test]
    fn fullscreen_games_turn_game_mode_on_by_themselves() {
        let (mut core, _rx, _) = setup(true);
        core.set_fullscreen_app(Some("game.exe".into()));
        assert!(core.game_mode_on());
        // Turned off for this game...
        core.toggle_game_mode("Scroll Lock");
        assert!(!core.game_mode_on());
        core.set_fullscreen_app(Some("game.exe".into()));
        assert!(!core.game_mode_on(), "stays off while that game runs");
        // ...and back to automatic once it closes.
        core.set_fullscreen_app(None);
        core.set_fullscreen_app(Some("other.exe".into()));
        assert!(core.game_mode_on(), "the next game turns it on again");
        core.set_fullscreen_app(None);
        assert!(!core.game_mode_on());
    }

    #[test]
    fn a_game_starting_doesnt_pull_the_mouse_back() {
        let (mut core, rx, _) = setup(true);
        core.on_mouse_move(960, 1079);
        let _ = enter_pos(&rx);
        core.set_fullscreen_app(Some("video.exe".into()));
        assert_eq!(core.active, "surface", "still on the Surface");
    }

    #[test]
    fn a_storm_of_nonsense_positions_is_reported() {
        let (mut core, rx, _) = setup(true);
        assert!(core.on_mouse_move(960, 1079));
        let _ = enter_pos(&rx);
        for i in 0..STALE_STORM - 1 {
            core.on_mouse_move(3000 + i as i32, 2000);
        }
        assert!(core.stale_reported.is_none(), "a few are normal right after crossing");
        core.on_mouse_move(3100, 2000);
        assert!(core.stale_reported.is_some(), "this many in 2 s is reported as an error");
    }

    #[test]
    fn bouncing_between_screens_is_reported() {
        let (mut core, _rx, _) = setup(true);
        for i in 0..BOUNCE_SWITCHES - 1 {
            core.note_switch("x", format!("switch {i}"));
        }
        assert!(core.bounce_reported.is_none(), "a few switches are normal");
        core.note_switch("x", "one too many".into());
        assert!(core.bounce_reported.is_some(), "this many in a few seconds is a bounce");
    }

    #[test]
    fn coming_back_does_not_bounce_straight_out_again() {
        let (mut core, rx, cursor) = setup(true);
        assert!(core.on_mouse_move(960, 1079));
        let _ = enter_pos(&rx);
        // Push up past the Surface's top edge: back on monitor 1, bottom row.
        for _ in 0..4 {
            if core.active == "pc" {
                break;
            }
            core.on_mouse_move(960, 540 - 60);
        }
        assert_eq!(core.active, "pc");
        let (x, y) = *cursor.lock().unwrap();
        assert_eq!(y, 1079);
        // A wobble back down onto the same edge doesn't cross again...
        assert!(!core.on_mouse_move(x, 1079));
        assert!(!core.on_mouse_move(x + 3, 1080));
        assert_eq!(core.active, "pc", "stays on the PC");
        // ...but once the pointer has moved off the edge, it works as usual.
        assert!(!core.on_mouse_move(x, 1060));
        assert!(core.on_mouse_move(x, 1079));
        assert_eq!(core.active, "surface");
    }

    #[test]
    fn mac_and_linux_keys_go_to_the_client() {
        let (mut core, rx, _) = setup(true);
        assert!(!core.on_evdev_key(30, true), "keys stay here while the mouse is here");
        core.on_mouse_move(500, 1079);
        let _ = enter_pos(&rx);
        assert!(core.on_evdev_key(30, true));
        assert!(matches!(rx.try_recv().unwrap(), ToClient::Key { code: 30, down: true }));
        // A Mac sharing its keyboard: Command arrives as Ctrl, and Ctrl as Command.
        core.command_is_ctrl = true;
        core.on_evdev_key(125, true);
        assert!(matches!(rx.try_recv().unwrap(), ToClient::Key { code: 29, down: true }));
        core.on_evdev_key(29, true);
        assert!(matches!(rx.try_recv().unwrap(), ToClient::Key { code: 125, down: true }));
        // Scroll Lock (F14 on a Mac) still brings the mouse home.
        core.on_evdev_key(KEY_SCROLLLOCK, true);
        assert_eq!(core.active, core.me);
    }

    #[test]
    fn keys_and_buttons_go_to_the_client() {
        let (mut core, rx, _) = setup(true);
        core.on_mouse_move(500, 1079);
        let _ = enter_pos(&rx);
        assert!(core.on_key(0x41, 0x1E, false, true));
        assert!(matches!(rx.try_recv().unwrap(), ToClient::Key { code: 30, down: true }));
        assert!(core.on_button(BTN_LEFT, true));
        assert!(matches!(rx.try_recv().unwrap(), ToClient::Button { button: BTN_LEFT, down: true }));
        for _ in 0..6 {
            assert!(core.on_mouse_move(960, 540 - 100), "can't leave while a button is held");
        }
        assert_eq!(core.active, "surface");
    }

    #[test]
    fn no_switch_while_dragging_locally() {
        let (mut core, _rx, _) = setup(true);
        core.on_button(BTN_LEFT, true);
        assert!(!core.on_mouse_move(500, 1079));
        assert_eq!(core.active, "pc");
    }

    #[test]
    fn scroll_lock_returns_home_and_disconnect_releases() {
        let (mut core, _rx, _) = setup(true);
        core.on_mouse_move(500, 1079);
        assert!(core.on_key(0x91, 0x46, false, true));
        assert_eq!(core.active, "pc");
        core.on_mouse_move(500, 1079);
        assert_eq!(core.active, "surface");
        core.remove_client("surface", 1);
        assert_eq!(core.active, "pc");
    }

    #[test]
    fn clipboard_follows_focus() {
        let (mut core, rx, _) = setup(true);
        core.local_clipboard(Clip::Text { text: "hello".into() });
        core.on_mouse_move(500, 1079);
        assert!(rx.try_iter().any(|m| matches!(m, ToClient::Clipboard { ref text } if text == "hello")));
    }

    #[test]
    fn new_client_is_auto_placed_below_monitor_1() {
        let mut cfg = Config::default();
        cfg.name = "pc".into();
        let cursor = Arc::new(Mutex::new((0, 0)));
        let mut core = Core::new(&cfg, Box::new(FakeDesk { mons: user_monitors(), cursor }), None);
        let (tx, _rx) = mpsc::channel();
        core.add_client("surface", Slot { id: 1, tx, bulk: Bulk::new(), last_seen: Arc::new(AtomicU64::new(now_ms())), w: 1520, h: 1014, clip_gen: 0 });
        let p = core.ensure_placed("surface").expect("placed");
        assert_eq!((p.x, p.y), (0.0, 1080.0));
        assert!(core.ensure_placed("surface").is_none(), "unchanged second time");
    }

    #[test]
    fn edge_resistance_needs_a_continuous_push() {
        let (mut core, _rx, _) = setup(true);
        core.on_mouse_move(500, 1079);
        assert_eq!(core.active, "surface");
        // Clicking near the top: lots of small nudges, but backing off in between.
        for _ in 0..10 {
            core.on_mouse_move(960, 540 - 30);
            core.on_mouse_move(960, 540 + 5);
        }
        assert_eq!(core.active, "surface", "nudges with pauses never add up");
        // Moving sideways along the edge resets too.
        core.on_mouse_move(960, 540 - 50);
        core.on_mouse_move(960 + 40, 540);
        core.on_mouse_move(960, 540 - 50);
        assert_eq!(core.active, "surface");
        core.on_mouse_move(960, 540 - 50);
        assert_eq!(core.active, "pc", "50 + 50 >= 80 px of continuous push");
    }

    #[test]
    fn zero_resistance_switches_immediately() {
        let (mut core, _rx, _) = setup(true);
        let mut cfg = Config::default();
        cfg.name = "pc".into();
        cfg.screens = core.screens.clone();
        cfg.edge_resistance = 0;
        core.apply_config(&cfg);
        core.on_mouse_move(500, 1079);
        assert!(core.on_mouse_move(960, 539));
        assert_eq!(core.active, "pc");
    }

    fn with_file_jobs(core: &mut Core) -> mpsc::Receiver<FileJob> {
        let (tx, rx) = mpsc::channel();
        core.file_jobs = Some(tx);
        rx
    }

    #[test]
    fn drag_files_from_windows_onto_the_surface() {
        let (mut core, rx, _) = setup(true);
        let jobs = with_file_jobs(&mut core);
        assert!(!core.on_button(BTN_LEFT, true), "press goes to Explorer");
        core.on_mouse_move(500, 900);
        for _ in 0..4 {
            assert!(!core.on_mouse_move(500, 1079), "pushing a drag at the edge stays local");
        }
        assert!(STRIPS.with(|c| c.get()) >= 1, "a drop catcher is placed along the edge");
        assert!(jobs.try_recv().is_err(), "nothing sent yet");

        // The drop target reports what the drag carries.
        core.local_grab_result(vec![r"C:\Users\j\photo.jpg".into()]);
        assert_eq!(core.active, "surface", "carried across");
        assert_eq!(ENDED.with(|c| c.get()), 1, "the drag is ended on Windows, over our own window");
        let sent: Vec<_> = rx.try_iter().collect();
        assert!(sent.iter().any(|m| matches!(m, ToClient::Enter { .. })));
        assert!(!sent.iter().any(|m| matches!(m, ToClient::Button { .. })), "the Surface never sees the press");

        assert!(core.on_mouse_move(960 + 20, 540 + 200), "moving while carrying");
        assert!(core.on_button(BTN_LEFT, false), "the drop is swallowed");
        assert!(!rx.try_iter().any(|m| matches!(m, ToClient::Button { .. })));
        match jobs.try_recv() {
            Ok(FileJob::SendToClient { name, paths }) => {
                assert_eq!(name, "surface");
                assert_eq!(paths, vec![r"C:\Users\j\photo.jpg".to_string()]);
            }
            _ => panic!("expected SendToClient"),
        }
    }

    #[test]
    fn dragging_something_that_is_not_files_stays_put() {
        let (mut core, _rx, _) = setup(true);
        let jobs = with_file_jobs(&mut core);
        core.on_button(BTN_LEFT, true);
        for _ in 0..10 {
            core.on_mouse_move(500, 1079);
        }
        assert!(STRIPS.with(|c| c.get()) >= 1);
        core.local_grab_result(Vec::new()); // e.g. dragging text, not files
        assert_eq!(core.active, "pc");
        let shown = STRIPS.with(|c| c.get());
        for _ in 0..10 {
            core.on_mouse_move(500, 1079);
        }
        assert_eq!(STRIPS.with(|c| c.get()), shown, "no retry until the button is released");
        assert!(jobs.try_recv().is_err());
        core.on_button(BTN_LEFT, false);
        core.on_mouse_move(500, 1079);
        assert_eq!(core.active, "surface", "normal crossing works again");
    }

    #[test]
    fn drag_files_from_the_surface_onto_windows() {
        let (mut core, rx, _) = setup(true);
        let _jobs = with_file_jobs(&mut core);
        core.on_mouse_move(500, 1079);
        let _: Vec<_> = rx.try_iter().collect();
        assert!(core.on_button(BTN_LEFT, true));
        assert!(matches!(rx.try_recv(), Ok(ToClient::Button { button: BTN_LEFT, down: true })));
        core.on_mouse_move(960, 540 - 50);
        core.on_mouse_move(960, 540 - 50);
        assert!(rx.try_iter().any(|m| matches!(m, ToClient::FilesGrab)), "asks the Surface what it drags");
        assert_eq!(core.active, "surface");

        core.client_grab_result("surface", vec!["/home/j/report.pdf".into()]);
        assert_eq!(core.active, "pc", "carried onto Windows");
        assert!(rx.try_iter().any(|m| matches!(m, ToClient::Leave)), "the Surface releases its button on Leave");

        assert!(!core.on_button(BTN_LEFT, false), "release on Windows passes through");
        match rx.try_recv() {
            Ok(ToClient::FilesSend { paths }) => assert_eq!(paths, vec!["/home/j/report.pdf".to_string()]),
            other => panic!("expected FilesSend, got {other:?}"),
        }
    }

    #[test]
    fn carrying_back_to_where_it_started_cancels() {
        let (mut core, rx, _) = setup(true);
        let jobs = with_file_jobs(&mut core);
        core.on_button(BTN_LEFT, true);
        for _ in 0..4 {
            core.on_mouse_move(500, 1079);
        }
        core.local_grab_result(vec!["C:\\x".into()]);
        assert_eq!(core.active, "surface");
        core.on_mouse_move(960, 540 - 200); // back up onto Windows, still holding
        assert_eq!(core.active, "pc");
        core.on_button(BTN_LEFT, false);
        assert!(jobs.try_recv().is_err(), "nothing sent");
        assert!(!rx.try_iter().any(|m| matches!(m, ToClient::FilesSend { .. })));
    }

    #[test]
    fn pushing_past_the_edge_reports_points_off_screen() {
        // Windows reports where the mouse would go (y > 1079) before pinning it.
        let (mut core, rx, _) = setup(true);
        assert!(core.on_mouse_move(500, 1086), "a push below monitor 1 crosses");
        assert_eq!(core.active, "surface");
        assert!(rx.try_iter().any(|m| matches!(m, ToClient::Enter { y: 0, .. })));
    }

    #[test]
    fn dragging_files_with_off_screen_points_starts_a_grab() {
        let (mut core, _rx, _) = setup(true);
        let jobs = with_file_jobs(&mut core);
        core.on_button(BTN_LEFT, true);
        core.on_mouse_move(500, 1070);
        for y in [1081, 1083, 1084, 1088] {
            assert!(!core.on_mouse_move(500, y));
        }
        assert!(STRIPS.with(|c| c.get()) >= 1, "pushes past the edge place the drop catcher");
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn far_off_screen_points_are_ignored() {
        let (mut core, _rx, _) = setup(true);
        assert!(!core.on_mouse_move(500, 5000));
        assert_eq!(core.active, "pc");
    }

    #[test]
    fn stale_position_right_after_crossing_does_not_bounce_back() {
        let (mut core, rx, cursor) = setup(true);
        assert!(core.on_mouse_move(1671, 1079));
        assert_eq!(core.active, "surface");
        let _: Vec<_> = rx.try_iter().collect();
        // The next report still comes from far away (not relative to the parked spot).
        assert!(core.on_mouse_move(-90, 1080));
        assert_eq!(core.active, "surface", "must not bounce onto monitor 3");
        assert_eq!(*cursor.lock().unwrap(), (960, 540), "re-parked");
        assert!(!rx.try_iter().any(|m| matches!(m, ToClient::Move { .. })), "stale report is not a move");
        // Then normal small moves work.
        assert!(core.on_mouse_move(965, 545));
        assert!(rx.try_iter().any(|m| matches!(m, ToClient::Move { .. })));
    }

    #[test]
    fn unresponsive_client_gives_the_mouse_back() {
        let (mut core, _rx, cursor) = setup(true);
        core.on_mouse_move(500, 1079);
        assert_eq!(core.active, "surface");
        core.check_responsive();
        assert_eq!(core.active, "surface", "answering: stays");
        // Silent for 5 s.
        core.clients["surface"].last_seen.store(now_ms().saturating_sub(5000), Ordering::Relaxed);
        core.check_responsive();
        assert_eq!(core.active, "pc", "mouse comes back");
        assert_eq!(*cursor.lock().unwrap(), (960, 540));
        assert!(!core.on_mouse_move(500, 1079), "won't cross into a computer that isn't answering");
        assert_eq!(core.active, "pc");
        core.clients["surface"].last_seen.store(now_ms(), Ordering::Relaxed);
        assert!(core.on_mouse_move(500, 1079), "answering again: crossing works");
    }

    #[test]
    fn clipboard_images_follow_focus_without_blocking_input() {
        let (mut core, rx, _) = setup(true);
        core.local_clipboard(Clip::Image { png: "iVBORw0KGgo=".into() });
        core.on_mouse_move(500, 1079);
        assert!(
            !rx.try_iter().any(|m| matches!(m, ToClient::ClipboardImage { .. })),
            "images don't go down the input channel"
        );
        let queued = core.clients["surface"].bulk.pop();
        assert!(matches!(queued, Some(ToClient::ClipboardImage { ref png }) if png == "iVBORw0KGgo="));
    }
}
