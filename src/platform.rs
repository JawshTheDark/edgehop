//! Small OS-specific helpers: DPI awareness and showing/hiding the window.
use crate::state::{lk, AppState};
use eframe::egui;

/// Distinct enough that no Explorer window (e.g. the "Edgehop" install folder) matches.
pub const WINDOW_TITLE: &str = "Edgehop — keyboard & mouse sharing";

/// On Windows we can truly hide to the tray and restore via Win32.
/// Elsewhere the window minimizes instead (restoring a hidden window is unreliable).
pub const HIDE_TO_TRAY: bool = cfg!(windows);

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

pub fn show_window(app: &AppState) {
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
        if !HIDE_TO_TRAY {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
}

pub fn hide_window(ctx: &egui::Context) {
    if HIDE_TO_TRAY {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
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
#[cfg_attr(not(windows), allow(dead_code))]
pub fn monitor_rects() -> Vec<crate::layout::Monitor> {
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
    #[cfg(not(windows))]
    {
        let _ = with_names;
        Vec::new()
    }
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
    #[cfg(not(windows))]
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
    fn drag_source_ok(&self) -> Option<bool> {
        None
    }
}

pub fn client_hooks() -> Option<std::sync::Arc<dyn DesktopHooks>> {
    #[cfg(target_os = "linux")]
    return crate::kde::Kde::connect().map(|k| std::sync::Arc::new(k) as std::sync::Arc<dyn DesktopHooks>);
    #[cfg(windows)]
    return Some(std::sync::Arc::new(WinHooks));
    #[allow(unreachable_code)]
    None
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
