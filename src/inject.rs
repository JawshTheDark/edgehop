//! Replaying input on the client: uinput (Linux), SendInput (Windows), Quartz (macOS).

pub trait Injector: Send {
    /// Logical size of the desktop in pixels, reported to the server.
    fn screen_size(&self) -> (i32, i32);
    fn move_to(&mut self, x: i32, y: i32);
    fn button(&mut self, button: u8, down: bool);
    /// Units of 1/120 notch; positive dy = away from the user (scroll up).
    fn wheel(&mut self, dx: i32, dy: i32);
    fn key(&mut self, evdev_code: u16, down: bool);
}

pub fn create() -> Result<Box<dyn Injector>, String> {
    #[cfg(target_os = "linux")]
    return linux::Uinput::new().map(|i| Box::new(i) as Box<dyn Injector>);
    #[cfg(windows)]
    return Ok(Box::new(windows::SendInputInjector::new()));
    #[cfg(target_os = "macos")]
    return macos::Quartz::new().map(|i| Box::new(i) as Box<dyn Injector>);
    #[allow(unreachable_code)]
    Err("This operating system is not supported as a client".into())
}

/// Whether the system lets this process inject input, where it asks
/// (macOS: Accessibility). None where no permission is involved.
pub fn permission() -> Option<bool> {
    #[cfg(target_os = "macos")]
    return Some(macos::trusted());
    #[allow(unreachable_code)]
    None
}

/// Mac clients: Ctrl from the server's keyboard acts as Command (and the
/// Windows key as Control), so Ctrl+C copies and Ctrl+V pastes like on Windows.
static CTRL_IS_COMMAND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_ctrl_is_command(on: bool) {
    if cfg!(target_os = "macos") {
        CTRL_IS_COMMAND.store(on, std::sync::atomic::Ordering::Relaxed);
    }
}

#[allow(dead_code)]
fn ctrl_is_command() -> bool {
    CTRL_IS_COMMAND.load(std::sync::atomic::Ordering::Relaxed)
}

/// The key to hold with C or V to copy or paste on this computer, as sent to
/// [`Injector::key`]: Ctrl, except on a Mac (Command, which is Ctrl when Ctrl acts as Command).
pub fn copy_modifier() -> u16 {
    if cfg!(target_os = "macos") && !ctrl_is_command() {
        125
    } else {
        29
    }
}

/// Swap Ctrl and Meta (Windows key / Command), left and right.
#[allow(dead_code)]
fn swap_ctrl_meta(code: u16) -> u16 {
    match code {
        29 => 125,
        125 => 29,
        97 => 126,
        126 => 97,
        c => c,
    }
}

/// Accumulates high-resolution wheel deltas into whole notches.
#[derive(Default)]
#[allow(dead_code)]
struct WheelAcc {
    x: i32,
    y: i32,
}

#[allow(dead_code)]
impl WheelAcc {
    fn add(&mut self, dx: i32, dy: i32) -> (i32, i32) {
        self.x += dx;
        self.y += dy;
        let (nx, ny) = (self.x / 120, self.y / 120);
        self.x -= nx * 120;
        self.y -= ny * 120;
        (nx, ny)
    }
}

// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod linux {
    use super::{Injector, WheelAcc};
    use crate::proto::*;
    use std::ffi::CString;

    const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
    const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
    const UI_SET_RELBIT: libc::c_ulong = 0x4004_5566;
    const UI_SET_ABSBIT: libc::c_ulong = 0x4004_5567;
    const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
    const UI_ABS_SETUP: libc::c_ulong = 0x401c_5504;
    const UI_DEV_CREATE: libc::c_ulong = 0x5501;
    const UI_DEV_DESTROY: libc::c_ulong = 0x5502;

    const EV_SYN: u16 = 0;
    const EV_KEY: u16 = 1;
    const EV_REL: u16 = 2;
    const EV_ABS: u16 = 3;
    const REL_HWHEEL: u16 = 6;
    const REL_WHEEL: u16 = 8;
    const REL_WHEEL_HI_RES: u16 = 11;
    const REL_HWHEEL_HI_RES: u16 = 12;
    const ABS_X: u16 = 0;
    const ABS_Y: u16 = 1;
    const BTN_LEFT_CODE: u16 = 0x110;
    const ABS_MAX: i32 = 65535;

    #[repr(C)]
    struct InputId {
        bustype: u16,
        vendor: u16,
        product: u16,
        version: u16,
    }
    #[repr(C)]
    struct UinputSetup {
        id: InputId,
        name: [u8; 80],
        ff_effects_max: u32,
    }
    #[repr(C)]
    struct InputAbsinfo {
        value: i32,
        minimum: i32,
        maximum: i32,
        fuzz: i32,
        flat: i32,
        resolution: i32,
    }
    #[repr(C)]
    struct UinputAbsSetup {
        code: u16,
        absinfo: InputAbsinfo,
    }
    #[repr(C)]
    struct InputEvent {
        time: libc::timeval,
        type_: u16,
        code: u16,
        value: i32,
    }

    struct Device {
        fd: libc::c_int,
    }

    impl Device {
        fn open(name: &str, product: u16, setup: impl FnOnce(libc::c_int)) -> Result<Device, String> {
            let path = CString::new("/dev/uinput").unwrap();
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
            if fd < 0 {
                return Err(format!(
                    "Can't open /dev/uinput ({}). Install the edgehop package (it adds a udev rule), \
                     or run: sudo modprobe uinput && sudo setfacl -m u:$USER:rw /dev/uinput",
                    std::io::Error::last_os_error()
                ));
            }
            setup(fd);
            let mut s = UinputSetup {
                id: InputId { bustype: 0x06, vendor: 0x4b56, product, version: 1 },
                name: [0; 80],
                ff_effects_max: 0,
            };
            let n = name.as_bytes();
            s.name[..n.len().min(79)].copy_from_slice(&n[..n.len().min(79)]);
            unsafe {
                if libc::ioctl(fd, UI_DEV_SETUP as _, &s as *const UinputSetup) < 0 || libc::ioctl(fd, UI_DEV_CREATE as _) < 0 {
                    let e = std::io::Error::last_os_error();
                    libc::close(fd);
                    return Err(format!("uinput device setup failed: {e}"));
                }
            }
            Ok(Device { fd })
        }

        fn emit(&self, events: &[(u16, u16, i32)]) {
            let mut buf: Vec<InputEvent> = events
                .iter()
                .map(|&(t, c, v)| InputEvent { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_: t, code: c, value: v })
                .collect();
            buf.push(InputEvent { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_: EV_SYN, code: 0, value: 0 });
            let bytes = buf.len() * std::mem::size_of::<InputEvent>();
            unsafe {
                libc::write(self.fd, buf.as_ptr() as *const libc::c_void, bytes);
            }
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            unsafe {
                libc::ioctl(self.fd, UI_DEV_DESTROY as _);
                libc::close(self.fd);
            }
        }
    }

    pub struct Uinput {
        kbd: Device,
        mouse: Device,
        w: i32,
        h: i32,
        wheel: WheelAcc,
    }

    impl Uinput {
        pub fn new() -> Result<Uinput, String> {
            let kbd = Device::open("edgehop keyboard", 1, |fd| unsafe {
                libc::ioctl(fd, UI_SET_EVBIT as _, EV_KEY as libc::c_int);
                for k in 1..=255 {
                    libc::ioctl(fd, UI_SET_KEYBIT as _, k as libc::c_int);
                }
            })?;
            // Absolute pointer + buttons (classified as a mouse, like a VM tablet),
            // so the cursor lands exactly where the server says.
            let mouse = Device::open("edgehop pointer", 2, |fd| unsafe {
                libc::ioctl(fd, UI_SET_EVBIT as _, EV_KEY as libc::c_int);
                for b in BTN_LEFT_CODE..=BTN_LEFT_CODE + 4 {
                    libc::ioctl(fd, UI_SET_KEYBIT as _, b as libc::c_int);
                }
                libc::ioctl(fd, UI_SET_EVBIT as _, EV_REL as libc::c_int);
                for r in [REL_WHEEL, REL_HWHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES] {
                    libc::ioctl(fd, UI_SET_RELBIT as _, r as libc::c_int);
                }
                libc::ioctl(fd, UI_SET_EVBIT as _, EV_ABS as libc::c_int);
                for axis in [ABS_X, ABS_Y] {
                    libc::ioctl(fd, UI_SET_ABSBIT as _, axis as libc::c_int);
                    let a = UinputAbsSetup {
                        code: axis,
                        absinfo: InputAbsinfo { value: 0, minimum: 0, maximum: ABS_MAX, fuzz: 0, flat: 0, resolution: 0 },
                    };
                    libc::ioctl(fd, UI_ABS_SETUP as _, &a as *const UinputAbsSetup);
                }
            })?;
            let (w, h) = detect_screen();
            log::info!("uinput ready, screen {w}x{h}");
            Ok(Uinput { kbd, mouse, w, h, wheel: WheelAcc::default() })
        }
    }

    impl Injector for Uinput {
        fn screen_size(&self) -> (i32, i32) {
            (self.w, self.h)
        }
        fn move_to(&mut self, x: i32, y: i32) {
            let ax = (x.clamp(0, self.w - 1) as i64 * ABS_MAX as i64 / (self.w - 1).max(1) as i64) as i32;
            let ay = (y.clamp(0, self.h - 1) as i64 * ABS_MAX as i64 / (self.h - 1).max(1) as i64) as i32;
            self.mouse.emit(&[(EV_ABS, ABS_X, ax), (EV_ABS, ABS_Y, ay)]);
        }
        fn button(&mut self, button: u8, down: bool) {
            let code = match button {
                BTN_LEFT => 0x110,
                BTN_RIGHT => 0x111,
                BTN_MIDDLE => 0x112,
                BTN_BACK => 0x113,
                BTN_FORWARD => 0x114,
                _ => return,
            };
            self.mouse.emit(&[(EV_KEY, code, down as i32)]);
        }
        fn wheel(&mut self, dx: i32, dy: i32) {
            let (nx, ny) = self.wheel.add(dx, dy);
            let mut ev = Vec::new();
            if dy != 0 {
                ev.push((EV_REL, REL_WHEEL_HI_RES, dy));
            }
            if ny != 0 {
                ev.push((EV_REL, REL_WHEEL, ny));
            }
            if dx != 0 {
                ev.push((EV_REL, REL_HWHEEL_HI_RES, dx));
            }
            if nx != 0 {
                ev.push((EV_REL, REL_HWHEEL, nx));
            }
            if !ev.is_empty() {
                self.mouse.emit(&ev);
            }
        }
        fn key(&mut self, code: u16, down: bool) {
            if (1..=255).contains(&code) {
                self.kbd.emit(&[(EV_KEY, code, down as i32)]);
            }
        }
    }

    /// Logical desktop size. Only used to scale motion, so an estimate is fine.
    fn detect_screen() -> (i32, i32) {
        // KDE Plasma (Wayland or X11): logical geometry of every output, after scaling.
        if let Ok(out) = std::process::Command::new("kscreen-doctor").arg("-o").output() {
            let s = String::from_utf8_lossy(&out.stdout);
            let (mut w, mut h) = (0i32, 0i32);
            for line in s.lines() {
                // "\tGeometry: 0,0 1520x1014" (may contain ANSI color codes)
                let mut clean = String::new();
                let mut chars = line.chars();
                while let Some(c) = chars.next() {
                    if c == '\x1b' {
                        // skip "ESC [ ... <letter>"
                        for d in chars.by_ref() {
                            if d.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    } else {
                        clean.push(c);
                    }
                }
                if let Some(g) = clean.split("Geometry:").nth(1) {
                    let parts: Vec<i32> = g
                        .split(|c: char| !c.is_ascii_digit() && c != '-')
                        .filter_map(|p| p.parse().ok())
                        .collect();
                    if let [x, y, gw, gh] = parts[..] {
                        w = w.max(x + gw);
                        h = h.max(y + gh);
                    }
                }
            }
            if w > 0 && h > 0 {
                return (w, h);
            }
        }
        if let Ok(out) = std::process::Command::new("xrandr").arg("--current").output() {
            let s = String::from_utf8_lossy(&out.stdout);
            // "Screen 0: minimum 16 x 16, current 2736 x 1824, maximum ..."
            if let Some(cur) = s.split("current ").nth(1) {
                let mut it = cur.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
                if let (Some(w), Some(h)) = (it.next(), it.next()) {
                    if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                        return (w, h);
                    }
                }
            }
        }
        if let Ok(dir) = std::fs::read_dir("/sys/class/drm") {
            for e in dir.flatten() {
                let p = e.path();
                let connected = std::fs::read_to_string(p.join("status")).map(|s| s.trim() == "connected").unwrap_or(false);
                if connected {
                    if let Ok(modes) = std::fs::read_to_string(p.join("modes")) {
                        if let Some((w, h)) = modes.lines().next().and_then(|l| l.split_once('x')) {
                            if let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().trim_end_matches(|c: char| !c.is_ascii_digit()).parse()) {
                                return (w, h);
                            }
                        }
                    }
                }
            }
        }
        (1920, 1080)
    }
}

// ---------------------------------------------------------------------------
#[cfg(windows)]
mod windows {
    use super::Injector;
    use crate::keymap::{evdev_to_win, WinKey};
    use crate::proto::*;
    use crate::win32::*;

    pub struct SendInputInjector;

    impl SendInputInjector {
        pub fn new() -> Self {
            SendInputInjector
        }
        fn mouse(&self, dx: i32, dy: i32, data: u32, flags: u32) {
            let input = INPUT {
                type_: INPUT_MOUSE,
                u: INPUT_U { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
            };
            unsafe {
                SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
            }
        }
    }

    impl Injector for SendInputInjector {
        fn screen_size(&self) -> (i32, i32) {
            let (_, _, w, h) = virtual_screen();
            (w, h)
        }
        fn move_to(&mut self, x: i32, y: i32) {
            let (_, _, w, h) = virtual_screen();
            let nx = (x.clamp(0, w - 1) as i64 * 65535 / (w - 1).max(1) as i64) as i32;
            let ny = (y.clamp(0, h - 1) as i64 * 65535 / (h - 1).max(1) as i64) as i32;
            self.mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
        }
        fn button(&mut self, button: u8, down: bool) {
            let (flags, data) = match (button, down) {
                (BTN_LEFT, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                (BTN_LEFT, false) => (MOUSEEVENTF_LEFTUP, 0),
                (BTN_RIGHT, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                (BTN_RIGHT, false) => (MOUSEEVENTF_RIGHTUP, 0),
                (BTN_MIDDLE, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                (BTN_MIDDLE, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                (BTN_BACK, d) => (if d { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP }, 1),
                (BTN_FORWARD, d) => (if d { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP }, 2),
                _ => return,
            };
            self.mouse(0, 0, data, flags);
        }
        fn wheel(&mut self, dx: i32, dy: i32) {
            if dy != 0 {
                self.mouse(0, 0, dy as u32, MOUSEEVENTF_WHEEL);
            }
            if dx != 0 {
                self.mouse(0, 0, dx as u32, MOUSEEVENTF_HWHEEL);
            }
        }
        fn key(&mut self, code: u16, down: bool) {
            let up = if down { 0 } else { KEYEVENTF_KEYUP };
            let ki = match evdev_to_win(code) {
                Some(WinKey::Scan(s, ext)) => KEYBDINPUT {
                    wVk: 0,
                    wScan: s,
                    dwFlags: KEYEVENTF_SCANCODE | up | if ext { KEYEVENTF_EXTENDEDKEY } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
                Some(WinKey::Vk(vk)) => KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: up, time: 0, dwExtraInfo: 0 },
                None => return,
            };
            let input = INPUT { type_: INPUT_KEYBOARD, u: INPUT_U { ki } };
            unsafe {
                SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
            }
        }
    }
}

// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod macos {
    use super::{Injector, WheelAcc};
    use crate::keymap::evdev_to_mac;
    use crate::proto::*;
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    #[repr(C)]
    #[derive(Clone, Copy)]
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
    type CGEventRef = *mut c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayBounds(display: u32) -> CGRect;
        fn CGEventCreateMouseEvent(src: *const c_void, kind: u32, p: CGPoint, button: u32) -> CGEventRef;
        fn CGEventCreateKeyboardEvent(src: *const c_void, key: u16, down: bool) -> CGEventRef;
        fn CGEventCreateScrollWheelEvent2(src: *const c_void, units: u32, count: u32, w1: i32, w2: i32, w3: i32) -> CGEventRef;
        fn CGEventSetFlags(e: CGEventRef, flags: u64);
        fn CGEventSetIntegerValueField(e: CGEventRef, field: u32, value: i64);
        fn CGEventPost(tap: u32, e: CGEventRef);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(p: *const c_void);
        fn CFDictionaryCreate(
            alloc: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        static kCFBooleanTrue: *const c_void;
        // Only their addresses are used.
        static kCFTypeDictionaryKeyCallBacks: [usize; 6];
        static kCFTypeDictionaryValueCallBacks: [usize; 5];
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }

    /// Show macOS's "Edgehop would like to control this computer" prompt,
    /// which leads to the Accessibility settings. Once per run.
    fn ask_for_permission() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks) as *const c_void,
                std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks) as *const c_void,
            );
            AXIsProcessTrustedWithOptions(options);
            if !options.is_null() {
                CFRelease(options);
            }
        });
    }

    const MOUSE_MOVED: u32 = 5;
    const LEFT_DOWN: u32 = 1;
    const LEFT_UP: u32 = 2;
    const RIGHT_DOWN: u32 = 3;
    const RIGHT_UP: u32 = 4;
    const LEFT_DRAGGED: u32 = 6;
    const RIGHT_DRAGGED: u32 = 7;
    const OTHER_DOWN: u32 = 25;
    const OTHER_UP: u32 = 26;
    const OTHER_DRAGGED: u32 = 27;
    const FIELD_CLICK_STATE: u32 = 1;
    const HID_TAP: u32 = 0;

    const FLAG_CAPS: u64 = 0x10000;
    const FLAG_SHIFT: u64 = 0x20000;
    const FLAG_CTRL: u64 = 0x40000;
    const FLAG_ALT: u64 = 0x80000;
    const FLAG_CMD: u64 = 0x100000;

    unsafe impl Send for Quartz {}

    pub fn trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    pub struct Quartz {
        pos: CGPoint,
        origin: CGPoint,
        w: i32,
        h: i32,
        buttons: [bool; 6],
        flags: u64,
        last_click: (u8, Instant, i64),
        wheel: WheelAcc,
    }

    impl Quartz {
        pub fn new() -> Result<Quartz, String> {
            if !trusted() {
                log::warn!("Edgehop needs Accessibility permission: System Settings → Privacy & Security → Accessibility");
                ask_for_permission();
            }
            let b = unsafe { CGDisplayBounds(CGMainDisplayID()) };
            Ok(Quartz {
                pos: CGPoint { x: 0.0, y: 0.0 },
                origin: b.origin,
                w: b.size.width as i32,
                h: b.size.height as i32,
                buttons: [false; 6],
                flags: 0,
                last_click: (0, Instant::now() - Duration::from_secs(10), 0),
                wheel: WheelAcc::default(),
            })
        }

        fn post(&self, e: CGEventRef) {
            if e.is_null() {
                return;
            }
            unsafe {
                CGEventPost(HID_TAP, e);
                CFRelease(e as *const c_void);
            }
        }
    }

    impl Injector for Quartz {
        fn screen_size(&self) -> (i32, i32) {
            (self.w, self.h)
        }

        fn move_to(&mut self, x: i32, y: i32) {
            self.pos = CGPoint { x: self.origin.x + x as f64, y: self.origin.y + y as f64 };
            let (kind, btn) = if self.buttons[BTN_LEFT as usize] {
                (LEFT_DRAGGED, 0)
            } else if self.buttons[BTN_RIGHT as usize] {
                (RIGHT_DRAGGED, 1)
            } else if self.buttons[BTN_MIDDLE as usize] {
                (OTHER_DRAGGED, 2)
            } else {
                (MOUSE_MOVED, 0)
            };
            let e = unsafe { CGEventCreateMouseEvent(std::ptr::null(), kind, self.pos, btn) };
            self.post(e);
        }

        fn button(&mut self, button: u8, down: bool) {
            let (kind, btn) = match (button, down) {
                (BTN_LEFT, true) => (LEFT_DOWN, 0),
                (BTN_LEFT, false) => (LEFT_UP, 0),
                (BTN_RIGHT, true) => (RIGHT_DOWN, 1),
                (BTN_RIGHT, false) => (RIGHT_UP, 1),
                (BTN_MIDDLE, d) => (if d { OTHER_DOWN } else { OTHER_UP }, 2),
                (BTN_BACK, d) => (if d { OTHER_DOWN } else { OTHER_UP }, 3),
                (BTN_FORWARD, d) => (if d { OTHER_DOWN } else { OTHER_UP }, 4),
                _ => return,
            };
            if (button as usize) < self.buttons.len() {
                self.buttons[button as usize] = down;
            }
            if down {
                let (lb, lt, n) = self.last_click;
                let count = if lb == button && lt.elapsed() < Duration::from_millis(450) { n + 1 } else { 1 };
                self.last_click = (button, Instant::now(), count);
            }
            let e = unsafe { CGEventCreateMouseEvent(std::ptr::null(), kind, self.pos, btn) };
            if !e.is_null() {
                unsafe { CGEventSetIntegerValueField(e, FIELD_CLICK_STATE, self.last_click.2.max(1)) };
            }
            self.post(e);
        }

        fn wheel(&mut self, dx: i32, dy: i32) {
            let (nx, ny) = self.wheel.add(dx, dy);
            if nx != 0 || ny != 0 {
                let e = unsafe { CGEventCreateScrollWheelEvent2(std::ptr::null(), 1, 2, ny, nx, 0) };
                self.post(e);
            }
        }

        fn key(&mut self, code: u16, down: bool) {
            let code = if super::ctrl_is_command() { super::swap_ctrl_meta(code) } else { code };
            let flag = match code {
                42 | 54 => FLAG_SHIFT,
                29 | 97 => FLAG_CTRL,
                56 | 100 => FLAG_ALT,
                125 | 126 => FLAG_CMD,
                58 => FLAG_CAPS,
                _ => 0,
            };
            if flag != 0 && flag != FLAG_CAPS {
                if down {
                    self.flags |= flag;
                } else {
                    self.flags &= !flag;
                }
            } else if flag == FLAG_CAPS && down {
                self.flags ^= FLAG_CAPS;
            }
            let Some(k) = evdev_to_mac(code) else { return };
            let e = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), k, down) };
            if !e.is_null() {
                unsafe { CGEventSetFlags(e, self.flags) };
            }
            self.post(e);
        }
    }
}
