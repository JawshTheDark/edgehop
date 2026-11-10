//! `edgehop --self-test [--type]`: exercise the parts of Edgehop that talk to
//! the operating system (clipboard, input injection, screen size) without a
//! server, so a build can be checked on a machine nobody is sitting at, such
//! as a CI Mac. Prints one line per check and exits non-zero if any fails.
//!
//! `--type` also types a sentence into whatever window has focus, selects it
//! and copies it, then checks the clipboard: the whole keyboard path
//! (key codes, Shift, Command/Ctrl) end to end. Open an empty text editor first.
use std::time::{Duration, Instant};

const TYPED: &str = "Hello from Edgehop, 123!";

#[derive(Default)]
struct Report {
    failed: u32,
}

impl Report {
    fn line(&mut self, status: &str, check: &str, detail: impl std::fmt::Display) {
        println!("{status:<5} {check:<18} {detail}");
        if status == "FAIL" {
            self.failed += 1;
        }
    }
    fn pass(&mut self, check: &str, detail: impl std::fmt::Display) {
        self.line("ok", check, detail);
    }
    fn fail(&mut self, check: &str, detail: impl std::fmt::Display) {
        self.line("FAIL", check, detail);
    }
    fn skip(&mut self, check: &str, detail: impl std::fmt::Display) {
        self.line("skip", check, detail);
    }
    fn info(&mut self, check: &str, detail: impl std::fmt::Display) {
        self.line("info", check, detail);
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut r = Report::default();
    r.info(
        "build",
        format!("Edgehop {} on {} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH),
    );
    r.info("log file", crate::state::log_path().display());
    clipboard_text(&mut r);
    clipboard_image(&mut r);

    let mut inj = match crate::inject::create() {
        Ok(i) => i,
        Err(e) => {
            r.fail("input injector", e);
            return finish(r);
        }
    };
    let (w, h) = inj.screen_size();
    if w > 0 && h > 0 {
        r.pass("screen size", format!("{w}x{h}"));
    } else {
        r.fail("screen size", format!("{w}x{h}"));
    }
    let allowed = crate::inject::permission();
    match allowed {
        Some(true) => r.info("input permission", "granted"),
        Some(false) => r.info("input permission", "not granted (Accessibility); injected input may be dropped"),
        None => {}
    }
    r.info("desktop hooks", if crate::platform::client_hooks().is_some() { "available" } else { "none on this system" });

    let events_work = pointer(&mut r, inj.as_mut(), w, h, allowed);
    if args.iter().any(|a| a == "--type") {
        typing(&mut r, inj.as_mut(), events_work);
    }
    finish(r)
}

fn finish(r: Report) -> i32 {
    if r.failed == 0 {
        println!("self-test passed");
        0
    } else {
        println!("self-test: {} check(s) failed", r.failed);
        1
    }
}

fn clipboard_text(r: &mut Report) {
    let probe = format!("edgehop self-test {}", std::process::id());
    crate::clipboard::set(&probe);
    std::thread::sleep(Duration::from_millis(150));
    match crate::clipboard::get() {
        Some(t) if t == probe => r.pass("clipboard text", "copied and read back"),
        Some(t) => r.fail("clipboard text", format!("read back {t:?}")),
        None => r.fail("clipboard text", "nothing on the clipboard after copying"),
    }
}

fn clipboard_image(r: &mut Report) {
    crate::clipboard::set_image_png(&crate::icon::png(64));
    std::thread::sleep(Duration::from_millis(150));
    let Some(png) = crate::clipboard::get_image_png() else {
        r.fail("clipboard image", "no image on the clipboard after copying one");
        return;
    };
    // PNG header: width and height are big-endian u32s at bytes 16 and 20.
    let dim = |at: usize| png.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    match (dim(16), dim(20)) {
        (Some(64), Some(64)) => r.pass("clipboard image", format!("64x64 image round-tripped ({} bytes)", png.len())),
        (w, h) => r.fail("clipboard image", format!("read back a {w:?}x{h:?} image")),
    }
}

/// Move the pointer and read it back. Returns whether injected input works.
fn pointer(r: &mut Report, inj: &mut dyn crate::inject::Injector, w: i32, h: i32, allowed: Option<bool>) -> bool {
    let Some(_) = cursor_pos() else {
        r.skip("pointer", "can't read the pointer position on this system");
        return allowed != Some(false);
    };
    let mut missed = Vec::new();
    for (x, y) in [(w / 3, h / 3), (2 * w / 3, 2 * h / 3), (w / 2, h / 2)] {
        inj.move_to(x, y);
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut at = cursor_pos();
        while !near(at, x, y) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            at = cursor_pos();
        }
        if !near(at, x, y) {
            missed.push(format!("asked for {x},{y}, pointer at {at:?}"));
        }
    }
    if missed.is_empty() {
        r.pass("pointer", "moved to 3 positions and landed on each");
        true
    } else if allowed == Some(false) {
        r.skip("pointer", format!("didn't move without Accessibility permission ({})", missed.join("; ")));
        false
    } else {
        r.fail("pointer", missed.join("; "));
        false
    }
}

fn near(at: Option<(f64, f64)>, x: i32, y: i32) -> bool {
    at.is_some_and(|(ax, ay)| (ax - x as f64).abs() <= 2.0 && (ay - y as f64).abs() <= 2.0)
}

fn typing(r: &mut Report, inj: &mut dyn crate::inject::Injector, events_work: bool) {
    if !events_work {
        r.skip("typing", "injected input isn't reaching the system (see above)");
        return;
    }
    const SHIFT: u16 = 42;
    #[cfg(target_os = "macos")]
    const MODIFIER: u16 = 125; // Command
    #[cfg(not(target_os = "macos"))]
    const MODIFIER: u16 = 29; // Ctrl
    const KEY_A: u16 = 30;
    const KEY_C: u16 = 46;

    crate::clipboard::set("before typing");
    fn tap(inj: &mut dyn crate::inject::Injector, code: u16, shift: bool) {
        if shift {
            inj.key(SHIFT, true);
        }
        inj.key(code, true);
        std::thread::sleep(Duration::from_millis(8));
        inj.key(code, false);
        if shift {
            inj.key(SHIFT, false);
        }
        std::thread::sleep(Duration::from_millis(12));
    }
    for c in TYPED.chars() {
        match us_key(c) {
            Some((code, shift)) => tap(inj, code, shift),
            None => {
                r.fail("typing", format!("no key for {c:?}"));
                return;
            }
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    for code in [KEY_A, KEY_C] {
        inj.key(MODIFIER, true);
        tap(inj, code, false);
        inj.key(MODIFIER, false);
        std::thread::sleep(Duration::from_millis(200));
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got = crate::clipboard::get();
    while got.as_deref() != Some(TYPED) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        got = crate::clipboard::get();
    }
    match got {
        Some(t) if t == TYPED => r.pass("typing", format!("typed, selected and copied {TYPED:?}")),
        Some(t) if t == "before typing" => {
            r.fail("typing", "select-all + copy didn't reach the focused window (is a text editor in front?)")
        }
        other => r.fail("typing", format!("expected {TYPED:?}, clipboard has {other:?}")),
    }
}

/// US layout: character -> (evdev key code, needs Shift).
fn us_key(c: char) -> Option<(u16, bool)> {
    const ROWS: [(&str, u16); 4] = [("1234567890", 2), ("qwertyuiop", 16), ("asdfghjkl", 30), ("zxcvbnm", 44)];
    let lower = c.to_ascii_lowercase();
    for (row, first) in ROWS {
        if let Some(i) = row.find(lower) {
            return Some((first + i as u16, c.is_ascii_uppercase()));
        }
    }
    Some(match c {
        ' ' => (57, false),
        ',' => (51, false),
        '.' => (52, false),
        '!' => (2, true),
        '?' => (53, true),
        '-' => (12, false),
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
fn cursor_pos() -> Option<(f64, f64)> {
    use std::ffi::c_void;
    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreate(src: *const c_void) -> *mut c_void;
        fn CGEventGetLocation(e: *mut c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(p: *const c_void);
    }
    unsafe {
        let e = CGEventCreate(std::ptr::null());
        if e.is_null() {
            return None;
        }
        let p = CGEventGetLocation(e);
        CFRelease(e as *const c_void);
        Some((p.x, p.y))
    }
}

#[cfg(windows)]
fn cursor_pos() -> Option<(f64, f64)> {
    let mut p = crate::win32::POINT { x: 0, y: 0 };
    (unsafe { crate::win32::GetCursorPos(&mut p) } != 0).then_some((p.x as f64, p.y as f64))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn cursor_pos() -> Option<(f64, f64)> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_typed_character_has_a_key() {
        for c in super::TYPED.chars() {
            assert!(super::us_key(c).is_some(), "{c:?}");
        }
        assert_eq!(super::us_key('H'), Some((35, true)));
        assert_eq!(super::us_key('1'), Some((2, false)));
        assert_eq!(super::us_key('m'), Some((50, false)));
    }
}
