//! Server: owns the physical keyboard/mouse, decides which screen is active,
//! and forwards input to the active client.
use crate::config::{Config, Peer};
use crate::layout::{Monitor, Rect, ScreenPos};
use crate::net::{self, Announce};
use crate::files;
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

/// Outgoing file data for one client, sent between input messages.
pub struct Bulk {
    q: Mutex<VecDeque<ToClient>>,
    cv: Condvar,
    closed: AtomicBool,
}

impl Bulk {
    pub fn new() -> Arc<Bulk> {
        Arc::new(Bulk { q: Mutex::new(VecDeque::new()), cv: Condvar::new(), closed: AtomicBool::new(false) })
    }
    /// Queue a message, waiting while the queue is full (backpressure for the file reader).
    pub fn push(&self, m: ToClient) -> std::io::Result<()> {
        let mut q = lk(&self.q);
        while q.len() >= 16 && !self.closed.load(Ordering::Relaxed) {
            q = self.cv.wait_timeout(q, Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
        }
        if self.closed.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("connection closed"));
        }
        q.push_back(m);
        Ok(())
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
}

const VK_SCROLL: u32 = 0x91;
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
                match clip {
                    Clip::Text { text } => {
                        let _ = slot.tx.send(ToClient::Clipboard { text });
                    }
                    // Images can be megabytes: send them in the gaps between input.
                    Clip::Image { png } => slot.bulk.push_now(ToClient::ClipboardImage { png }),
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
            Hit::Server(sx, sy) => self.enter_server(sx, sy),
            Hit::Client(name, px, py) => self.enter_client(name, dir, px, py),
        }
    }

    /// We crossed while the physical left button is still held down.
    fn hold_left_here(&mut self) {
        if self.is_remote() {
            self.remote_buttons = 1;
            self.left_remote = true;
        } else {
            self.local_buttons = 1;
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
            let hit = self.edge_hit(x, y);
            if self.local_buttons > 0 {
                // A left-drag pushed against an edge with a screen beyond: maybe files.
                let can_grab = self.left_local
                    && self.local_buttons == 1
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
            log::info!("ignoring stale mouse position ({x}, {y}); parked at ({cx0}, {cy0})");
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
                let carrying = self.carry.is_some() && self.left_remote && self.remote_buttons == 1;
                match self.cast(ox, oy, dir, Some(&active)) {
                    Some(hit) if self.remote_buttons == 0 => {
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
                            && self.remote_buttons == 1
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
        self.send(&active, ToClient::Move { x: ax as i32, y: ay as i32 });
        // Keep the real cursor parked so the next delta is measured from the center.
        self.desk.set_cursor(cx0, cy0);
        true
    }

    pub fn on_button(&mut self, button: u8, down: bool) -> bool {
        let remote = self.is_remote();
        let counter = if remote { &mut self.remote_buttons } else { &mut self.local_buttons };
        *counter = if down { *counter + 1 } else { counter.saturating_sub(1) };
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
        let Some(p) = self.grab_pending.take() else { return };
        self.grab_tried = true;
        if files.is_empty() || p.from != name || self.active != name || !self.left_remote {
            return;
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

    pub fn on_key(&mut self, vk: u32, scan: u32, extended: bool, down: bool) -> bool {
        if !self.is_remote() {
            return false;
        }
        if vk == VK_SCROLL {
            if down {
                log::info!("Scroll Lock pressed: returning to {}", self.me);
                self.go_home();
            }
            return true;
        }
        if let Some(code) = crate::keymap::win_to_evdev(scan, extended, vk) {
            self.send(&self.active, ToClient::Key { code, down });
        }
        true
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
        self.push = 0.0;
        self.push_dir = None;
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
        log::info!("switch {} -> {name} at ({}, {})", self.active, cx as i32, cy as i32);
        self.leave_current();
        if from_server {
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
        self.leave_current();
        self.active = self.me.clone();
        self.desk.set_cursor(x, y);
        self.desk.set_cursor_visible(true);
        self.desk.hide_drop_strip();
        self.sync_server_clipboard();
        self.publish();
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

    /// Register a client; returns the id of a replaced older connection.
    pub fn add_client(&mut self, name: &str, slot: Slot) -> Option<u64> {
        self.clients.insert(name.to_string(), slot).map(|old| old.id)
    }

    /// Make sure a connected client has a place next to a monitor and that its
    /// stored size is current. Returns the placement to save if it changed.
    pub fn ensure_placed(&mut self, name: &str) -> Option<ScreenPos> {
        let (w, h) = self.clients.get(name).map(|s| (s.w, s.h))?;
        self.mons = self.desk.monitors();
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
        if !self.clipboard {
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
    next_id: AtomicU64,
    private_key: Vec<u8>,
    stop: Arc<AtomicBool>,
}

pub struct ServerEngine {
    shared: Arc<Shared>,
    port: u16,
    #[allow(dead_code)]
    hook_thread: u32,
}

impl ServerEngine {
    pub fn start(app: Arc<AppState>) -> Result<ServerEngine, String> {
        #[cfg(not(windows))]
        {
            let _ = app;
            Err("Server mode is only available on Windows for now. Use Client mode on this computer.".into())
        }
        #[cfg(windows)]
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
            let (clip_tx, clip_rx) = mpsc::channel();
            let mut core = Core::new(&cfg, Box::new(hooks::WinDesktop), Some(app.clone()));
            core.clip_jobs = Some(clip_tx);
            let (file_tx, file_rx) = mpsc::channel::<FileJob>();
            core.file_jobs = Some(file_tx);
            let shared = Arc::new(Shared {
                app: app.clone(),
                core: Mutex::new(core),
                streams: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                private_key: cfg.private_key_bytes(),
                stop: stop.clone(),
            });

            // Files a drag carries, reported by the drop target at the screen edge.
            let (drag_tx, drag_rx) = mpsc::channel::<Vec<String>>();
            let s = shared.clone();
            thread::spawn(move || {
                for files in drag_rx {
                    recovery::guard("drag handler", || lk(&s.core).local_grab_result(files));
                }
            });

            // Input capture thread (low-level hooks need their own message loop).
            let (tid_tx, tid_rx) = mpsc::channel();
            let s = shared.clone();
            thread::Builder::new()
                .name("hooks".into())
                .spawn(move || hooks::run(s, tid_tx, drag_tx))
                .map_err(|e| e.to_string())?;
            let hook_thread = tid_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "Input capture failed to start".to_string())?;
            if hook_thread == 0 {
                return Err("Could not install keyboard/mouse hooks".into());
            }

            // File worker: grabbing a dragged selection and sending files.
            let s = shared.clone();
            thread::spawn(move || {
                for job in file_rx {
                    recovery::guard("file worker", || match job {
                        FileJob::SendToClient { name, paths } => send_files(&s, name, paths),
                    });
                }
            });

            // Clipboard worker (keeps clipboard I/O out of the hook callback).
            let s = shared.clone();
            thread::spawn(move || {
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
                                    return;
                                }
                                ours = Some(seq);
                            }
                            let clip = match crate::clipboard::get().filter(|t| !t.is_empty()) {
                                Some(text) => Some(Clip::Text { text }),
                                None => crate::clipboard::get_image_png().map(|p| Clip::Image { png: files::b64(&p) }),
                            };
                            if let Some(c) = clip {
                                lk(&s.core).local_clipboard(c);
                            }
                        }
                        ClipJob::SetLocal(c) => {
                            match c {
                                Clip::Text { text } => crate::clipboard::set(&text),
                                Clip::Image { png } => {
                                    if let Some(bytes) = files::unb64(&png) {
                                        crate::clipboard::set_image_png(&bytes);
                                        log::info!("clipboard image received ({})", files::human(bytes.len() as u64));
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
            });

            // Accept loop.
            let s = shared.clone();
            thread::spawn(move || {
                for conn in listener.incoming() {
                    if s.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = conn else { continue };
                    let s2 = s.clone();
                    thread::spawn(move || {
                        let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                        recovery::guard("connection handler", || {
                            if let Err(e) = handle_client(&s2, stream) {
                                log::info!("client {peer}: {e}");
                            }
                        });
                    });
                }
            });

            // LAN discovery.
            let announce = Announce { name: cfg.name.clone(), port: cfg.port, key: cfg.public_key.clone() };
            let st = stop.clone();
            thread::spawn(move || net::run_discovery_responder(st, announce));

            // Watchdog + keepalive, every second:
            // - ping clients; take the mouse back from one that stops answering;
            // - if the switching state stays locked (a bug), restart cleanly;
            // - if Windows silently dropped our input hooks, reinstall them.
            let s = shared.clone();
            thread::Builder::new()
                .name("watchdog".into())
                .spawn(move || {
                    let mut last_rehook: Option<Instant> = None;
                    while !s.stop.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_secs(1));
                        let mons = crate::platform::monitor_rects();
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
                        drop(core);
                        if hooks::look_lost() && last_rehook.map_or(true, |t| t.elapsed() > Duration::from_secs(10)) {
                            log::warn!("Windows dropped our input hooks; reinstalling them");
                            hooks::reinstall(hook_thread);
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

    pub fn apply_config(&self, cfg: &Config) {
        lk(&self.shared.core).apply_config(cfg);
    }

    pub fn stop(self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        lk(&self.shared.core).go_home();
        crate::platform::restore_cursor();
        #[cfg(windows)]
        hooks::stop(self.hook_thread);
        let _ = TcpStream::connect_timeout(
            &([127, 0, 0, 1], self.port).into(),
            Duration::from_millis(300),
        );
        for (_, s) in lk(&self.shared.streams).drain() {
            let _ = s.shutdown(Shutdown::Both);
        }
        lk(&self.shared.core).clients.clear();
    }
}

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
    let ToServer::Hello { name, width, height, os, .. } = rx.recv::<ToServer>()? else {
        return Err(std::io::Error::other("expected hello"));
    };
    let mut name = sanitize_name(&name);
    let app = &s.app;
    let cfg = app.cfg();
    if name == cfg.name {
        name = format!("{name}-client");
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
                c.trusted_clients.retain(|p| p.key != k && p.name != n);
                c.trusted_clients.push(Peer { name: n.clone(), key: k });
            });
            log::info!("paired with {name}");
        }
    }

    tx.send(&ToClient::Welcome { server_name: cfg.name.clone() })?;

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
        if let Some(st) = lk(&s.streams).remove(&old) {
            let _ = st.shutdown(Shutdown::Both);
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
    let mut sink: Option<files::Sink> = None;
    let result = loop {
        let msg = rx.recv::<ToServer>();
        if msg.is_ok() {
            last_seen.store(now_ms(), Ordering::Relaxed);
        }
        match msg {
            Ok(ToServer::Clipboard { text }) => lk(&s.core).client_clipboard(&name, Clip::Text { text }),
            Ok(ToServer::ClipboardImage { png }) => lk(&s.core).client_clipboard(&name, Clip::Image { png }),
            Ok(ToServer::FilesGrabbed { files }) => lk(&s.core).client_grab_result(&name, files),
            Ok(ToServer::File { m }) => receive_files(s, &mut sink, m),
            Ok(_) => {}
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

/// Stream files to a client in the background, between its input messages.
fn send_files(s: &Arc<Shared>, name: String, paths: Vec<String>) {
    let (bulk, me) = {
        let core = lk(&s.core);
        (core.clients.get(&name).map(|c| c.bulk.clone()), core.me.clone())
    };
    let Some(bulk) = bulk else { return };
    let app = s.app.clone();
    thread::spawn(move || {
        let paths: Vec<std::path::PathBuf> = paths.into_iter().map(Into::into).collect();
        app.set_status(format!("Sending files to {name}…"), true);
        match files::stream(&me, &paths, |m| bulk.push(ToClient::File { m })) {
            Ok(bytes) => {
                log::info!("sent {} to {name}", files::human(bytes));
                app.set_status(format!("Sent {} to {name}", files::human(bytes)), true);
            }
            Err(e) => {
                let _ = bulk.push(ToClient::File { m: files::FileMsg::Abort { reason: e.to_string() } });
                log::warn!("sending files to {name} failed: {e}");
                app.set_status(format!("Sending files to {name} failed: {e}"), false);
            }
        }
    });
}

/// Files dropped here from a client: copy them into the folder they were
/// dropped on (desktop or an Explorer window) and put them on the clipboard.
fn receive_files(s: &Arc<Shared>, sink: &mut Option<files::Sink>, m: files::FileMsg) {
    let app = &s.app;
    if let files::FileMsg::Begin { from, total, .. } = &m {
        app.set_status(format!("Receiving {} from {from}…", files::human(*total)), true);
    }
    match files::feed(sink, m) {
        Ok(Some(done)) => {
            let what = files::describe(&done.items, done.bytes);
            log::info!("received {what} from {}", done.from);
            let point = lk(&s.core).drop_point.take();
            let folder = point.and_then(|(x, y)| crate::platform::folder_at(x, y));
            let placed = folder.as_ref().map(|dir| (dir, files::copy_into(&done.items, dir)));
            match placed {
                Some((dir, Ok(paths))) => {
                    log::info!("dropped into {}", dir.display());
                    crate::clipboard::set_files(&paths);
                    app.set_status(format!("{what} from {} dropped into {}", done.from, dir.display()), true);
                }
                Some((dir, Err(e))) => {
                    log::warn!("couldn't copy into {}: {e}", dir.display());
                    crate::clipboard::set_files(&done.items);
                    app.set_status(format!("{what} from {} copied — paste with Ctrl+V", done.from), true);
                }
                None => {
                    crate::clipboard::set_files(&done.items);
                    app.set_status(
                        format!("{what} from {} copied — click a folder, then Ctrl+V", done.from),
                        true,
                    );
                }
            }
        }
        Ok(None) => {}
        Err(e) => {
            log::warn!("receiving files failed: {e}");
            app.set_status(format!("Receiving files failed: {e}"), false);
        }
    }
}

// ---------------------------------------------------------------------------
// Windows low-level hooks
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod hooks {
    use super::*;
    use crate::win32::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    static ACTIVE: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

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
    }

    fn shared() -> Option<Arc<Shared>> {
        lk(&ACTIVE).clone()
    }

    /// Fail-safe: the hook never waits long for the switching state. If it
    /// is stuck (a bug), input goes straight to Windows instead of freezing.
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
                        log::warn!("input hook: switching state busy >100 ms; passing input through");
                    }
                    return None;
                }
            }
        }
    }

    fn handle_mouse(msg: u32, ms: &MSLLHOOKSTRUCT) -> bool {
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
        core.on_key(kb.vkCode, kb.scanCode, kb.flags & LLKHF_EXTENDED != 0, down)
    }

    /// Tick of the last event our hooks saw (to notice if Windows drops them).
    static LAST_HOOK_TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    const WM_APP_REHOOK: u32 = 0x8000 + 1;

    /// Windows saw recent input that our hooks never got: they were removed.
    pub fn look_lost() -> bool {
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

    pub fn reinstall(thread_id: u32) {
        unsafe {
            PostThreadMessageW(thread_id, WM_APP_REHOOK, 0, 0);
        }
    }

    unsafe fn install() -> (HANDLE, HANDLE) {
        let hmod = GetModuleHandleW(std::ptr::null());
        (SetWindowsHookExW(WH_MOUSE_LL, mouse_proc, hmod, 0), SetWindowsHookExW(WH_KEYBOARD_LL, key_proc, hmod, 0))
    }

    unsafe extern "system" fn mouse_proc(code: i32, w: usize, l: isize) -> isize {
        LAST_HOOK_TICK.store(GetTickCount(), Ordering::Relaxed);
        if code >= 0 {
            let ms = &*(l as *const MSLLHOOKSTRUCT);
            if ms.flags & LLMHF_INJECTED == 0 {
                let block = catch_unwind(AssertUnwindSafe(|| handle_mouse(w as u32, ms))).unwrap_or(false);
                if block {
                    return 1;
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, w, l)
    }

    unsafe extern "system" fn key_proc(code: i32, w: usize, l: isize) -> isize {
        LAST_HOOK_TICK.store(GetTickCount(), Ordering::Relaxed);
        if code >= 0 {
            let kb = &*(l as *const KBDLLHOOKSTRUCT);
            if kb.flags & LLKHF_INJECTED == 0 {
                let block = catch_unwind(AssertUnwindSafe(|| handle_key(w as u32, kb))).unwrap_or(false);
                if block {
                    return 1;
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, w, l)
    }

    pub fn run(s: Arc<Shared>, ready: mpsc::Sender<u32>, drags: mpsc::Sender<Vec<String>>) {
        *lk(&ACTIVE) = Some(s);
        // The drop target lives on this thread: it needs the message loop below.
        crate::dragdrop::init(drags);
        unsafe {
            let (mut mh, mut kh) = install();
            if mh.is_null() || kh.is_null() {
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
            let _ = ready.send(GetCurrentThreadId());
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.message == WM_APP_REHOOK {
                    UnhookWindowsHookEx(mh);
                    UnhookWindowsHookEx(kh);
                    (mh, kh) = install();
                    if mh.is_null() || kh.is_null() {
                        log::error!("reinstalling input hooks failed");
                    }
                    continue;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            UnhookWindowsHookEx(mh);
            UnhookWindowsHookEx(kh);
        }
        *lk(&ACTIVE) = None;
        log::info!("input capture stopped");
    }

    pub fn stop(thread_id: u32) {
        unsafe {
            PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
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
