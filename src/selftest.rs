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
    // Linux CI has no desktop: only the input-device side of sharing can be checked.
    #[cfg(target_os = "linux")]
    if args.iter().any(|a| a == "--sharing-only") {
        sharing_linux(&mut r);
        return finish(r);
    }
    clipboard_text(&mut r);
    clipboard_image(&mut r);
    #[cfg(target_os = "macos")]
    mac_files(&mut r);

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
    let typed = args.iter().any(|a| a == "--type") && typing(&mut r, inj.as_mut(), events_work);
    if typed && cfg!(target_os = "macos") {
        ctrl_as_command(&mut r, inj.as_mut());
    }
    #[cfg(target_os = "macos")]
    if args.iter().any(|a| a == "--sharing") {
        sharing_mac(&mut r, inj.as_mut(), events_work, typed);
    }
    #[cfg(target_os = "linux")]
    if args.iter().any(|a| a == "--sharing") {
        sharing_linux(&mut r);
    }
    finish(r)
}

/// Command+A, Command+C in the focused window; what it copied.
#[cfg(target_os = "macos")]
fn copy_everything(inj: &mut dyn crate::inject::Injector) -> Option<String> {
    const NOTHING: &str = "edgehop: nothing copied yet";
    // Twice at most: the first Command+A can go to dismissing a text prediction.
    for _ in 0..2 {
        crate::clipboard::set(NOTHING);
        select_all_and_copy(inj, 125);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            // While an app is copying, the clipboard is briefly empty: keep waiting.
            if let Some(t) = crate::clipboard::get().filter(|t| t != NOTHING) {
                return Some(t);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    None
}

/// Waits until input capture has reported what `ok` looks for.
#[cfg(target_os = "macos")]
fn wait_for<T: Clone>(seen: &std::sync::Mutex<Vec<T>>, ok: impl Fn(&[T]) -> bool) -> (bool, Vec<T>) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let got = crate::state::lk(seen).clone();
        if ok(&got) || Instant::now() > deadline {
            return (ok(&got), got);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Sharing this Mac's keyboard and mouse: the event tap sees the pointer,
/// keys, buttons and wheel, swallows input while "away", and the pointer can
/// be parked and hidden.
#[cfg(target_os = "macos")]
fn sharing_mac(r: &mut Report, inj: &mut dyn crate::inject::Injector, events_work: bool, typed: bool) {
    use crate::server::capture::{self, Input};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    if !events_work {
        r.skip("sharing", "injected input isn't reaching the system, so capture can't be checked");
        return;
    }
    let seen: Arc<Mutex<Vec<Input>>> = Arc::default();
    let swallow = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let (s2, w2, st2) = (seen.clone(), swallow.clone(), stop.clone());
    let tap = std::thread::spawn(move || {
        capture::tap_loop(
            Box::new(move |i| {
                crate::state::lk(&s2).push(i);
                w2.load(Ordering::Relaxed)
            }),
            tx,
            st2,
        )
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(h) if h != 0 => r.pass("capture", "Quartz event tap installed"),
        _ => {
            r.fail("capture", "macOS refused the event tap (Accessibility)");
            return;
        }
    }
    std::thread::sleep(Duration::from_millis(200));
    let clear = || crate::state::lk(&seen).clear();

    clear();
    inj.move_to(300, 300);
    let (ok, got) = wait_for(&seen, |v| v.iter().any(|i| matches!(i, Input::Move { x, y, .. } if (x - 300).abs() <= 2 && (y - 300).abs() <= 2)));
    if ok { r.pass("capture pointer", "saw the pointer arrive at 300,300") } else { r.fail("capture pointer", format!("saw {got:?}")) }

    clear();
    // Right arrow: a key that types nothing into TextEdit.
    inj.key(106, true);
    inj.key(106, false);
    let want = [Input::Key { code: 106, down: true }, Input::Key { code: 106, down: false }];
    let (ok, got) = wait_for(&seen, |v| want.iter().all(|w| v.contains(w)));
    if ok { r.pass("capture keys", "Right arrow down and up, as key 106") } else { r.fail("capture keys", format!("saw {got:?}")) }

    clear();
    inj.key(42, true); // Shift
    inj.key(42, false);
    let want = [Input::Key { code: 42, down: true }, Input::Key { code: 42, down: false }];
    let (ok, got) = wait_for(&seen, |v| want.iter().all(|w| v.contains(w)));
    if ok { r.pass("capture modifiers", "Shift down and up") } else { r.fail("capture modifiers", format!("saw {got:?}")) }

    clear();
    inj.button(crate::proto::BTN_LEFT, true);
    inj.button(crate::proto::BTN_LEFT, false);
    let want = [
        Input::Button { button: crate::proto::BTN_LEFT, down: true },
        Input::Button { button: crate::proto::BTN_LEFT, down: false },
    ];
    let (ok, got) = wait_for(&seen, |v| want.iter().all(|w| v.contains(w)));
    if ok { r.pass("capture buttons", "left button down and up") } else { r.fail("capture buttons", format!("saw {got:?}")) }

    clear();
    inj.wheel(0, 120);
    let (ok, got) = wait_for(&seen, |v| v.iter().any(|i| matches!(i, Input::Wheel { dy, .. } if *dy > 0)));
    if ok { r.pass("capture wheel", "one notch up") } else { r.fail("capture wheel", format!("saw {got:?}")) }

    // While the mouse is on another computer, nothing may reach this Mac.
    if typed {
        // What TextEdit holds now (the checks above typed into it too).
        let before = copy_everything(inj);
        swallow.store(true, Ordering::Relaxed);
        for _ in 0..3 {
            inj.key(44, true); // Z
            inj.key(44, false);
        }
        std::thread::sleep(Duration::from_millis(300));
        swallow.store(false, Ordering::Relaxed);
        let after = copy_everything(inj);
        if before.is_some() && before == after {
            r.pass("swallow while away", "keys typed while away never reached TextEdit");
        } else {
            r.fail("swallow while away", format!("TextEdit held {before:?}, then {after:?}"));
        }
    } else {
        r.skip("swallow while away", "needs --type (a text editor in front)");
    }
    stop.store(true, Ordering::Relaxed);
    let _ = tap.join();

    capture::warp(200, 200);
    std::thread::sleep(Duration::from_millis(100));
    let at = capture::pointer();
    if at == (200, 200) { r.pass("park pointer", "warped to 200,200") } else { r.fail("park pointer", format!("pointer at {at:?}")) }
    capture::set_pointer_visible(false);
    capture::set_pointer_visible(true);
    r.pass("hide pointer", "hid and showed the pointer");
    let mons = crate::platform::monitors();
    if mons.is_empty() {
        r.fail("monitors", "no displays found");
    } else {
        let list: Vec<String> =
            mons.iter().map(|m| format!("{} {}x{} at {},{}", m.label, m.rect.w(), m.rect.h(), m.rect.l, m.rect.t)).collect();
        r.pass("monitors", list.join("; "));
    }
}

/// Sharing from Linux: a virtual keyboard+mouse made for the test is found,
/// its events come through translated, and grabbing it is exclusive.
#[cfg(target_os = "linux")]
fn sharing_linux(r: &mut Report) {
    use crate::server::capture::{test_devices, Input};
    const NAME: &str = "Selftest input";
    let dev = match crate::inject::test_device(NAME) {
        Ok(d) => d,
        Err(e) => {
            r.fail("test device", e);
            return;
        }
    };
    std::thread::sleep(Duration::from_millis(800)); // udev creates the node
    let devs = test_devices();
    devs.scan();
    let found = devs.describe();
    r.info("input devices", found.join("; "));
    if !found.iter().any(|d| d.starts_with(NAME)) {
        r.fail("find devices", "the test keyboard+mouse wasn't picked up (permissions?)");
        return;
    }
    r.pass("find devices", "found the test keyboard+mouse");
    let seen: std::sync::Mutex<Vec<Input>> = Default::default();
    let check = |r: &mut Report, name: &str, events: &[(u16, u16, i32)], want: &[Input]| {
        crate::state::lk(&seen).clear();
        dev.emit(events);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !want.iter().all(|w| crate::state::lk(&seen).contains(w)) {
            let got = devs.poll(100);
            crate::state::lk(&seen).extend(got);
        }
        let got = crate::state::lk(&seen).clone();
        if want.iter().all(|w| got.contains(w)) {
            r.pass(name, format!("{want:?}"));
        } else {
            r.fail(name, format!("expected {want:?}, saw {got:?}"));
        }
    };
    check(r, "capture pointer", &[(2, 0, 7), (2, 1, -4)], &[Input::Move { dx: 7, dy: -4 }]);
    check(r, "capture keys", &[(1, 37, 1)], &[Input::Key { code: 37, down: true }]);
    check(r, "capture keys up", &[(1, 37, 0)], &[Input::Key { code: 37, down: false }]);
    check(r, "capture buttons", &[(1, 0x110, 1)], &[Input::Button { button: crate::proto::BTN_LEFT, down: true }]);
    check(r, "capture buttons up", &[(1, 0x110, 0)], &[Input::Button { button: crate::proto::BTN_LEFT, down: false }]);
    check(r, "capture wheel", &[(2, 11, 120), (2, 8, 1)], &[Input::Wheel { dx: 0, dy: 120 }]);

    // A key held while leaving for another computer: the device itself must
    // report it released (the desktop would otherwise keep it down), and say
    // nothing more when the key is really let go.
    dev.emit(&[(1, 38, 1)]);
    std::thread::sleep(Duration::from_millis(100));
    devs.poll(100);
    let released = devs.release_held_named(NAME);
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !got.contains(&Input::Key { code: 38, down: false }) {
        got.extend(devs.poll(100));
    }
    dev.emit(&[(1, 38, 0)]);
    let after = devs.poll(300);
    if released == 1 && got.contains(&Input::Key { code: 38, down: false }) && after.is_empty() {
        r.pass("release held keys", "a held key is released on this computer before its keyboard is taken over");
    } else {
        r.fail("release held keys", format!("released {released}, then saw {got:?}, and {after:?} at the real release"));
    }

    // Grabbing must be exclusive: nobody else can grab the device after us.
    if !devs.grab_named(NAME, true) {
        r.fail("grab", "couldn't grab the test device");
        return;
    }
    let other = test_devices();
    other.scan();
    let stolen = other.grab_named(NAME, true);
    check(r, "capture while grabbed", &[(2, 0, 3), (2, 1, 3)], &[Input::Move { dx: 3, dy: 3 }]);
    devs.grab_named(NAME, false);
    if stolen {
        r.fail("grab", "a second reader could grab the device too");
    } else {
        r.pass("grab", "exclusive while away, released after");
    }
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

/// Files on the clipboard and on the drag pasteboard (what a Finder drag
/// leaves there), written and read back.
#[cfg(target_os = "macos")]
fn mac_files(r: &mut Report) {
    let dir = std::env::temp_dir().join(format!("edgehop-self-test-{}", std::process::id()));
    let paths: Vec<std::path::PathBuf> = ["one.txt", "two with spaces.txt"].iter().map(|n| dir.join(n)).collect();
    if std::fs::create_dir_all(&dir).is_err() || paths.iter().any(|p| std::fs::write(p, "edgehop").is_err()) {
        r.fail("clipboard files", format!("couldn't make test files in {}", dir.display()));
        return;
    }
    let same = |got: &[std::path::PathBuf]| {
        let canon = |p: &std::path::PathBuf| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        got.len() == paths.len() && got.iter().zip(&paths).all(|(a, b)| canon(a) == canon(b))
    };
    let saved = crate::clipboard::get();
    crate::clipboard::set_files(&paths);
    std::thread::sleep(Duration::from_millis(150));
    match crate::clipboard::get_files() {
        Some(f) if same(&f) => r.pass("clipboard files", "2 files copied and read back"),
        Some(f) => r.fail("clipboard files", format!("read back {f:?}")),
        None => r.fail("clipboard files", "no files on the clipboard after copying two"),
    }
    let before = crate::mac_files::drag_change_count();
    if !crate::mac_files::put_on_drag_pasteboard(&paths) {
        r.fail("drag pasteboard", "couldn't write to it");
    } else {
        let after = crate::mac_files::drag_change_count();
        let f = crate::mac_files::drag_files();
        if after != before && same(&f) {
            r.pass("drag pasteboard", format!("a new drag is noticed (count {before} -> {after}) and its 2 files read"));
        } else {
            r.fail("drag pasteboard", format!("count {before} -> {after}, files {f:?}"));
        }
    }
    let desktop = directories::UserDirs::new().and_then(|u| u.desktop_dir().map(|d| d.to_path_buf()));
    r.info("drop folder", format!("desktop is {desktop:?}"));
    if let Some(t) = saved {
        crate::clipboard::set(&t);
    }
    let _ = std::fs::remove_dir_all(&dir);
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

/// Returns whether the sentence was typed and copied back.
fn typing(r: &mut Report, inj: &mut dyn crate::inject::Injector, events_work: bool) -> bool {
    if !events_work {
        r.skip("typing", "injected input isn't reaching the system (see above)");
        return false;
    }
    const SHIFT: u16 = 42;
    #[cfg(target_os = "macos")]
    const MODIFIER: u16 = 125; // Command
    #[cfg(not(target_os = "macos"))]
    const MODIFIER: u16 = 29; // Ctrl

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
                return false;
            }
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    select_all_and_copy(inj, MODIFIER);
    match copied() {
        Some(t) if t == TYPED => {
            r.pass("typing", format!("typed, selected and copied {TYPED:?}"));
            return true;
        }
        Some(t) if t == "before typing" => {
            r.fail("typing", "select-all + copy didn't reach the focused window (is a text editor in front?)")
        }
        other => r.fail("typing", format!("expected {TYPED:?}, clipboard has {other:?}")),
    }
    false
}

/// With "Ctrl works as Command" on, Ctrl+A / Ctrl+C from the server must
/// select and copy on a Mac.
fn ctrl_as_command(r: &mut Report, inj: &mut dyn crate::inject::Injector) {
    crate::clipboard::set("before ctrl");
    crate::inject::set_ctrl_is_command(true);
    select_all_and_copy(inj, 29);
    crate::inject::set_ctrl_is_command(false);
    match copied() {
        Some(t) if t == TYPED => r.pass("ctrl as command", "Ctrl+A, Ctrl+C selected and copied"),
        other => r.fail("ctrl as command", format!("Ctrl+A, Ctrl+C didn't copy; clipboard has {other:?}")),
    }
}

fn select_all_and_copy(inj: &mut dyn crate::inject::Injector, modifier: u16) {
    for code in [30, 46] {
        inj.key(modifier, true);
        std::thread::sleep(Duration::from_millis(8));
        inj.key(code, true);
        std::thread::sleep(Duration::from_millis(8));
        inj.key(code, false);
        inj.key(modifier, false);
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The clipboard once it holds the typed sentence, or whatever it holds after 3 s.
fn copied() -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got = crate::clipboard::get();
    while got.as_deref() != Some(TYPED) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        got = crate::clipboard::get();
    }
    got
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
