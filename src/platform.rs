//! Small OS-specific helpers: DPI awareness and showing/hiding the window.
use crate::state::{lk, AppState};
use eframe::egui;

/// Distinct enough that no Explorer window (e.g. the "Edgehop" install folder) matches.
pub const WINDOW_TITLE: &str = "Edgehop — keyboard & mouse sharing";

/// On Windows we can truly hide to the tray and restore via Win32.
/// Elsewhere the window minimizes instead (restoring a hidden window is unreliable).
pub const HIDE_TO_TRAY: bool = cfg!(windows);

/// On Linux the window is closed (not hidden) into the tray, and a new one
/// opens from the tray icon: hidden windows don't reliably come back there.
pub const CLOSE_TO_TRAY: bool = cfg!(target_os = "linux");

pub fn init_dpi() {
    #[cfg(windows)]
    unsafe {
        use crate::win32::*;
        // SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2) so coordinates are physical pixels.
        let user32 = LoadLibraryW(wide("user32.dll").as_ptr());
        if !user32.is_null() {
            let f = GetProcAddress(user32, b"SetProcessDpiAwarenessContext\0".as_ptr());
            if !f.is_null() {
                let f: extern "system" fn(isize) -> i32 = std::mem::transmute(f);
                f(-4);
            }
        }
    }
}

/// Proof that this is the only Edgehop running (held for the whole run).
pub struct InstanceGuard {
    #[cfg(windows)]
    _mutex: crate::win32::HANDLE,
    #[cfg(unix)]
    listener: Option<std::os::unix::net::UnixListener>,
    /// Held for as long as this copy runs.
    #[cfg(unix)]
    _lock: std::fs::File,
}

/// Claim "the one Edgehop running". If another copy has it, ask that one to
/// show its window and return None. `restarting`: we're replacing ourselves,
/// so wait for the old copy to exit instead.
#[cfg(windows)]
pub fn single_instance(restarting: bool) -> Option<InstanceGuard> {
    use crate::win32::*;
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attributes: *const std::ffi::c_void, initial_owner: i32, name: *const u16) -> HANDLE;
        fn GetLastError() -> u32;
        fn WaitForSingleObject(handle: HANDLE, ms: u32) -> u32;
        fn CloseHandle(handle: HANDLE) -> i32;
    }
    const ERROR_ALREADY_EXISTS: u32 = 183;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_ABANDONED: u32 = 0x80;
    unsafe {
        let name = wide("Local\\Edgehop.Running");
        let mutex = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        if mutex.is_null() {
            return Some(InstanceGuard { _mutex: mutex }); // can't tell: run
        }
        if GetLastError() != ERROR_ALREADY_EXISTS {
            return Some(InstanceGuard { _mutex: mutex });
        }
        let r = WaitForSingleObject(mutex, if restarting { 15_000 } else { 0 });
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
            return Some(InstanceGuard { _mutex: mutex });
        }
        CloseHandle(mutex);
        let title = wide(WINDOW_TITLE);
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
        None
    }
}

/// Linux/macOS: a socket in the user's runtime folder; the running copy
/// listens on it and shows its window when asked.
#[cfg(unix)]
pub fn single_instance(restarting: bool) -> Option<InstanceGuard> {
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::os::fd::AsRawFd;
    let path = instance_socket();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Who runs is decided by a lock on a file (the system lets go of it when
    // the holder ends, however it ends): two copies started in the same
    // instant can't both win, as they could when each only looked for the
    // other's socket.
    let lock_path = path.with_extension("lock");
    let lock = match std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path) {
        Ok(f) => f,
        Err(e) => {
            crate::state::print_err(&format!("can't open {}: {e}", lock_path.display()));
            return None;
        }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(if restarting { 15 } else { 0 });
    loop {
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            // Ours. Any socket there is a leftover from a run that has ended.
            let _ = std::fs::remove_file(&path);
            return Some(InstanceGuard { listener: UnixListener::bind(&path).ok(), _lock: lock });
        }
        if std::time::Instant::now() < deadline {
            // Replacing ourselves: wait for the old copy to go, quietly.
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        if let Ok(mut other) = UnixStream::connect(&path) {
            let _ = other.write_all(b"show\n");
        }
        crate::state::print_err("Edgehop is already running; showing its window.");
        return None;
    }
}

#[cfg(not(any(windows, unix)))]
pub fn single_instance(_restarting: bool) -> Option<InstanceGuard> {
    Some(InstanceGuard {})
}

#[cfg(unix)]
fn instance_socket() -> std::path::PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => std::path::PathBuf::from(dir).join("edgehop.sock"),
        None => crate::files::received_root().with_file_name("edgehop.sock"),
    }
}

impl InstanceGuard {
    /// Show the window when another copy of Edgehop is started.
    pub fn listen(&mut self, app: std::sync::Arc<AppState>) {
        #[cfg(unix)]
        if let Some(listener) = self.listener.as_ref().and_then(|l| l.try_clone().ok()) {
            let _ = std::thread::Builder::new().name("instance".into()).spawn(move || {
                for conn in listener.incoming().flatten() {
                    drop(conn);
                    log::info!("Edgehop was opened again; showing the window");
                    show_window(&app);
                }
            });
        }
        #[cfg(not(unix))]
        let _ = app;
    }
}

/// Windows Firewall: let other computers reach this copy of Edgehop on
/// private and domain networks (off: remove the rule). Needs admin.
#[cfg(windows)]
pub fn set_firewall_rule(on: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let netsh = |raw: String| {
        std::process::Command::new("netsh").raw_arg(raw).creation_flags(CREATE_NO_WINDOW).output()
    };
    let _ = netsh("advfirewall firewall delete rule name=\"Edgehop\"".into());
    let _ = netsh("advfirewall firewall delete rule name=\"kvmlink\"".into());
    if !on {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = netsh(format!(
        "advfirewall firewall add rule name=\"Edgehop\" dir=in action=allow program=\"{}\" enable=yes profile=private,domain",
        exe.display()
    ))
    .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("netsh: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

pub fn show_window(app: &AppState) {
    if CLOSE_TO_TRAY && !app.window_open.load(std::sync::atomic::Ordering::Relaxed) {
        app.request_window();
        return;
    }
    #[cfg(windows)]
    unsafe {
        use crate::win32::*;
        let title = wide(WINDOW_TITLE);
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
    }
    if let Some(ctx) = lk(&app.ctx).as_ref() {
        // Windows: also tell the window library it's visible again (it's applied
        // on the next frame, which the line above makes happen). It only acts
        // on changes to what it remembers, so without this, the next "hide"
        // looks like nothing to do and the X button does nothing.
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        if !HIDE_TO_TRAY {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
}

pub fn hide_window(ctx: &egui::Context) {
    if HIDE_TO_TRAY {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        // And hide it directly: the window may have been shown by another
        // copy of Edgehop (opened again from the desktop icon), which the
        // window library never hears about.
        #[cfg(windows)]
        unsafe {
            use crate::win32::*;
            let title = wide(WINDOW_TITLE);
            let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
            if !hwnd.is_null() {
                ShowWindow(hwnd, SW_HIDE);
            }
        }
        log::debug!("window hidden; Edgehop keeps running in the tray");
    } else {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
}

/// Monitors with model names, for display. Cached: the name lookup is slow.
pub fn monitors() -> Vec<crate::layout::Monitor> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, Vec<crate::layout::Monitor>)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, v)) = c.as_ref() {
        if at.elapsed() < Duration::from_secs(5) {
            return v.clone();
        }
    }
    let v = enumerate(true);
    *c = Some((Instant::now(), v.clone()));
    v
}

/// Monitor rectangles only: fast, used by the switching logic.
pub fn monitor_rects() -> Vec<crate::layout::Monitor> {
    #[cfg(target_os = "linux")]
    {
        // Asking the desktop runs a program: reuse the answer for a few seconds.
        use std::sync::Mutex;
        use std::time::{Duration, Instant};
        static CACHE: Mutex<Option<(Instant, Vec<crate::layout::Monitor>)>> = Mutex::new(None);
        static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let stale = {
            let c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
            match c.as_ref() {
                Some((at, v)) if at.elapsed() < Duration::from_secs(3) => return v.clone(),
                Some((_, v)) => Some(v.clone()),
                None => None,
            }
        };
        let refresh = || {
            let v = enumerate(false);
            *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v.clone()));
            v
        };
        match stale {
            // Callers include the input path, holding the switching state's
            // lock: hand back what's known and look again in the background,
            // never wait here for an external program.
            Some(v) => {
                if !REFRESHING.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    std::thread::spawn(move || {
                        refresh();
                        REFRESHING.store(false, std::sync::atomic::Ordering::Relaxed);
                    });
                }
                v
            }
            None => refresh(), // the very first time
        }
    }
    #[cfg(not(target_os = "linux"))]
    enumerate(false)
}

fn enumerate(with_names: bool) -> Vec<crate::layout::Monitor> {
    #[cfg(windows)]
    {
        use crate::layout::{Monitor, Rect};
        let mut v: Vec<Monitor> = crate::win32::monitors(with_names)
            .into_iter()
            .map(|(r, label, primary)| Monitor {
                rect: Rect::new(r.left as f64, r.top as f64, r.right as f64, r.bottom as f64),
                label,
                primary,
            })
            .collect();
        v.sort_by(|a, b| a.rect.l.partial_cmp(&b.rect.l).unwrap_or(std::cmp::Ordering::Equal));
        v
    }
    #[cfg(target_os = "macos")]
    {
        let _ = with_names;
        mac_monitors()
    }
    #[cfg(target_os = "linux")]
    {
        let _ = with_names;
        let mut v = linux_monitors();
        v.sort_by(|a, b| a.rect.l.partial_cmp(&b.rect.l).unwrap_or(std::cmp::Ordering::Equal));
        v
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = with_names;
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
fn mac_monitors() -> Vec<crate::layout::Monitor> {
    use crate::layout::{Monitor, Rect};
    #[repr(C)]
    struct P {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    struct S {
        w: f64,
        h: f64,
    }
    #[repr(C)]
    struct R {
        o: P,
        s: S,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGDisplayBounds(display: u32) -> R;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayIsBuiltin(display: u32) -> u32;
    }
    let mut ids = [0u32; 16];
    let mut n = 0u32;
    if unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) } != 0 {
        return Vec::new();
    }
    let main = unsafe { CGMainDisplayID() };
    let mut v: Vec<Monitor> = ids[..n as usize]
        .iter()
        .map(|&id| {
            let b = unsafe { CGDisplayBounds(id) };
            Monitor {
                rect: Rect::xywh(b.o.x, b.o.y, b.s.w, b.s.h),
                label: if unsafe { CGDisplayIsBuiltin(id) } != 0 { "Built-in".into() } else { String::new() },
                primary: id == main,
            }
        })
        .collect();
    v.sort_by(|a, b| a.rect.l.partial_cmp(&b.rect.l).unwrap_or(std::cmp::Ordering::Equal));
    for (i, m) in v.iter_mut().enumerate() {
        if m.label.is_empty() {
            m.label = format!("Display {}", i + 1);
        }
    }
    v
}

/// Remove terminal colour codes ("ESC [ ... letter").
#[cfg(target_os = "linux")]
fn strip_ansi(line: &str) -> String {
    let mut clean = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            clean.push(c);
        }
    }
    clean
}

/// Monitors in desktop (logical) coordinates: KDE's kscreen-doctor, else xrandr.
#[cfg(target_os = "linux")]
fn linux_monitors() -> Vec<crate::layout::Monitor> {
    if let Ok(out) = std::process::Command::new("kscreen-doctor").arg("-o").output() {
        let v = parse_kscreen(&String::from_utf8_lossy(&out.stdout));
        if !v.is_empty() {
            return v;
        }
    }
    if let Ok(out) = std::process::Command::new("xrandr").arg("--current").output() {
        return parse_xrandr(&String::from_utf8_lossy(&out.stdout));
    }
    Vec::new()
}

/// `kscreen-doctor -o`: "Output: 1 eDP-1 ..." blocks with "enabled",
/// "priority 1" and "Geometry: 0,0 1520x1014" lines.
#[cfg(target_os = "linux")]
pub fn parse_kscreen(text: &str) -> Vec<crate::layout::Monitor> {
    use crate::layout::{Monitor, Rect};
    struct Out {
        name: String,
        enabled: bool,
        primary: bool,
        rect: Option<Rect>,
    }
    let mut outs: Vec<Out> = Vec::new();
    for raw in text.lines() {
        let line = strip_ansi(raw);
        let words: Vec<&str> = line.split_whitespace().collect();
        if let Some(i) = words.iter().position(|w| *w == "Output:") {
            outs.push(Out {
                name: words.get(i + 2).unwrap_or(&"").to_string(),
                enabled: true,
                primary: false,
                rect: None,
            });
        }
        let Some(o) = outs.last_mut() else { continue };
        for (i, w) in words.iter().enumerate() {
            match *w {
                "disabled" => o.enabled = false,
                "priority" if words.get(i + 1) == Some(&"1") => o.primary = true,
                "primary" => o.primary = true,
                _ => {}
            }
        }
        if let Some(g) = line.split("Geometry:").nth(1) {
            let n: Vec<f64> = g
                .split(|c: char| !c.is_ascii_digit() && c != '-')
                .filter_map(|p| p.parse().ok())
                .collect();
            if let [x, y, w, h] = n[..] {
                o.rect = Some(Rect::xywh(x, y, w, h));
            }
        }
    }
    outs.into_iter()
        .filter(|o| o.enabled)
        .filter_map(|o| o.rect.map(|rect| Monitor { rect, label: o.name, primary: o.primary }))
        .collect()
}

/// `xrandr --current`: "HDMI-1 connected primary 1920x1080+1920+0 (...)".
#[cfg(target_os = "linux")]
pub fn parse_xrandr(text: &str) -> Vec<crate::layout::Monitor> {
    use crate::layout::{Monitor, Rect};
    text.lines()
        .filter(|l| l.contains(" connected"))
        .filter_map(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            let geo = words.iter().find(|w| w.contains('x') && w.contains('+'))?;
            let (size, pos) = geo.split_once('+')?;
            let (w, h) = size.split_once('x')?;
            let (x, y) = pos.split_once('+')?;
            Some(Monitor {
                rect: Rect::xywh(x.parse().ok()?, y.parse().ok()?, w.parse().ok()?, h.parse().ok()?),
                label: words[0].to_string(),
                primary: words.contains(&"primary"),
            })
        })
        .collect()
}

/// The folder a drop at (x, y) should land in: the desktop or an open
/// File Explorer window. None for anything else.
pub fn folder_at(x: i32, y: i32) -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        use crate::win32::{window_at, UnderCursor};
        match window_at(x, y) {
            UnderCursor::Desktop => directories::UserDirs::new().and_then(|u| u.desktop_dir().map(|d| d.to_path_buf())),
            UnderCursor::Explorer(hwnd) => explorer_folder(hwnd),
            UnderCursor::Other => None,
        }
    }
    #[cfg(target_os = "macos")]
    return crate::mac_files::folder_at(x, y);
    #[cfg(target_os = "linux")]
    {
        let _ = (x, y);
        None
    }
}

#[cfg(windows)]
fn explorer_folder(hwnd: isize) -> Option<std::path::PathBuf> {
    use std::os::windows::process::CommandExt;
    let script = format!(
        "$sh = New-Object -ComObject Shell.Application; foreach ($w in $sh.Windows()) {{ try {{ if ([long]$w.HWND -eq {hwnd}) {{ $w.Document.Folder.Self.Path; break }} }} catch {{}} }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    let path = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    path.is_dir().then_some(path)
}

/// Undo a hidden cursor (startup, shutdown, crash).
pub fn restore_cursor() {
    #[cfg(windows)]
    crate::win32::show_cursor();
}

/// Show/hide this computer's cursor from any thread (Windows).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn set_cursor_visible(visible: bool) {
    #[cfg(windows)]
    {
        use std::sync::{mpsc, Mutex, OnceLock};
        static TX: OnceLock<Mutex<mpsc::Sender<bool>>> = OnceLock::new();
        let tx = TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<bool>();
            std::thread::spawn(move || {
                let mut hidden = false;
                for show in rx {
                    if show && hidden {
                        crate::win32::show_cursor();
                        hidden = false;
                    } else if !show && !hidden {
                        crate::win32::hide_cursor();
                        hidden = true;
                    }
                }
            });
            Mutex::new(tx)
        });
        let _ = crate::state::lk(tx).send(visible);
    }
    #[cfg(not(windows))]
    let _ = visible;
}

/// Desktop integration used by the client.
pub trait DesktopHooks: Send + Sync {
    /// The mouse moved to another computer.
    fn away(&self, _hide_pointer: bool, _dim_percent: Option<u8>) {}
    /// The mouse is back on this computer.
    fn back(&self) {}
    /// Folder under the pointer that dropped files should go into.
    fn drop_folder(&self) -> Option<std::path::PathBuf> {
        None
    }
    /// Is the drag under the pointer coming from a file manager? None = can't tell.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn drag_source_ok(&self) -> Option<bool> {
        None
    }
}

pub fn client_hooks() -> Option<std::sync::Arc<dyn DesktopHooks>> {
    #[cfg(target_os = "linux")]
    return crate::kde::Kde::connect().map(|k| std::sync::Arc::new(k) as std::sync::Arc<dyn DesktopHooks>);
    #[cfg(windows)]
    return Some(std::sync::Arc::new(WinHooks));
    #[cfg(target_os = "macos")]
    return Some(std::sync::Arc::new(MacHooks(std::sync::atomic::AtomicBool::new(false))));
    #[allow(unreachable_code)]
    None
}

/// The app's bundle identifier (Info.plist): what macOS files permissions under.
#[cfg(target_os = "macos")]
pub const MAC_BUNDLE_ID: &str = "com.edgehop.app";

/// The Edgehop.app this binary runs from, if any.
#[cfg(target_os = "macos")]
pub fn mac_app_bundle() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?.to_path_buf();
    (app.extension().is_some_and(|e| e == "app")).then_some(app)
}

/// Forget every input permission macOS holds for Edgehop, so the next launch
/// asks afresh. Newer macOS has no way to remove an app from those lists, and
/// a permission granted to an older copy can look "on" while macOS refuses the
/// new one. Returns (service, succeeded) for each.
#[cfg(target_os = "macos")]
pub fn reset_mac_permissions() -> Vec<(&'static str, bool)> {
    let mut done = Vec::new();
    for service in ["Accessibility", "ListenEvent", "PostEvent"] {
        let ok = std::process::Command::new("tccutil")
            .args(["reset", service, MAC_BUNDLE_ID])
            .status()
            .is_ok_and(|s| s.success());
        log::info!("tccutil reset {service} {MAC_BUNDLE_ID}: {}", if ok { "done" } else { "failed" });
        done.push((service, ok));
    }
    // Downloaded apps can run from a hidden temporary copy, which muddles permissions.
    if let Some(app) = mac_app_bundle() {
        let _ = std::process::Command::new("xattr").args(["-dr", "com.apple.quarantine"]).arg(&app).status();
    }
    done
}

/// Quit and open Edgehop.app again (permissions are checked per launch).
#[cfg(target_os = "macos")]
pub fn relaunch_mac_app() -> ! {
    match mac_app_bundle() {
        Some(app) => {
            log::info!("relaunching {}", app.display());
            crate::state::flush_logs();
            let _ = std::process::Command::new("sh").args(["-c", "sleep 1; open -n \"$0\""]).arg(&app).spawn();
            std::process::exit(0);
        }
        None => crate::recovery::restart_self("relaunch after resetting permissions"),
    }
}

/// Mac client: hide the pointer while the mouse is on another computer.
#[cfg(target_os = "macos")]
struct MacHooks(std::sync::atomic::AtomicBool);

#[cfg(target_os = "macos")]
impl DesktopHooks for MacHooks {
    fn away(&self, hide_pointer: bool, _dim: Option<u8>) {
        use std::sync::atomic::Ordering;
        if hide_pointer && !self.0.swap(true, Ordering::Relaxed) {
            crate::server::capture::set_pointer_visible(false);
        }
    }
    fn back(&self) {
        use std::sync::atomic::Ordering;
        if self.0.swap(false, Ordering::Relaxed) {
            crate::server::capture::set_pointer_visible(true);
        }
    }
}

#[cfg(windows)]
struct WinHooks;

#[cfg(windows)]
impl DesktopHooks for WinHooks {
    fn away(&self, hide_pointer: bool, _dim: Option<u8>) {
        if hide_pointer {
            set_cursor_visible(false);
        }
    }
    fn back(&self) {
        set_cursor_visible(true);
    }
    fn drop_folder(&self) -> Option<std::path::PathBuf> {
        let (x, y) = crate::win32::cursor_pos();
        folder_at(x, y)
    }
    fn drag_source_ok(&self) -> Option<bool> {
        let src = crate::win32::drag_source();
        log::info!("drag source: {src:?}");
        Some(src.can_grab())
    }
}

#[cfg(target_os = "linux")]
impl DesktopHooks for crate::kde::Kde {
    fn away(&self, hide_pointer: bool, dim_percent: Option<u8>) {
        crate::kde::Kde::away(self, hide_pointer, dim_percent)
    }
    fn back(&self) {
        crate::kde::Kde::back(self)
    }
    fn drop_folder(&self) -> Option<std::path::PathBuf> {
        let w = self.window_under_pointer();
        log::info!("drop target window: {w:?}");
        w.as_ref().and_then(crate::kde::drop_folder)
    }
    fn drag_source_ok(&self) -> Option<bool> {
        let w = self.window_under_pointer();
        log::info!("drag source window: {w:?}");
        w.map(|w| crate::kde::is_file_manager(&w))
    }
}

/// The system in one line (OS version, desktop session), for the log and diagnostics.
pub fn environment() -> String {
    #[cfg(target_os = "macos")]
    {
        let v = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        format!(
            "macOS {v} · Accessibility {}",
            match crate::inject::permission() {
                Some(true) => "allowed",
                _ => "not allowed",
            }
        )
    }
    #[cfg(target_os = "linux")]
    {
        let os = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
            .unwrap_or_else(|| "Linux".into());
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default();
        let var = |k: &str| std::env::var(k).unwrap_or_else(|_| "-".into());
        format!(
            "{os} · kernel {kernel} · session {} · desktop {} · WAYLAND_DISPLAY {} · DISPLAY {}",
            var("XDG_SESSION_TYPE"),
            var("XDG_CURRENT_DESKTOP"),
            var("WAYLAND_DISPLAY"),
            var("DISPLAY")
        )
    }
    #[cfg(windows)]
    {
        #[repr(C)]
        struct OsVersionInfo {
            size: u32,
            major: u32,
            minor: u32,
            build: u32,
            platform: u32,
            csd: [u16; 128],
        }
        #[link(name = "ntdll")]
        extern "system" {
            fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
        }
        let mut v = OsVersionInfo { size: std::mem::size_of::<OsVersionInfo>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
        unsafe { RtlGetVersion(&mut v) };
        format!("Windows {}.{}.{}", v.major, v.minor, v.build)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        std::env::consts::OS.to_string()
    }
}

/// Local wall-clock time: (hour, minute, second, millisecond).
pub fn local_clock() -> (u32, u32, u32, u32) {
    let (_, _, _, h, m, s, ms) = local_datetime_parts();
    (h, m, s, ms)
}

/// "2026-11-10 14:05:22" in local time.
pub fn local_datetime() -> String {
    let (y, mo, d, h, m, s, _) = local_datetime_parts();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

fn local_datetime_parts() -> (u32, u32, u32, u32, u32, u32, u32) {
    #[cfg(windows)]
    {
        let t = crate::win32::local_time();
        (t.wYear as u32, t.wMonth as u32, t.wDay as u32, t.wHour as u32, t.wMinute as u32, t.wSecond as u32, t.wMilliseconds as u32)
    }
    #[cfg(unix)]
    {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let secs = now.as_secs() as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            libc::localtime_r(&secs, &mut tm);
        }
        (
            (tm.tm_year + 1900) as u32,
            (tm.tm_mon + 1) as u32,
            tm.tm_mday as u32,
            tm.tm_hour as u32,
            tm.tm_min as u32,
            tm.tm_sec as u32,
            now.subsec_millis(),
        )
    }
}
