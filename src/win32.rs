//! Minimal hand-written Win32 bindings (keeps us independent of binding-crate churn).
#![allow(non_snake_case, non_camel_case_types, dead_code, clippy::upper_case_acronyms)]
use std::ffi::c_void;

pub type HANDLE = *mut c_void;
pub type HOOKPROC = unsafe extern "system" fn(i32, usize, isize) -> isize;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub struct MONITORINFOEXW {
    pub cbSize: u32,
    pub rcMonitor: RECT,
    pub rcWork: RECT,
    pub dwFlags: u32,
    pub szDevice: [u16; 32],
}

pub type MONITORENUMPROC = unsafe extern "system" fn(HANDLE, HANDLE, *mut RECT, isize) -> i32;
pub const MONITORINFOF_PRIMARY: u32 = 1;

#[repr(C)]
pub struct MSG {
    pub hwnd: HANDLE,
    pub message: u32,
    pub wParam: usize,
    pub lParam: isize,
    pub time: u32,
    pub pt: POINT,
    pub lPrivate: u32,
}

#[repr(C)]
pub struct MSLLHOOKSTRUCT {
    pub pt: POINT,
    pub mouseData: u32,
    pub flags: u32,
    pub time: u32,
    pub dwExtraInfo: usize,
}

#[repr(C)]
pub struct KBDLLHOOKSTRUCT {
    pub vkCode: u32,
    pub scanCode: u32,
    pub flags: u32,
    pub time: u32,
    pub dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MOUSEINPUT {
    pub dx: i32,
    pub dy: i32,
    pub mouseData: u32,
    pub dwFlags: u32,
    pub time: u32,
    pub dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct KEYBDINPUT {
    pub wVk: u16,
    pub wScan: u16,
    pub dwFlags: u32,
    pub time: u32,
    pub dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union INPUT_U {
    pub mi: MOUSEINPUT,
    pub ki: KEYBDINPUT,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct INPUT {
    pub type_: u32,
    pub u: INPUT_U,
}

pub const WH_KEYBOARD_LL: i32 = 13;
pub const WH_MOUSE_LL: i32 = 14;
pub const WM_QUIT: u32 = 0x0012;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSKEYUP: u32 = 0x0105;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_MBUTTONDOWN: u32 = 0x0207;
pub const WM_MBUTTONUP: u32 = 0x0208;
pub const WM_MOUSEWHEEL: u32 = 0x020A;
pub const WM_XBUTTONDOWN: u32 = 0x020B;
pub const WM_XBUTTONUP: u32 = 0x020C;
pub const WM_MOUSEHWHEEL: u32 = 0x020E;
pub const LLMHF_INJECTED: u32 = 0x01;
pub const LLKHF_EXTENDED: u32 = 0x01;
pub const LLKHF_INJECTED: u32 = 0x10;

pub const INPUT_MOUSE: u32 = 0;
pub const INPUT_KEYBOARD: u32 = 1;
pub const MOUSEEVENTF_MOVE: u32 = 0x0001;
pub const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
pub const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
pub const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
pub const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
pub const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
pub const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
pub const MOUSEEVENTF_XDOWN: u32 = 0x0080;
pub const MOUSEEVENTF_XUP: u32 = 0x0100;
pub const MOUSEEVENTF_WHEEL: u32 = 0x0800;
pub const MOUSEEVENTF_HWHEEL: u32 = 0x1000;
pub const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;
pub const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
pub const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
pub const KEYEVENTF_KEYUP: u32 = 0x0002;
pub const KEYEVENTF_SCANCODE: u32 = 0x0008;

pub const SM_CXSCREEN: i32 = 0;
pub const SM_CYSCREEN: i32 = 1;
pub const SM_XVIRTUALSCREEN: i32 = 76;
pub const SM_YVIRTUALSCREEN: i32 = 77;
pub const SM_CXVIRTUALSCREEN: i32 = 78;
pub const SM_CYVIRTUALSCREEN: i32 = 79;
pub const SW_SHOW: i32 = 5;
pub const SW_RESTORE: i32 = 9;

#[link(name = "user32")]
extern "system" {
    pub fn SetWindowsHookExW(idHook: i32, lpfn: HOOKPROC, hmod: HANDLE, dwThreadId: u32) -> HANDLE;
    pub fn UnhookWindowsHookEx(hhk: HANDLE) -> i32;
    pub fn CallNextHookEx(hhk: HANDLE, nCode: i32, wParam: usize, lParam: isize) -> isize;
    pub fn GetMessageW(lpMsg: *mut MSG, hWnd: HANDLE, min: u32, max: u32) -> i32;
    pub fn TranslateMessage(lpMsg: *const MSG) -> i32;
    pub fn DispatchMessageW(lpMsg: *const MSG) -> isize;
    pub fn PostThreadMessageW(idThread: u32, msg: u32, wParam: usize, lParam: isize) -> i32;
    pub fn SetCursorPos(x: i32, y: i32) -> i32;
    pub fn GetCursorPos(p: *mut POINT) -> i32;
    pub fn GetSystemMetrics(index: i32) -> i32;
    pub fn SendInput(n: u32, inputs: *const INPUT, size: i32) -> u32;
    pub fn FindWindowW(class: *const u16, title: *const u16) -> HANDLE;
    pub fn ShowWindow(hwnd: HANDLE, cmd: i32) -> i32;
    pub fn SetForegroundWindow(hwnd: HANDLE) -> i32;
    pub fn EnumDisplayMonitors(hdc: HANDLE, clip: *const RECT, f: MONITORENUMPROC, data: isize) -> i32;
    pub fn GetMonitorInfoW(hmon: HANDLE, info: *mut MONITORINFOEXW) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> HANDLE;
    pub fn GetCurrentThreadId() -> u32;
    pub fn LoadLibraryW(name: *const u16) -> HANDLE;
    pub fn GetProcAddress(module: HANDLE, name: *const u8) -> *mut c_void;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Bounds of the whole virtual desktop: (left, top, width, height).
pub fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    }
}

// ---- Display names (QueryDisplayConfig) ----

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct LUID {
    low: u32,
    high: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PATH_SOURCE {
    adapter: LUID,
    id: u32,
    mode_idx: u32,
    status: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PATH_TARGET {
    adapter: LUID,
    id: u32,
    mode_idx: u32,
    tech: u32,
    rotation: u32,
    scaling: u32,
    refresh_num: u32,
    refresh_den: u32,
    scanline: u32,
    available: i32,
    status: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PATH_INFO {
    source: PATH_SOURCE,
    target: PATH_TARGET,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DC_HEADER {
    kind: u32,
    size: u32,
    adapter: LUID,
    id: u32,
}

#[repr(C)]
struct SOURCE_NAME {
    header: DC_HEADER,
    gdi_name: [u16; 32],
}

#[repr(C)]
struct TARGET_NAME {
    header: DC_HEADER,
    flags: u32,
    tech: u32,
    edid_mfg: u16,
    edid_product: u16,
    connector: u32,
    friendly: [u16; 64],
    path: [u16; 128],
}

#[link(name = "user32")]
extern "system" {
    fn GetDisplayConfigBufferSizes(flags: u32, paths: *mut u32, modes: *mut u32) -> i32;
    fn QueryDisplayConfig(flags: u32, np: *mut u32, paths: *mut PATH_INFO, nm: *mut u32, modes: *mut [u8; 64], topo: *mut u32) -> i32;
    fn DisplayConfigGetDeviceInfo(header: *mut DC_HEADER) -> i32;
}

fn wstr(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// GDI device name (`\\.\DISPLAY1`) -> monitor model ("DELL S2421H").
fn friendly_names() -> Vec<(String, String)> {
    const QDC_ONLY_ACTIVE_PATHS: u32 = 2;
    let mut out = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != 0 {
            return out;
        }
        let mut paths = vec![PATH_INFO::default(); np as usize];
        let mut modes = vec![[0u8; 64]; nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), std::ptr::null_mut()) != 0 {
            return out;
        }
        for p in &paths[..np as usize] {
            let mut src = SOURCE_NAME {
                header: DC_HEADER { kind: 1, size: std::mem::size_of::<SOURCE_NAME>() as u32, adapter: p.source.adapter, id: p.source.id },
                gdi_name: [0; 32],
            };
            let mut tgt = TARGET_NAME {
                header: DC_HEADER { kind: 2, size: std::mem::size_of::<TARGET_NAME>() as u32, adapter: p.target.adapter, id: p.target.id },
                flags: 0,
                tech: 0,
                edid_mfg: 0,
                edid_product: 0,
                connector: 0,
                friendly: [0; 64],
                path: [0; 128],
            };
            if DisplayConfigGetDeviceInfo(&mut src.header) == 0 && DisplayConfigGetDeviceInfo(&mut tgt.header) == 0 {
                out.push((wstr(&src.gdi_name), wstr(&tgt.friendly)));
            }
        }
    }
    out
}

/// Every monitor: (rect, model name or resolution, is primary).
pub fn monitors(with_names: bool) -> Vec<(RECT, String, bool)> {
    unsafe extern "system" fn cb(h: HANDLE, _dc: HANDLE, _r: *mut RECT, data: isize) -> i32 {
        let out = &mut *(data as *mut Vec<(RECT, String, bool)>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(h, &mut info) != 0 {
            out.push((info.rcMonitor, wstr(&info.szDevice), info.dwFlags & MONITORINFOF_PRIMARY != 0));
        }
        1
    }
    let mut out: Vec<(RECT, String, bool)> = Vec::new();
    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), cb, &mut out as *mut _ as isize);
    }
    // QueryDisplayConfig is slow; callers on the input path skip it.
    let names = if with_names { friendly_names() } else { Vec::new() };
    for (_, label, _) in out.iter_mut() {
        *label = names
            .iter()
            .find(|(gdi, name)| gdi == label && !name.trim().is_empty())
            .map(|(_, name)| name.trim().to_string())
            .unwrap_or_default();
    }
    out
}

pub fn primary_center() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN) / 2, GetSystemMetrics(SM_CYSCREEN) / 2) }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn time_monitor_lookups() {
        for names in [false, true] {
            let t = std::time::Instant::now();
            let mut worst = std::time::Duration::ZERO;
            for _ in 0..10 {
                let s = std::time::Instant::now();
                let m = super::monitors(names);
                assert!(!m.is_empty());
                worst = worst.max(s.elapsed());
            }
            println!("with_names={names}: avg {:?}, worst {:?}", t.elapsed() / 10, worst);
        }
    }
}

// ---- Clipboard file lists, foreground process, key taps (file drag) ----

pub const CF_HDROP: u32 = 15;
const GMEM_MOVEABLE: u32 = 0x0002;
const GMEM_ZEROINIT: u32 = 0x0040;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(owner: HANDLE) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> HANDLE;
    fn SetClipboardData(format: u32, mem: HANDLE) -> HANDLE;
    pub fn GetClipboardSequenceNumber() -> u32;
    fn GetForegroundWindow() -> HANDLE;
    fn GetWindowThreadProcessId(hwnd: HANDLE, pid: *mut u32) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> HANDLE;
    fn GlobalLock(mem: HANDLE) -> *mut std::ffi::c_void;
    fn GlobalUnlock(mem: HANDLE) -> i32;
    fn GlobalFree(mem: HANDLE) -> HANDLE;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> HANDLE;
    fn QueryFullProcessImageNameW(process: HANDLE, flags: u32, name: *mut u16, size: *mut u32) -> i32;
    fn CloseHandle(h: HANDLE) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn DragQueryFileW(drop: HANDLE, index: u32, file: *mut u16, len: u32) -> u32;
}

fn open_clipboard() -> bool {
    for _ in 0..20 {
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

/// Files currently on the clipboard (what Explorer's Copy puts there).
pub fn clipboard_files() -> Option<Vec<std::path::PathBuf>> {
    if !open_clipboard() {
        return None;
    }
    let out = unsafe {
        let h = GetClipboardData(CF_HDROP);
        let files = if h.is_null() { Vec::new() } else { hdrop_paths(h) };
        CloseClipboard();
        files
    };
    (!out.is_empty()).then_some(out)
}

/// Paths in an HDROP (clipboard or drag-and-drop data).
pub fn hdrop_paths(h: HANDLE) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    unsafe {
        let n = DragQueryFileW(h, u32::MAX, std::ptr::null_mut(), 0);
        for i in 0..n {
            let len = DragQueryFileW(h, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            DragQueryFileW(h, i, buf.as_mut_ptr(), buf.len() as u32);
            out.push(std::path::PathBuf::from(wstr(&buf)));
        }
    }
    out
}

/// Put files on the clipboard so Explorer's Paste copies them.
pub fn set_clipboard_files(paths: &[std::path::PathBuf]) -> bool {
    // DROPFILES header (20 bytes) followed by NUL-separated wide paths and a final NUL.
    let mut wide: Vec<u16> = Vec::new();
    for p in paths {
        wide.extend(p.as_os_str().to_string_lossy().encode_utf16());
        wide.push(0);
    }
    wide.push(0);
    let bytes = 20 + wide.len() * 2;
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes);
        if mem.is_null() {
            return false;
        }
        let p = GlobalLock(mem) as *mut u8;
        if p.is_null() {
            GlobalFree(mem);
            return false;
        }
        std::ptr::write_unaligned(p as *mut u32, 20); // pFiles
        std::ptr::write_unaligned(p.add(16) as *mut i32, 1); // fWide
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, p.add(20), wide.len() * 2);
        GlobalUnlock(mem);
        if !open_clipboard() {
            GlobalFree(mem);
            return false;
        }
        EmptyClipboard();
        let ok = !SetClipboardData(CF_HDROP, mem).is_null();
        if !ok {
            GlobalFree(mem);
        }
        CloseClipboard();
        ok
    }
}

/// Is File Explorer (or the desktop) the active window?
pub fn foreground_is_explorer() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        ok && String::from_utf16_lossy(&buf[..len as usize]).to_lowercase().ends_with(r"\explorer.exe")
    }
}

/// Inject virtual-key presses: [(vk, down)].
pub fn tap_keys(keys: &[(u16, bool)]) {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, down)| INPUT {
            type_: INPUT_KEYBOARD,
            u: INPUT_U {
                ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if down { 0 } else { KEYEVENTF_KEYUP }, time: 0, dwExtraInfo: 0 },
            },
        })
        .collect();
    unsafe {
        SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
    }
}

/// Inject a left-button release (Windows saw the press but we swallowed the release).
pub fn left_up() {
    let input = INPUT {
        type_: INPUT_MOUSE,
        u: INPUT_U { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: MOUSEEVENTF_LEFTUP, time: 0, dwExtraInfo: 0 } },
    };
    unsafe {
        SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
    }
}


#[link(name = "user32")]
extern "system" {
    fn WindowFromPoint(p: POINT) -> HANDLE;
    fn GetAncestor(hwnd: HANDLE, flags: u32) -> HANDLE;
    fn GetClassNameW(hwnd: HANDLE, buf: *mut u16, len: i32) -> i32;
}

pub enum UnderCursor {
    Desktop,
    /// A File Explorer window (top-level HWND).
    Explorer(isize),
    Other,
}

/// What top-level window is at (x, y)?
pub fn window_at(x: i32, y: i32) -> UnderCursor {
    unsafe {
        let w = WindowFromPoint(POINT { x, y });
        if w.is_null() {
            return UnderCursor::Other;
        }
        let root = GetAncestor(w, 2); // GA_ROOT
        let mut buf = [0u16; 64];
        let n = GetClassNameW(root, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
        match String::from_utf16_lossy(&buf[..n]).as_str() {
            "Progman" | "WorkerW" => UnderCursor::Desktop,
            "CabinetWClass" | "ExploreWClass" => UnderCursor::Explorer(root as isize),
            _ => UnderCursor::Other,
        }
    }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        GetCursorPos(&mut p);
    }
    (p.x, p.y)
}


#[link(name = "user32")]
extern "system" {
    fn CreateCursor(inst: HANDLE, xhot: i32, yhot: i32, w: i32, h: i32, and: *const u8, xor: *const u8) -> HANDLE;
    fn CopyIcon(icon: HANDLE) -> HANDLE;
    fn DestroyCursor(cursor: HANDLE) -> i32;
    fn SetSystemCursor(cursor: HANDLE, id: u32) -> i32;
    fn SystemParametersInfoW(action: u32, param: u32, pv: *mut std::ffi::c_void, ini: u32) -> i32;
}

/// Every standard system cursor (arrow, I-beam, hand, resize, busy, ...).
const SYSTEM_CURSORS: [u32; 17] =
    [32512, 32513, 32514, 32515, 32516, 32631, 32640, 32641, 32642, 32643, 32644, 32645, 32646, 32648, 32649, 32650, 32651];

/// Make the cursor invisible everywhere (swap every system cursor for a blank one).
pub fn hide_cursor() {
    unsafe {
        let and = [0xFFu8; 128];
        let xor = [0u8; 128];
        let blank = CreateCursor(GetModuleHandleW(std::ptr::null()), 0, 0, 32, 32, and.as_ptr(), xor.as_ptr());
        if blank.is_null() {
            return;
        }
        for id in SYSTEM_CURSORS {
            let c = CopyIcon(blank);
            if !c.is_null() {
                SetSystemCursor(c, id);
            }
        }
        DestroyCursor(blank);
    }
}

/// Reload the user's cursor scheme.
pub fn show_cursor() {
    const SPI_SETCURSORS: u32 = 0x57;
    unsafe {
        SystemParametersInfoW(SPI_SETCURSORS, 0, std::ptr::null_mut(), 0);
    }
}


/// Top-level window that currently has focus.
pub fn foreground_root() -> isize {
    unsafe { GetAncestor(GetForegroundWindow(), 2) as isize }
}


#[repr(C)]
struct GUITHREADINFO {
    cbSize: u32,
    flags: u32,
    hwndActive: HANDLE,
    hwndFocus: HANDLE,
    hwndCapture: HANDLE,
    hwndMenuOwner: HANDLE,
    hwndMoveSize: HANDLE,
    hwndCaret: HANDLE,
    rcCaret: RECT,
}

#[link(name = "user32")]
extern "system" {
    fn GetGUIThreadInfo(thread: u32, info: *mut GUITHREADINFO) -> i32;
}

const GUI_INMOVESIZE: u32 = 0x2;

/// Executable file name ("explorer.exe") of the process owning `hwnd`.
pub fn process_of(hwnd: HANDLE) -> String {
    unsafe {
        if hwnd.is_null() {
            return String::new();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit('\\').next().unwrap_or("").to_lowercase()
    }
}

pub fn class_of(hwnd: HANDLE) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// What Windows says about the app a drag is coming from.
#[derive(Debug)]
pub struct DragSource {
    /// Program of the active window.
    pub app: String,
    /// Window class of the active window.
    pub class: String,
    /// Program holding the mouse capture (the drag loop runs there).
    pub capture_app: String,
    /// The user is moving or resizing a window, not dragging content.
    pub moving_window: bool,
    pub root: isize,
}

pub fn drag_source() -> DragSource {
    unsafe {
        let mut gi: GUITHREADINFO = std::mem::zeroed();
        gi.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        let have = GetGUIThreadInfo(0, &mut gi) != 0;
        let active = if have && !gi.hwndActive.is_null() { gi.hwndActive } else { GetForegroundWindow() };
        let root = GetAncestor(active, 2);
        DragSource {
            app: process_of(root),
            class: class_of(root),
            capture_app: if have { process_of(gi.hwndCapture) } else { String::new() },
            moving_window: have && (gi.flags & GUI_INMOVESIZE != 0 || !gi.hwndMoveSize.is_null()),
            root: root as isize,
        }
    }
}

impl DragSource {
    /// Terminals: Ctrl+C there may stop a running program, so never press it.
    pub fn is_terminal(&self) -> bool {
        const CLASSES: [&str; 3] = ["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS", "PseudoConsoleWindow"];
        const APPS: [&str; 10] = [
            "windowsterminal.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "conhost.exe", "openconsole.exe",
            "wezterm-gui.exe", "alacritty.exe", "mintty.exe", "putty.exe",
        ];
        CLASSES.contains(&self.class.as_str())
            || APPS.contains(&self.app.as_str())
            || APPS.contains(&self.capture_app.as_str())
    }
    /// Safe to press Esc + Ctrl+C to learn what's being dragged.
    pub fn can_grab(&self) -> bool {
        !self.moving_window && !self.is_terminal()
    }
}


#[repr(C)]
struct LASTINPUTINFO {
    cbSize: u32,
    dwTime: u32,
}

#[link(name = "user32")]
extern "system" {
    fn GetLastInputInfo(info: *mut LASTINPUTINFO) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetTickCount() -> u32;
}

/// Tick count of the last keyboard/mouse input anywhere in this session.
pub fn last_input_tick() -> u32 {
    let mut li = LASTINPUTINFO { cbSize: 8, dwTime: 0 };
    unsafe {
        GetLastInputInfo(&mut li);
    }
    li.dwTime
}
