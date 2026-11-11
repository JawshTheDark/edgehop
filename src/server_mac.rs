//! macOS input capture for sharing this Mac's keyboard and mouse.
//!
//! A Quartz event tap sees every mouse and keyboard event before any app does
//! and can swallow it. While the mouse is on this Mac, events pass through and
//! the pointer's position decides when to cross an edge. While it's on another
//! computer, events are swallowed, the pointer stays parked (hidden) in the
//! middle of the main display, and the raw movement deltas drive the other
//! screen. Needs the Accessibility permission, like any Mac app that does this.
use super::*;
use std::ffi::c_void;

type CGEventRef = *mut c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type TapCallback = extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, callback: TapCallback, info: *mut c_void) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetLocation(e: CGEventRef) -> CGPoint;
    fn CGEventGetIntegerValueField(e: CGEventRef, field: u32) -> i64;
    fn CGEventGetDoubleValueField(e: CGEventRef, field: u32) -> f64;
    fn CGEventGetFlags(e: CGEventRef) -> u64;
    fn CGEventCreate(src: *const c_void) -> CGEventRef;
    fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
    fn CGAssociateMouseAndMouseCursorPosition(connected: i32) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayHideCursor(display: u32) -> i32;
    fn CGDisplayShowCursor(display: u32) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: CFMachPortRef, order: isize) -> *mut c_void;
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: *mut c_void, mode: *const c_void);
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: bool) -> i32;
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRelease(p: *const c_void);
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const std::ffi::c_char, encoding: u32) -> *const c_void;
    static kCFRunLoopCommonModes: *const c_void;
    static kCFRunLoopDefaultMode: *const c_void;
    static kCFBooleanTrue: *const c_void;
}

// Event types.
const LEFT_DOWN: u32 = 1;
const LEFT_UP: u32 = 2;
const RIGHT_DOWN: u32 = 3;
const RIGHT_UP: u32 = 4;
const MOVED: u32 = 5;
const LEFT_DRAGGED: u32 = 6;
const RIGHT_DRAGGED: u32 = 7;
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const SCROLL: u32 = 22;
const OTHER_DOWN: u32 = 25;
const OTHER_UP: u32 = 26;
const OTHER_DRAGGED: u32 = 27;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

// Event fields.
const MOUSE_BUTTON_NUMBER: u32 = 3;
const MOUSE_DELTA_X: u32 = 4;
const MOUSE_DELTA_Y: u32 = 5;
const KEYCODE: u32 = 9;
const SCROLL_FIXED_DELTA_Y: u32 = 93;
const SCROLL_FIXED_DELTA_X: u32 = 94;

const HID_TAP: u32 = 0;
const SESSION_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const TAP_DEFAULT: u32 = 0;

const CAPS_LOCK_KEYCODE: u16 = 0x39;

static ERROR: Mutex<String> = Mutex::new(String::new());
static REFUSED: AtomicBool = AtomicBool::new(false);

/// macOS refused the event tap the last time sharing started.
pub fn refused() -> bool {
    REFUSED.load(Ordering::Relaxed)
}

pub fn failure() -> String {
    let e = lk(&ERROR).clone();
    if e.is_empty() {
        "macOS didn't let Edgehop read the keyboard and mouse.".into()
    } else {
        e
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
}

const ACCESSIBILITY: &str = "System Settings > Privacy & Security > Accessibility \
     (\u{201c}Device Control and Data Access\u{201d} on newer macOS)";

/// macOS refused the event tap: work out which permission is missing, ask
/// for it, and say what to do.
fn explain_refusal() -> String {
    let trusted = crate::inject::permission() == Some(true);
    let (listen, post) = unsafe { (CGPreflightListenEventAccess(), CGPreflightPostEventAccess()) };
    log::error!(
        "macOS refused the input tap (Accessibility: {}, Input Monitoring: {}, posting events: {})",
        if trusted { "allowed" } else { "not allowed" },
        if listen { "allowed" } else { "not allowed" },
        if post { "allowed" } else { "not allowed" },
    );
    REFUSED.store(true, Ordering::Relaxed);
    let msg = if !trusted {
        crate::inject::permission_prompt();
        format!(
            "Allow Edgehop under {ACCESSIBILITY}, then press Start. Already allowed there? macOS is \
             holding a permission for an older copy: press Reset permissions on the Screens tab."
        )
    } else if !listen {
        unsafe { CGRequestListenEventAccess() };
        "Allow Edgehop under System Settings > Privacy & Security > Input Monitoring too, then restart Edgehop.".into()
    } else {
        "macOS lists Edgehop as allowed but still refuses it, which happens when the permission belongs \
         to an older copy: press Reset permissions on the Screens tab."
            .into()
    };
    log::error!("{msg}");
    msg
}

/// One input event, translated for the switching logic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// Pointer position (clamped to the screens) and the raw movement.
    Move { x: i32, y: i32, dx: i32, dy: i32 },
    Button { button: u8, down: bool },
    /// 1/120ths of a notch; positive dy scrolls up.
    Wheel { dx: i32, dy: i32 },
    /// Linux (evdev) key code.
    Key { code: u16, down: bool },
    /// A key with no equivalent elsewhere (e.g. Fn): swallowed while away.
    Other,
}

/// Receives each input event; returns true to swallow it.
pub type Sink = Box<dyn FnMut(Input) -> bool + Send>;

struct TapState {
    sink: Sink,
    tap: CFMachPortRef,
}

fn mask() -> u64 {
    [
        LEFT_DOWN, LEFT_UP, RIGHT_DOWN, RIGHT_UP, MOVED, LEFT_DRAGGED, RIGHT_DRAGGED, KEY_DOWN, KEY_UP, FLAGS_CHANGED,
        SCROLL, OTHER_DOWN, OTHER_UP, OTHER_DRAGGED,
    ]
    .iter()
    .fold(0u64, |m, t| m | (1u64 << t))
}

/// Device-specific flag bit for each modifier key (left/right told apart).
/// For each modifier key: (its own left/right bit, its family flag, both sides' bits).
fn modifier_bit(keycode: u16) -> Option<(u64, u64, u64)> {
    const SHIFT: u64 = 0x20000;
    const CTRL: u64 = 0x40000;
    const ALT: u64 = 0x80000;
    const CMD: u64 = 0x100000;
    Some(match keycode {
        0x38 => (0x02, SHIFT, 0x06),     // left shift
        0x3C => (0x04, SHIFT, 0x06),     // right shift
        0x3B => (0x01, CTRL, 0x2001),    // left control
        0x3E => (0x2000, CTRL, 0x2001),  // right control
        0x3A => (0x20, ALT, 0x60),       // left option
        0x3D => (0x40, ALT, 0x60),       // right option
        0x37 => (0x08, CMD, 0x18),       // left command
        0x36 => (0x10, CMD, 0x18),       // right command
        _ => return None,
    })
}

fn button_for(etype: u32, number: i64) -> u8 {
    match etype {
        LEFT_DOWN | LEFT_UP => BTN_LEFT,
        RIGHT_DOWN | RIGHT_UP => BTN_RIGHT,
        _ => match number {
            2 => BTN_MIDDLE,
            3 => BTN_BACK,
            4 => BTN_FORWARD,
            _ => BTN_MIDDLE,
        },
    }
}

/// Up to two inputs for one Quartz event (Caps Lock reports press+release at once).
fn translate(etype: u32, e: CGEventRef) -> [Option<Input>; 2] {
    unsafe {
        match etype {
            MOVED | LEFT_DRAGGED | RIGHT_DRAGGED | OTHER_DRAGGED => {
                let p = CGEventGetLocation(e);
                let dx = CGEventGetIntegerValueField(e, MOUSE_DELTA_X) as i32;
                let dy = CGEventGetIntegerValueField(e, MOUSE_DELTA_Y) as i32;
                [Some(Input::Move { x: p.x.floor() as i32, y: p.y.floor() as i32, dx, dy }), None]
            }
            LEFT_DOWN | RIGHT_DOWN | OTHER_DOWN | LEFT_UP | RIGHT_UP | OTHER_UP => {
                let number = CGEventGetIntegerValueField(e, MOUSE_BUTTON_NUMBER);
                let down = matches!(etype, LEFT_DOWN | RIGHT_DOWN | OTHER_DOWN);
                [Some(Input::Button { button: button_for(etype, number), down }), None]
            }
            SCROLL => {
                let dy = (CGEventGetDoubleValueField(e, SCROLL_FIXED_DELTA_Y) * 120.0).round() as i32;
                let dx = (CGEventGetDoubleValueField(e, SCROLL_FIXED_DELTA_X) * 120.0).round() as i32;
                if dx == 0 && dy == 0 {
                    [None, None]
                } else {
                    // Horizontal: positive is right everywhere else.
                    [Some(Input::Wheel { dx: -dx, dy }), None]
                }
            }
            KEY_DOWN | KEY_UP => {
                let k = CGEventGetIntegerValueField(e, KEYCODE) as u16;
                match crate::keymap::mac_to_evdev(k) {
                    Some(code) => [Some(Input::Key { code, down: etype == KEY_DOWN }), None],
                    None => [Some(Input::Other), None],
                }
            }
            FLAGS_CHANGED => {
                let k = CGEventGetIntegerValueField(e, KEYCODE) as u16;
                let Some(code) = crate::keymap::mac_to_evdev(k) else { return [Some(Input::Other), None] };
                if k == CAPS_LOCK_KEYCODE {
                    // macOS reports the lock toggling, not the key: tap it on the other side.
                    return [Some(Input::Key { code, down: true }), Some(Input::Key { code, down: false })];
                }
                match modifier_bit(k) {
                    Some((bit, family, both)) => {
                        let flags = CGEventGetFlags(e);
                        // Real keyboards set a left/right bit; synthetic events
                        // may only set the family flag.
                        let down = if flags & both != 0 { flags & bit != 0 } else { flags & family != 0 };
                        [Some(Input::Key { code, down }), None]
                    }
                    None => [Some(Input::Other), None],
                }
            }
            _ => [None, None],
        }
    }
}

/// Into the flight recorder: exactly what macOS reported, and whether we kept it.
fn record(input: Input, took: bool) {
    let t = took as i64;
    match input {
        Input::Move { x, y, dx, dy } => {
            crate::flight::rec("mac move", "x y os_dx os_dy took", &[x as i64, y as i64, dx as i64, dy as i64, t])
        }
        Input::Button { button, down } => crate::flight::rec("mac button", "button down took", &[button as i64, down as i64, t]),
        Input::Wheel { dx, dy } => crate::flight::rec("mac wheel", "dx dy took", &[dx as i64, dy as i64, t]),
        Input::Key { code, down } => crate::flight::rec("mac key", "code down took", &[code as i64, down as i64, t]),
        Input::Other => crate::flight::rec("mac other", "took", &[t]),
    }
}

extern "C" fn callback(_proxy: *mut c_void, etype: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef {
    let st = unsafe { &mut *(info as *mut TapState) };
    if etype == TAP_DISABLED_BY_TIMEOUT || etype == TAP_DISABLED_BY_USER_INPUT {
        log::warn!("macOS paused the input tap ({}); turning it back on", if etype == TAP_DISABLED_BY_TIMEOUT { "timeout" } else { "user input" });
        unsafe { CGEventTapEnable(st.tap, true) };
        return event;
    }
    let mut swallow = false;
    for input in translate(etype, event).into_iter().flatten() {
        let took = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (st.sink)(input))).unwrap_or(false);
        record(input, took);
        swallow |= took;
    }
    if swallow {
        std::ptr::null_mut()
    } else {
        event
    }
}

/// Install the event tap and deliver events to `sink` on this thread until
/// `stop` is set. Sends the run loop handle on `ready` (0 if macOS refused).
pub fn tap_loop(sink: Sink, ready: mpsc::Sender<usize>, stop: Arc<AtomicBool>) {
    unsafe {
        let state = Box::into_raw(Box::new(TapState { sink, tap: std::ptr::null_mut() }));
        let mut tap = CGEventTapCreate(HID_TAP, HEAD_INSERT, TAP_DEFAULT, mask(), callback, state as *mut c_void);
        if tap.is_null() {
            tap = CGEventTapCreate(SESSION_TAP, HEAD_INSERT, TAP_DEFAULT, mask(), callback, state as *mut c_void);
        }
        if tap.is_null() {
            drop(Box::from_raw(state));
            *lk(&ERROR) = explain_refusal();
            let _ = ready.send(0);
            return;
        }
        (*state).tap = tap;
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        let run_loop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        REFUSED.store(false, Ordering::Relaxed);
        log::info!("input capture running (Quartz event tap)");
        let _ = ready.send(run_loop as usize);
        while !stop.load(Ordering::Relaxed) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.5, false);
        }
        CGEventTapEnable(tap, false);
        CFMachPortInvalidate(tap);
        CFRelease(source);
        CFRelease(tap);
        drop(Box::from_raw(state));
    }
    log::info!("input capture stopped");
}

/// The server's input capture thread.
/// While the mouse is on another computer, the Mac's own (hidden) pointer
/// still moves. Movement for the other screen comes from macOS's per-event
/// deltas, which are exact as long as Edgehop isn't moving the pointer
/// itself. Only when the hidden pointer nears a screen edge (where it would
/// stop, and could trigger hot corners or the Dock) is it put back in the
/// middle; the event right after that is measured from where it was put. On
/// real hardware some macOS versions don't keep a pointer moved this way:
/// that's detected, logged as an error, and parking is turned off.
#[derive(Default)]
struct Parker {
    /// Where we last put the pointer, if the next event hasn't arrived yet.
    pending: Option<(i32, i32)>,
    /// Warps that didn't stick, in a row.
    failed: u32,
    /// Parking turned off: macOS doesn't keep the pointer where we put it.
    off: bool,
    was_away: bool,
}

/// Re-centre once the hidden pointer is this close to an edge of the main display.
const PARK_MARGIN: i32 = 250;

impl Parker {
    /// Movement for the other screen from this event.
    fn motion(&mut self, x: i32, y: i32, dx: i32, dy: i32) -> (i32, i32) {
        let Some((tx, ty)) = self.pending.take() else { return (dx, dy) };
        // macOS's delta for this event counts our warp as movement: measure
        // from where we put the pointer instead, if it stayed there.
        // Fixed slack: macOS's delta can't be trusted here (it may count the warp).
        const SLACK: i32 = 200;
        if (x - tx).abs() <= SLACK && (y - ty).abs() <= SLACK {
            self.failed = 0;
            crate::flight::rec("mac parked", "x y tx ty ok", &[x as i64, y as i64, tx as i64, ty as i64, 1]);
            return (x - tx, y - ty);
        }
        self.failed += 1;
        crate::flight::rec("mac parked", "x y tx ty ok", &[x as i64, y as i64, tx as i64, ty as i64, 0]);
        log::error!(
            "macOS didn't keep the pointer where Edgehop put it: put at ({tx}, {ty}), next event at ({x}, {y}) \
             with macOS delta ({dx}, {dy}) ({} in a row)",
            self.failed
        );
        if self.failed >= 3 && !self.off {
            self.off = true;
            log::error!(
                "macOS keeps ignoring Edgehop moving the pointer; it won't park the hidden pointer any more \
                 (movement still works; the hidden pointer may touch screen edges while you're on another computer)"
            );
            std::thread::spawn(|| crate::flight::log_dump("macOS isn't keeping the parked pointer"));
        }
        (0, 0)
    }

    /// Put the hidden pointer back in the middle if it's near an edge (or
    /// just left for another computer).
    fn maybe_park(&mut self, x: i32, y: i32, entering: bool) {
        if self.off {
            return;
        }
        let b = unsafe { CGDisplayBounds(CGMainDisplayID()) };
        let (l, t) = (b.origin.x as i32, b.origin.y as i32);
        let (r, bt) = (l + b.size.width as i32, t + b.size.height as i32);
        let near_edge = x < l + PARK_MARGIN || x >= r - PARK_MARGIN || y < t + PARK_MARGIN || y >= bt - PARK_MARGIN;
        if entering || near_edge {
            let (cx, cy) = main_display_center();
            if warp(cx, cy) {
                self.pending = Some((cx, cy));
            }
        }
    }
}

pub fn run(s: Arc<Shared>, ready: mpsc::Sender<usize>, _drags: mpsc::Sender<Vec<String>>) {
    let stop = s.stop.clone();
    let mut parker = Parker::default();
    let sink: Sink = Box::new(move |input| {
        if s.stop.load(Ordering::Relaxed) {
            return false;
        }
        let Some(mut core) = try_core(&s) else { return false };
        match input {
            Input::Move { x, y, dx, dy } => {
                let away = core.is_remote();
                if !away {
                    parker.was_away = false;
                    parker.pending = None;
                    let took = core.on_mouse_move(x, y);
                    if core.is_remote() {
                        // Just left for another computer from here.
                        parker.was_away = true;
                        parker.maybe_park(x, y, true);
                    }
                    return took;
                }
                let (mx, my) = parker.motion(x, y, dx, dy);
                let (cx, cy) = core.desk.center();
                crate::flight::rec("mac away", "x y mx my", &[x as i64, y as i64, mx as i64, my as i64]);
                let took = core.on_mouse_move(cx + mx, cy + my);
                if core.is_remote() {
                    parker.maybe_park(x, y, !parker.was_away);
                    parker.was_away = true;
                } else {
                    parker.was_away = false;
                    parker.pending = None;
                }
                took
            }
            Input::Button { button, down } => core.on_button(button, down),
            Input::Wheel { dx, dy } => core.on_wheel(dx, dy),
            Input::Key { code, down } => core.on_evdev_key(code, down),
            Input::Other => core.is_remote(),
        }
    });
    tap_loop(sink, ready, stop);
}

pub fn stop(run_loop: usize) {
    if run_loop != 0 {
        unsafe { CFRunLoopStop(run_loop as CFRunLoopRef) };
    }
}

/// macOS re-enables a paused tap through the callback; nothing to watch here.
pub fn look_lost() -> bool {
    false
}

pub fn reinstall(_run_loop: usize) {}

pub fn desktop() -> Box<dyn Desktop> {
    Box::new(MacDesktop { hidden: false })
}

pub struct MacDesktop {
    hidden: bool,
}

pub fn main_display_center() -> (i32, i32) {
    let b = unsafe { CGDisplayBounds(CGMainDisplayID()) };
    ((b.origin.x + b.size.width / 2.0) as i32, (b.origin.y + b.size.height / 2.0) as i32)
}

/// Move the pointer without generating events. Returns whether macOS says it
/// did; failures are logged (with macOS's error code) and recorded.
pub fn warp(x: i32, y: i32) -> bool {
    no_pause_after_warp();
    let err = unsafe { CGWarpMouseCursorPosition(CGPoint { x: x as f64 + 0.5, y: y as f64 + 0.5 }) };
    let now = pointer();
    crate::flight::rec("mac warp", "x y err at_x at_y", &[x as i64, y as i64, err as i64, now.0 as i64, now.1 as i64]);
    if err != 0 {
        log::error!("macOS refused to move the pointer to ({x}, {y}): CGWarpMouseCursorPosition error {err}");
        return false;
    }
    if (now.0 - x).abs() > 2 || (now.1 - y).abs() > 2 {
        log::error!("asked macOS to move the pointer to ({x}, {y}); it's at ({}, {})", now.0, now.1);
        return false;
    }
    true
}

/// macOS normally ignores the real mouse for a quarter of a second after a
/// warp. Turn that off once (the old global switch, looked up at run time;
/// otherwise the per-warp re-association trick).
fn no_pause_after_warp() {
    static DONE: std::sync::Once = std::sync::Once::new();
    static FALLBACK: AtomicBool = AtomicBool::new(false);
    DONE.call_once(|| unsafe {
        let f = libc::dlsym(libc::RTLD_DEFAULT, c"CGSetLocalEventsSuppressionInterval".as_ptr());
        if f.is_null() {
            log::warn!("CGSetLocalEventsSuppressionInterval isn't available; re-associating after each warp instead");
            FALLBACK.store(true, Ordering::Relaxed);
        } else {
            let f: extern "C" fn(f64) -> i32 = std::mem::transmute(f);
            let err = f(0.0);
            log::info!("pointer pause after warps turned off (CGSetLocalEventsSuppressionInterval -> {err})");
        }
    });
    if FALLBACK.load(Ordering::Relaxed) {
        let err = unsafe { CGAssociateMouseAndMouseCursorPosition(1) };
        if err != 0 {
            log::warn!("CGAssociateMouseAndMouseCursorPosition error {err}");
        }
    }
}

pub fn pointer() -> (i32, i32) {
    unsafe {
        let e = CGEventCreate(std::ptr::null());
        if e.is_null() {
            return main_display_center();
        }
        let p = CGEventGetLocation(e);
        CFRelease(e as *const c_void);
        (p.x.floor() as i32, p.y.floor() as i32)
    }
}

/// macOS only lets the frontmost app hide the pointer, unless the window
/// server is told otherwise (the same private switch Synergy and Barrier use).
fn allow_hiding_in_background() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let conn = libc::dlsym(libc::RTLD_DEFAULT, c"_CGSDefaultConnection".as_ptr());
        let set = libc::dlsym(libc::RTLD_DEFAULT, c"CGSSetConnectionProperty".as_ptr());
        if conn.is_null() || set.is_null() {
            log::warn!("can't hide the pointer while another app is in front (window server switch not found)");
            return;
        }
        let conn: extern "C" fn() -> i32 = std::mem::transmute(conn);
        let set: extern "C" fn(i32, i32, *const c_void, *const c_void) -> i32 = std::mem::transmute(set);
        let key = CFStringCreateWithCString(std::ptr::null(), c"SetsCursorInBackground".as_ptr(), 0x0800_0100);
        if !key.is_null() {
            let cid = conn();
            let r = set(cid, cid, key, kCFBooleanTrue);
            log::debug!("SetsCursorInBackground -> {r}");
            CFRelease(key);
        }
    });
}

pub fn set_pointer_visible(visible: bool) {
    allow_hiding_in_background();
    let err = unsafe {
        if visible {
            CGDisplayShowCursor(CGMainDisplayID())
        } else {
            CGDisplayHideCursor(CGMainDisplayID())
        }
    };
    crate::flight::rec("mac cursor", "visible err", &[visible as i64, err as i64]);
    if err != 0 {
        log::error!("macOS refused to {} the pointer: error {err}", if visible { "show" } else { "hide" });
    }
}

impl Desktop for MacDesktop {
    fn monitors(&self) -> Vec<Monitor> {
        crate::platform::monitor_rects()
    }
    fn center(&self) -> (i32, i32) {
        main_display_center()
    }
    fn set_cursor(&mut self, x: i32, y: i32) {
        // Parking at the centre while away is decided by the Parker above,
        // from the events themselves; this only places the pointer when the
        // mouse comes back.
        if (x, y) == main_display_center() || pointer() == (x, y) {
            return;
        }
        warp(x, y);
    }
    fn cursor(&self) -> (i32, i32) {
        pointer()
    }
    fn set_cursor_visible(&mut self, visible: bool) {
        if visible == self.hidden {
            set_pointer_visible(visible);
            self.hidden = !visible;
        }
    }
}

impl Drop for MacDesktop {
    fn drop(&mut self) {
        if self.hidden {
            set_pointer_visible(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Parker;

    #[test]
    fn movement_comes_from_macos_deltas_until_the_pointer_is_parked() {
        let mut p = Parker::default();
        assert_eq!(p.motion(900, 500, 3, -2), (3, -2));
        // Parked at the centre: the next event's delta counts the warp, so
        // measure from where the pointer was put instead.
        p.pending = Some((1720, 720));
        assert_eq!(p.motion(1725, 718, 1930, 36), (5, -2));
        assert_eq!(p.motion(1730, 718, 5, 0), (5, 0), "back to deltas after that");
    }

    #[test]
    fn parking_turns_off_when_macos_ignores_it() {
        let mut p = Parker::default();
        for _ in 0..3 {
            p.pending = Some((1720, 720));
            // The hidden pointer is still out by the edge: the warp didn't stick.
            assert_eq!(p.motion(3439, 600, 2000, -100), (0, 0), "a bogus jump is dropped");
        }
        assert!(p.off, "after three misses Edgehop stops parking");
        assert_eq!(p.motion(3439, 610, 0, 10), (0, 10), "and keeps using macOS's deltas");
    }
}
