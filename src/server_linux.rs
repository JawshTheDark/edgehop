//! Linux input capture for sharing this computer's keyboard and mouse.
//!
//! Keyboards, mice and touchpads are read straight from the kernel
//! (/dev/input/event*), which works the same on X11 and Wayland. While the
//! mouse is on this computer the devices are only watched: the desktop keeps
//! them, and the pointer's position (asked from X11, or reported by a small
//! KWin script on KDE Plasma) decides when to cross an edge. While it's on
//! another computer Edgehop grabs the devices, so nothing reaches this
//! desktop, and forwards everything. Coming back, the pointer is placed with
//! Edgehop's virtual (uinput) pointer, which works on Wayland too.
use super::*;
use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CString};
use std::path::PathBuf;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const REL_WHEEL_HI_RES: u16 = 11;
const REL_HWHEEL_HI_RES: u16 = 12;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const KEY_A: u16 = 30;
const KEY_Z: u16 = 44;
const BTN_LEFT_CODE: u16 = 0x110;
const BTN_TOOL_FINGER: u16 = 0x145;
const BTN_TOOL_QUINTTAP: u16 = 0x148;
const BTN_TOUCH: u16 = 0x14a;
const BTN_TOOL_DOUBLETAP: u16 = 0x14d;
const BTN_TOOL_TRIPLETAP: u16 = 0x14e;
const BTN_TOOL_QUADTAP: u16 = 0x14f;
const INPUT_PROP_POINTER: u16 = 0;
const INPUT_PROP_DIRECT: u16 = 1;

/// Touchpad: pixels moved per millimetre of finger travel.
const TOUCHPAD_PX_PER_MM: f64 = 10.0;
/// Two-finger scrolling: wheel units (1/120 notch) per pixel of travel.
const TOUCHPAD_SCROLL: f64 = 2.0;

fn ioc(dir: u64, nr: u64, size: u64) -> c_ulong {
    ((dir << 30) | (size << 16) | (0x45 << 8) | nr) as c_ulong
}
const IOC_READ: u64 = 2;
const IOC_WRITE: u64 = 1;
fn eviocgname(len: usize) -> c_ulong {
    ioc(IOC_READ, 0x06, len as u64)
}
fn eviocgprop(len: usize) -> c_ulong {
    ioc(IOC_READ, 0x09, len as u64)
}
fn eviocgkey(len: usize) -> c_ulong {
    ioc(IOC_READ, 0x18, len as u64)
}
fn eviocgbit(ev: u16, len: usize) -> c_ulong {
    ioc(IOC_READ, 0x20 + ev as u64, len as u64)
}
fn eviocgabs(abs: u16) -> c_ulong {
    ioc(IOC_READ, 0x40 + abs as u64, 24)
}
fn eviocgrab() -> c_ulong {
    ioc(IOC_WRITE, 0x90, 4)
}

/// One input event, translated for the switching logic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// Raw movement (mice: device counts; touchpads: pixels).
    Move { dx: i32, dy: i32 },
    Button { button: u8, down: bool },
    /// 1/120ths of a notch; positive dy scrolls up.
    Wheel { dx: i32, dy: i32 },
    Key { code: u16, down: bool },
}

#[derive(Default)]
struct DevState {
    dx: i32,
    dy: i32,
    wx: i32,
    wy: i32,
    // touchpad
    abs: (Option<i32>, Option<i32>),
    last: Option<(i32, i32)>,
    last_fingers: u8,
    tools: u8,
    touching: bool,
    rem: (f64, f64),
}

impl DevState {
    fn fingers(&self) -> u8 {
        (0..8).rev().find(|b| self.tools & (1 << b) != 0).map_or(0, |b| b + 1)
    }
}

struct Dev {
    path: PathBuf,
    fd: c_int,
    name: String,
    keyboard: bool,
    pointer: bool,
    touchpad: bool,
    hires: (bool, bool),
    /// Touchpad: pixels per device unit.
    scale: (f64, f64),
    st: DevState,
}

impl Drop for Dev {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

fn bits(fd: c_int, req: c_ulong, len: usize) -> Vec<u8> {
    let mut b = vec![0u8; len];
    unsafe { libc::ioctl(fd, req as _, b.as_mut_ptr()) };
    b
}

fn has(b: &[u8], bit: u16) -> bool {
    b.get(bit as usize / 8).is_some_and(|v| v & (1 << (bit % 8)) != 0)
}

#[repr(C)]
#[derive(Default)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

fn px_per_unit(fd: c_int, axis: u16) -> f64 {
    let mut a = AbsInfo::default();
    unsafe { libc::ioctl(fd, eviocgabs(axis) as _, &mut a as *mut AbsInfo) };
    // Units per millimetre; without it, guess the pad is 100 mm wide.
    let per_mm = if a.resolution > 0 { a.resolution as f64 } else { ((a.maximum - a.minimum).max(1) as f64) / 100.0 };
    TOUCHPAD_PX_PER_MM / per_mm
}

impl Dev {
    /// Open an event device; None if it isn't a keyboard, mouse or touchpad
    /// (or is one of Edgehop's own virtual devices).
    fn open(path: &std::path::Path) -> Result<Option<Dev>, std::io::Error> {
        let c = CString::new(path.as_os_str().as_encoded_bytes()).map_err(std::io::Error::other)?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut name = [0u8; 256];
        unsafe { libc::ioctl(fd, eviocgname(name.len()) as _, name.as_mut_ptr()) };
        let name = String::from_utf8_lossy(&name[..name.iter().position(|&b| b == 0).unwrap_or(0)]).into_owned();
        let ev = bits(fd, eviocgbit(0, 4), 4);
        let keys = bits(fd, eviocgbit(EV_KEY, 96), 96);
        let rel = bits(fd, eviocgbit(EV_REL, 4), 4);
        let abs = bits(fd, eviocgbit(EV_ABS, 8), 8);
        let props = bits(fd, eviocgprop(4), 4);
        let keyboard = has(&ev, EV_KEY) && (KEY_A..=KEY_Z).all(|k| has(&keys, k));
        let pointer = has(&ev, EV_REL) && has(&rel, REL_X) && has(&rel, REL_Y);
        let touchpad = has(&ev, EV_ABS)
            && has(&abs, ABS_X)
            && has(&abs, ABS_Y)
            && has(&keys, BTN_TOOL_FINGER)
            && !has(&props, INPUT_PROP_DIRECT)
            && (has(&props, INPUT_PROP_POINTER) || has(&keys, BTN_LEFT_CODE));
        let ours = name.to_lowercase().starts_with("edgehop");
        let mut dev = Dev {
            path: path.to_path_buf(),
            fd,
            name,
            keyboard,
            pointer,
            touchpad,
            hires: (has(&rel, REL_WHEEL_HI_RES), has(&rel, REL_HWHEEL_HI_RES)),
            scale: (1.0, 1.0),
            st: DevState::default(),
        };
        if ours || !(keyboard || pointer || touchpad) {
            return Ok(None); // dropped: closes the fd
        }
        if touchpad {
            dev.scale = (px_per_unit(fd, ABS_X), px_per_unit(fd, ABS_Y));
        }
        Ok(Some(dev))
    }

    fn kind(&self) -> String {
        [(self.keyboard, "keyboard"), (self.pointer, "mouse"), (self.touchpad, "touchpad")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, k)| *k)
            .collect::<Vec<_>>()
            .join("+")
    }

    fn set_grab(&self, on: bool) -> bool {
        unsafe { libc::ioctl(self.fd, eviocgrab() as _, on as c_int) == 0 }
    }

    /// Keys and buttons held down right now.
    fn held(&self) -> Vec<u16> {
        let b = bits(self.fd, eviocgkey(96), 96);
        (1..0x2ffu16).filter(|&k| has(&b, k)).collect()
    }

    fn event(&mut self, t: u16, code: u16, value: i32, out: &mut Vec<Input>) {
        if (t, code) == (EV_SYN, SYN_REPORT) {
            return self.flush(out);
        }
        let st = &mut self.st;
        match (t, code) {
            (EV_SYN, SYN_DROPPED) => {
                st.last = None;
                st.dx = 0;
                st.dy = 0;
            }
            (EV_KEY, c) if c < 0x100 => out.push(Input::Key { code: c, down: value != 0 }),
            (EV_KEY, c) if (BTN_LEFT_CODE..BTN_LEFT_CODE + 5).contains(&c) && value != 2 => {
                let button = match c - BTN_LEFT_CODE {
                    0 => BTN_LEFT,
                    1 => BTN_RIGHT,
                    2 => BTN_MIDDLE,
                    3 => BTN_BACK,
                    _ => BTN_FORWARD,
                };
                out.push(Input::Button { button, down: value != 0 });
            }
            (EV_KEY, BTN_TOUCH) => st.touching = value != 0,
            (EV_KEY, c @ (BTN_TOOL_FINGER | BTN_TOOL_DOUBLETAP | BTN_TOOL_TRIPLETAP | BTN_TOOL_QUADTAP | BTN_TOOL_QUINTTAP)) => {
                let n = match c {
                    BTN_TOOL_FINGER => 0,
                    BTN_TOOL_DOUBLETAP => 1,
                    BTN_TOOL_TRIPLETAP => 2,
                    BTN_TOOL_QUADTAP => 3,
                    _ => 4,
                };
                if value != 0 {
                    st.tools |= 1 << n;
                } else {
                    st.tools &= !(1 << n);
                }
            }
            (EV_REL, REL_X) => st.dx += value,
            (EV_REL, REL_Y) => st.dy += value,
            (EV_REL, REL_WHEEL_HI_RES) => st.wy += value,
            (EV_REL, REL_HWHEEL_HI_RES) => st.wx += value,
            (EV_REL, REL_WHEEL) if !self.hires.0 => st.wy += value * 120,
            (EV_REL, REL_HWHEEL) if !self.hires.1 => st.wx += value * 120,
            (EV_ABS, ABS_X) if self.touchpad => st.abs.0 = Some(value),
            (EV_ABS, ABS_Y) if self.touchpad => st.abs.1 = Some(value),
            _ => {}
        }
    }

    fn flush(&mut self, out: &mut Vec<Input>) {
        let st = &mut self.st;
        if st.dx != 0 || st.dy != 0 {
            out.push(Input::Move { dx: st.dx, dy: st.dy });
            st.dx = 0;
            st.dy = 0;
        }
        if st.wx != 0 || st.wy != 0 {
            out.push(Input::Wheel { dx: st.wx, dy: st.wy });
            st.wx = 0;
            st.wy = 0;
        }
        if !self.touchpad {
            return;
        }
        let fingers = st.fingers();
        if !st.touching || fingers == 0 {
            st.last = None;
            st.last_fingers = 0;
            st.rem = (0.0, 0.0);
            return;
        }
        let (Some(x), Some(y)) = st.abs else { return };
        if st.last_fingers == fingers {
            if let Some((lx, ly)) = st.last {
                let px = ((x - lx) as f64 * self.scale.0, (y - ly) as f64 * self.scale.1);
                // One finger moves the pointer; two scroll, content following
                // the fingers ("natural" scrolling).
                let (fx, fy) = match fingers {
                    1 => (px.0 + st.rem.0, px.1 + st.rem.1),
                    2 => (-px.0 * TOUCHPAD_SCROLL + st.rem.0, px.1 * TOUCHPAD_SCROLL + st.rem.1),
                    _ => (0.0, 0.0),
                };
                let (dx, dy) = (fx.trunc() as i32, fy.trunc() as i32);
                st.rem = (fx - dx as f64, fy - dy as f64);
                if dx != 0 || dy != 0 {
                    out.push(if fingers == 1 { Input::Move { dx, dy } } else { Input::Wheel { dx, dy } });
                }
            }
        } else {
            st.rem = (0.0, 0.0);
        }
        st.last = Some((x, y));
        st.last_fingers = fingers;
    }
}

/// The keyboards, mice and touchpads Edgehop watches (and grabs while away).
pub struct Devices {
    list: Mutex<Vec<Dev>>,
    skip: Mutex<HashSet<PathBuf>>,
    grabbed: AtomicBool,
    denied: AtomicU64,
}

impl Devices {
    pub fn new() -> Devices {
        Devices {
            list: Mutex::new(Vec::new()),
            skip: Mutex::new(HashSet::new()),
            grabbed: AtomicBool::new(false),
            denied: AtomicU64::new(0),
        }
    }

    /// Open devices that appeared since the last scan. Returns how many are in use.
    pub fn scan(&self) -> usize {
        let Ok(dir) = std::fs::read_dir("/dev/input") else { return 0 };
        let present: HashSet<PathBuf> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("event")))
            .collect();
        let mut list = lk(&self.list);
        let mut skip = lk(&self.skip);
        list.retain(|d| present.contains(&d.path));
        skip.retain(|p| present.contains(p));
        let mut denied = 0;
        for p in &present {
            if list.iter().any(|d| &d.path == p) || skip.contains(p) {
                continue;
            }
            match Dev::open(p) {
                Ok(Some(d)) => {
                    log::info!("input device {}: {:?} ({})", p.display(), d.name, d.kind());
                    if self.grabbed.load(Ordering::Relaxed) {
                        d.set_grab(true);
                    }
                    list.push(d);
                }
                Ok(None) => {
                    skip.insert(p.clone());
                }
                Err(e) => {
                    if e.raw_os_error() == Some(libc::EACCES) {
                        denied += 1;
                    } else {
                        skip.insert(p.clone());
                    }
                }
            }
        }
        self.denied.store(denied, Ordering::Relaxed);
        list.len()
    }

    /// Take the devices away from the desktop (true) or give them back.
    /// Keys and buttons held right now are released on the desktop first
    /// (through `out`), or it would see them stuck down until pressed again.
    pub fn grab(&self, on: bool, out: Option<&mut Box<dyn crate::inject::Injector>>) {
        let list = lk(&self.list);
        if self.grabbed.swap(on, Ordering::Relaxed) == on {
            return;
        }
        if on {
            if let Some(out) = out {
                for d in list.iter() {
                    for k in d.held() {
                        if (1..0x100).contains(&k) {
                            out.key(k, false);
                        } else if (BTN_LEFT_CODE..BTN_LEFT_CODE + 3).contains(&k) {
                            out.button([BTN_LEFT, BTN_RIGHT, BTN_MIDDLE][(k - BTN_LEFT_CODE) as usize], false);
                        }
                    }
                }
            }
        }
        let failed: Vec<&str> = list.iter().filter(|d| !d.set_grab(on)).map(|d| d.name.as_str()).collect();
        if !failed.is_empty() {
            log::warn!("couldn't {} {}", if on { "grab" } else { "release" }, failed.join(", "));
        }
        log::debug!("input devices {}", if on { "grabbed" } else { "released" });
    }

    /// Wait up to `timeout_ms` for input; returns what arrived.
    pub fn poll(&self, timeout_ms: i32) -> Vec<Input> {
        let mut fds: Vec<libc::pollfd> =
            lk(&self.list).iter().map(|d| libc::pollfd { fd: d.fd, events: libc::POLLIN, revents: 0 }).collect();
        if fds.is_empty() {
            thread::sleep(Duration::from_millis(timeout_ms as u64));
            return Vec::new();
        }
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        let mut out = Vec::new();
        if n <= 0 {
            return out;
        }
        let mut list = lk(&self.list);
        let mut gone = Vec::new();
        for p in fds.iter().filter(|p| p.revents != 0) {
            let Some(d) = list.iter_mut().find(|d| d.fd == p.fd) else { continue };
            let mut buf: [libc::input_event; 64] = unsafe { std::mem::zeroed() };
            loop {
                let size = std::mem::size_of::<libc::input_event>();
                let r = unsafe { libc::read(d.fd, buf.as_mut_ptr() as *mut c_void, size * buf.len()) };
                if r <= 0 {
                    let e = std::io::Error::last_os_error();
                    if r == 0 || e.raw_os_error() != Some(libc::EAGAIN) {
                        log::info!("input device {:?} went away", d.name);
                        gone.push(d.fd);
                    }
                    break;
                }
                for ev in &buf[..r as usize / size] {
                    d.event(ev.type_, ev.code, ev.value, &mut out);
                }
            }
        }
        list.retain(|d| !gone.contains(&d.fd));
        out
    }
}

// ---------------------------------------------------------------------------
// Where the pointer is while it's on this computer
// ---------------------------------------------------------------------------

type XOpenDisplay = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type XDefaultRootWindow = unsafe extern "C" fn(*mut c_void) -> c_ulong;
type XQueryPointer = unsafe extern "C" fn(
    *mut c_void,
    c_ulong,
    *mut c_ulong,
    *mut c_ulong,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_uint,
) -> c_int;

/// X11: ask the X server where the pointer is (libX11 loaded at run time).
struct X11 {
    dpy: *mut c_void,
    root: c_ulong,
    query: XQueryPointer,
}

impl X11 {
    fn open() -> Option<X11> {
        unsafe {
            let lib = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW);
            if lib.is_null() {
                return None;
            }
            let open = libc::dlsym(lib, c"XOpenDisplay".as_ptr());
            let root = libc::dlsym(lib, c"XDefaultRootWindow".as_ptr());
            let query = libc::dlsym(lib, c"XQueryPointer".as_ptr());
            if open.is_null() || root.is_null() || query.is_null() {
                return None;
            }
            let open: XOpenDisplay = std::mem::transmute(open);
            let root: XDefaultRootWindow = std::mem::transmute(root);
            let dpy = open(std::ptr::null());
            if dpy.is_null() {
                return None;
            }
            Some(X11 { dpy, root: root(dpy), query: std::mem::transmute(query) })
        }
    }

    fn pointer(&self) -> Option<(i32, i32)> {
        let (mut r, mut c, mut x, mut y, mut wx, mut wy, mut m) = (0, 0, 0, 0, 0, 0, 0);
        let ok = unsafe { (self.query)(self.dpy, self.root, &mut r, &mut c, &mut x, &mut y, &mut wx, &mut wy, &mut m) };
        (ok != 0).then_some((x, y))
    }
}

enum Source {
    X11(X11),
    /// KWin reports positions at screen edges over D-Bus.
    KWin,
}

fn session() -> &'static str {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        "wayland"
    } else if std::env::var_os("DISPLAY").is_some() {
        "x11"
    } else {
        "none"
    }
}

fn is_kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase().contains("KDE")
}

// ---------------------------------------------------------------------------
// The server's side
// ---------------------------------------------------------------------------

/// State shared by the capture thread and the desktop handle the switching logic owns.
struct Capture {
    devs: Devices,
    /// Edgehop's virtual keyboard + absolute pointer: places the pointer when
    /// the mouse comes back, and releases keys held while leaving.
    out: Mutex<Option<Box<dyn crate::inject::Injector>>>,
    kde: Option<Arc<crate::kde::Kde>>,
    /// Where we last put the pointer, and when: KWin reports that spot back,
    /// and it isn't the user pushing against the edge.
    warped: Mutex<Option<((i32, i32), Instant)>>,
    pointer_tx: Mutex<mpsc::Sender<bool>>,
}

static CURRENT: Mutex<Option<Arc<Capture>>> = Mutex::new(None);
static ERROR: Mutex<String> = Mutex::new(String::new());

pub fn failure() -> String {
    let e = lk(&ERROR).clone();
    if e.is_empty() {
        "Input capture failed to start".into()
    } else {
        e
    }
}

fn fail(ready: &mpsc::Sender<usize>, msg: String) {
    log::error!("{msg}");
    *lk(&ERROR) = msg;
    let _ = ready.send(0);
}

pub fn desktop() -> Box<dyn Desktop> {
    let kde = if is_kde() { crate::kde::Kde::connect().map(Arc::new) } else { None };
    let out = match crate::inject::create() {
        Ok(o) => Some(o),
        Err(e) => {
            log::warn!("no virtual pointer, so the pointer can't be placed on return: {e}");
            None
        }
    };
    // Hiding the pointer talks to KWin: keep it off the input path.
    let (tx, rx) = mpsc::channel::<bool>();
    let k = kde.clone();
    let _ = thread::Builder::new().name("pointer-vis".into()).spawn(move || {
        for visible in rx {
            if let Some(k) = &k {
                if visible {
                    k.back();
                } else {
                    k.away(true, None);
                }
            }
        }
    });
    let cap = Arc::new(Capture {
        devs: Devices::new(),
        out: Mutex::new(out),
        kde,
        warped: Mutex::new(None),
        pointer_tx: Mutex::new(tx),
    });
    *lk(&CURRENT) = Some(cap.clone());
    Box::new(LinuxDesktop { cap, hidden: false })
}

fn dispatch(s: &Shared, input: Input, src: &Source) {
    if s.stop.load(Ordering::Relaxed) {
        return;
    }
    // Ask X where the pointer is before locking (only needed on this computer).
    let local = match (input, src) {
        (Input::Move { .. }, Source::X11(x)) => x.pointer(),
        _ => None,
    };
    let Some(mut core) = try_core(s) else { return };
    match input {
        Input::Move { dx, dy } => {
            if core.is_remote() {
                let (cx, cy) = core.desk.center();
                core.on_mouse_move(cx + dx, cy + dy);
            } else if let Some((x, y)) = local {
                core.on_mouse_move(x, y);
            }
        }
        Input::Button { button, down } => {
            core.on_button(button, down);
        }
        Input::Wheel { dx, dy } => {
            core.on_wheel(dx, dy);
        }
        Input::Key { code, down } => {
            core.on_evdev_key(code, down);
        }
    }
}

pub fn run(s: Arc<Shared>, ready: mpsc::Sender<usize>, _drags: mpsc::Sender<Vec<String>>) {
    let Some(cap) = lk(&CURRENT).clone() else {
        return fail(&ready, "Input capture wasn't set up".into());
    };
    if cap.devs.scan() == 0 {
        let denied = cap.devs.denied.load(Ordering::Relaxed);
        return fail(
            &ready,
            if denied > 0 {
                "Edgehop isn't allowed to read the keyboard and mouse. Install the edgehop package \
                 (it allows this for the logged-in user), then log out and back in."
                    .into()
            } else {
                "No keyboard, mouse or touchpad found to share.".into()
            },
        );
    }
    let src = match session() {
        "x11" => match X11::open() {
            Some(x) => Source::X11(x),
            None => return fail(&ready, "Can't talk to the X server (libX11) to follow the pointer.".into()),
        },
        "wayland" => match &cap.kde {
            Some(k) => {
                let s2 = s.clone();
                let c2 = cap.clone();
                let first = AtomicBool::new(true);
                let ok = k.report_pointer(Some(Box::new(move |x, y| {
                    if first.swap(false, Ordering::Relaxed) {
                        log::info!("KWin is reporting the pointer at screen edges");
                    }
                    if s2.stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let just_placed = lk(&c2.warped)
                        .is_some_and(|(p, at)| at.elapsed() < Duration::from_millis(1500) && (p.0 - x).abs() <= 2 && (p.1 - y).abs() <= 2);
                    if just_placed {
                        return;
                    }
                    if let Some(mut core) = try_core(&s2) {
                        if !core.is_remote() {
                            core.on_mouse_move(x, y);
                        }
                    }
                })));
                if !ok {
                    return fail(&ready, "KWin didn't accept Edgehop's pointer script.".into());
                }
                Source::KWin
            }
            None => {
                return fail(
                    &ready,
                    "Sharing from Wayland works on KDE Plasma. On GNOME, choose \u{201c}GNOME on Xorg\u{201d} at the \
                     login screen to share from this computer."
                        .into(),
                )
            }
        },
        _ => return fail(&ready, "No graphical session found (DISPLAY / WAYLAND_DISPLAY).".into()),
    };
    log::info!(
        "input capture running ({} session, pointer from {})",
        session(),
        if matches!(src, Source::X11(_)) { "X11" } else { "KWin" }
    );
    let _ = ready.send(1);
    let mut last_scan = Instant::now();
    while !s.stop.load(Ordering::Relaxed) {
        for input in cap.devs.poll(200) {
            recovery::guard("input capture", || dispatch(&s, input, &src));
        }
        if last_scan.elapsed() > Duration::from_secs(2) {
            cap.devs.scan();
            last_scan = Instant::now();
        }
    }
    cap.devs.grab(false, None);
    if let (Source::KWin, Some(k)) = (&src, &cap.kde) {
        k.report_pointer(None);
    }
    log::info!("input capture stopped");
}

/// The capture loop watches the engine's stop flag.
pub fn stop(_handle: usize) {}

pub fn look_lost() -> bool {
    false
}

pub fn reinstall(_handle: usize) {}

pub struct LinuxDesktop {
    cap: Arc<Capture>,
    hidden: bool,
}

impl Desktop for LinuxDesktop {
    fn monitors(&self) -> Vec<Monitor> {
        crate::platform::monitor_rects()
    }
    fn center(&self) -> (i32, i32) {
        let mons = crate::platform::monitor_rects();
        mons.iter()
            .find(|m| m.primary)
            .or(mons.first())
            .map(|m| {
                let (x, y) = m.rect.center();
                (x as i32, y as i32)
            })
            .unwrap_or((960, 540))
    }
    fn set_cursor(&mut self, x: i32, y: i32) {
        // While away the devices are grabbed and the pointer can't move: no parking needed.
        if self.cap.devs.grabbed.load(Ordering::Relaxed) || (x, y) == self.center() {
            return;
        }
        if let Some(out) = lk(&self.cap.out).as_mut() {
            out.move_to(x, y);
            *lk(&self.cap.warped) = Some(((x, y), Instant::now()));
        }
    }
    fn set_cursor_visible(&mut self, visible: bool) {
        if visible == self.hidden {
            self.hidden = !visible;
            let _ = lk(&self.cap.pointer_tx).send(visible);
        }
    }
    fn captured(&mut self, remote: bool) {
        let mut out = lk(&self.cap.out);
        self.cap.devs.grab(remote, out.as_mut());
    }
}

impl Drop for LinuxDesktop {
    fn drop(&mut self) {
        self.cap.devs.grab(false, None);
        if self.hidden {
            let _ = lk(&self.cap.pointer_tx).send(true);
        }
    }
}

// ---------------------------------------------------------------------------
// Self-test support: read a device the test creates.
// ---------------------------------------------------------------------------

/// Watch input devices like the server does (for `edgehop --self-test`).
pub fn test_devices() -> Devices {
    Devices::new()
}

impl Devices {
    /// Names and kinds of the devices in use.
    pub fn describe(&self) -> Vec<String> {
        lk(&self.list).iter().map(|d| format!("{} ({})", d.name, d.kind())).collect()
    }
    /// Grab or release only devices whose name starts with `prefix`; true if all succeeded.
    pub fn grab_named(&self, prefix: &str, on: bool) -> bool {
        lk(&self.list).iter().filter(|d| d.name.starts_with(prefix)).all(|d| d.set_grab(on))
    }
}
