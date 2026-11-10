//! Windows: learn what a drag carries by being a real OLE drop target.
//!
//! While a left-drag is pushed against a screen edge that leads to another
//! computer, an invisible strip is placed along that edge. When the drag
//! reaches it, OLE hands us the data object and we read its file list
//! (CF_HDROP) — no keystrokes, no clipboard, works from any drag source.
//! To end the drag on this computer we cover the screen with the same
//! invisible window and release the button over it, so the drop lands on us.
#![allow(non_snake_case, clippy::upper_case_acronyms)]

use crate::win32::*;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[repr(C)]
struct GUID {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

const IID_IUNKNOWN: GUID = GUID { d1: 0, d2: 0, d3: 0, d4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
const IID_IDROPTARGET: GUID = GUID { d1: 0x122, d2: 0, d3: 0, d4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };

fn same(a: &GUID, b: &GUID) -> bool {
    a.d1 == b.d1 && a.d2 == b.d2 && a.d3 == b.d3 && a.d4 == b.d4
}

#[repr(C)]
#[derive(Clone, Copy)]
struct POINTL {
    x: i32,
    y: i32,
}

#[repr(C)]
struct FORMATETC {
    cfFormat: u16,
    ptd: *mut c_void,
    dwAspect: u32,
    lindex: i32,
    tymed: u32,
}

#[repr(C)]
struct STGMEDIUM {
    tymed: u32,
    data: *mut c_void,
    pUnkForRelease: *mut c_void,
}

#[repr(C)]
struct IDataObject {
    vtbl: *const IDataObjectVtbl,
}

#[repr(C)]
struct IDataObjectVtbl {
    QueryInterface: usize,
    AddRef: usize,
    Release: usize,
    GetData: unsafe extern "system" fn(*mut IDataObject, *const FORMATETC, *mut STGMEDIUM) -> i32,
}

#[repr(C)]
struct DropTarget {
    vtbl: *const DropTargetVtbl,
    refs: AtomicU32,
}

#[repr(C)]
struct DropTargetVtbl {
    QueryInterface: unsafe extern "system" fn(*mut DropTarget, *const GUID, *mut *mut c_void) -> i32,
    AddRef: unsafe extern "system" fn(*mut DropTarget) -> u32,
    Release: unsafe extern "system" fn(*mut DropTarget) -> u32,
    DragEnter: unsafe extern "system" fn(*mut DropTarget, *mut IDataObject, u32, POINTL, *mut u32) -> i32,
    DragOver: unsafe extern "system" fn(*mut DropTarget, u32, POINTL, *mut u32) -> i32,
    DragLeave: unsafe extern "system" fn(*mut DropTarget) -> i32,
    Drop: unsafe extern "system" fn(*mut DropTarget, *mut IDataObject, u32, POINTL, *mut u32) -> i32,
}

#[repr(C)]
struct WNDCLASSEXW {
    cbSize: u32,
    style: u32,
    lpfnWndProc: unsafe extern "system" fn(HANDLE, u32, usize, isize) -> isize,
    cbClsExtra: i32,
    cbWndExtra: i32,
    hInstance: HANDLE,
    hIcon: HANDLE,
    hCursor: HANDLE,
    hbrBackground: HANDLE,
    lpszMenuName: *const u16,
    lpszClassName: *const u16,
    hIconSm: HANDLE,
}

#[link(name = "ole32")]
extern "system" {
    fn OleInitialize(reserved: *mut c_void) -> i32;
    fn RegisterDragDrop(hwnd: HANDLE, target: *mut DropTarget) -> i32;
    fn ReleaseStgMedium(m: *mut STGMEDIUM);
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(c: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HANDLE,
        menu: HANDLE,
        inst: HANDLE,
        param: *mut c_void,
    ) -> HANDLE;
    fn DefWindowProcW(hwnd: HANDLE, msg: u32, w: usize, l: isize) -> isize;
    fn SetLayeredWindowAttributes(hwnd: HANDLE, key: u32, alpha: u8, flags: u32) -> i32;
    fn SetWindowPos(hwnd: HANDLE, after: HANDLE, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    fn ShowWindowAsync(hwnd: HANDLE, cmd: i32) -> i32;
    fn IsWindowVisible(hwnd: HANDLE) -> i32;
    fn GetWindowRect(hwnd: HANDLE, r: *mut RECT) -> i32;
}

const S_OK: i32 = 0;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
const DROPEFFECT_NONE: u32 = 0;
const DROPEFFECT_COPY: u32 = 1;
const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_LAYERED: u32 = 0x0008_0000;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_SHOWWINDOW: u32 = 0x0040;
/// Never wait on the window's thread (the input thread): it may be busy.
const SWP_ASYNCWINDOWPOS: u32 = 0x4000;
const HWND_TOPMOST: isize = -1;
const LWA_ALPHA: u32 = 2;

static HWND: AtomicIsize = AtomicIsize::new(0);
/// Files found in the current drag were already reported.
static REPORTED: AtomicBool = AtomicBool::new(false);
static SINK: OnceLock<Mutex<Sender<Vec<String>>>> = OnceLock::new();

unsafe extern "system" fn qi(this: *mut DropTarget, iid: *const GUID, out: *mut *mut c_void) -> i32 {
    if same(&*iid, &IID_IUNKNOWN) || same(&*iid, &IID_IDROPTARGET) {
        *out = this as *mut c_void;
        add_ref(this);
        S_OK
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}
unsafe extern "system" fn add_ref(this: *mut DropTarget) -> u32 {
    (*this).refs.fetch_add(1, Ordering::SeqCst) + 1
}
unsafe extern "system" fn release(this: *mut DropTarget) -> u32 {
    // The target lives for the whole process; never freed.
    (*this).refs.fetch_sub(1, Ordering::SeqCst).saturating_sub(1)
}

/// Read CF_HDROP from the dragged data.
unsafe fn files_of(data: *mut IDataObject) -> Vec<String> {
    if data.is_null() {
        return Vec::new();
    }
    let fmt = FORMATETC { cfFormat: CF_HDROP as u16, ptd: std::ptr::null_mut(), dwAspect: 1, lindex: -1, tymed: 1 };
    let mut med = STGMEDIUM { tymed: 0, data: std::ptr::null_mut(), pUnkForRelease: std::ptr::null_mut() };
    if ((*(*data).vtbl).GetData)(data, &fmt, &mut med) != S_OK {
        return Vec::new();
    }
    let files = hdrop_paths(med.data);
    ReleaseStgMedium(&mut med);
    files.into_iter().map(|p| p.to_string_lossy().into_owned()).collect()
}

unsafe extern "system" fn drag_enter(_t: *mut DropTarget, data: *mut IDataObject, _k: u32, _p: POINTL, effect: *mut u32) -> i32 {
    let files = files_of(data);
    if files.is_empty() {
        log::debug!("drag reached the edge but carries no files (text, a link, ...); ignoring");
        *effect = DROPEFFECT_NONE;
        return S_OK;
    }
    *effect = DROPEFFECT_COPY;
    if !REPORTED.swap(true, Ordering::SeqCst) {
        log::info!("drag reached the edge carrying {} item(s)", files.len());
        if let Some(tx) = SINK.get() {
            let _ = crate::state::lk(tx).send(files);
        }
    }
    S_OK
}
unsafe extern "system" fn drag_over(_t: *mut DropTarget, _k: u32, _p: POINTL, effect: *mut u32) -> i32 {
    *effect = if REPORTED.load(Ordering::SeqCst) { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    S_OK
}
unsafe extern "system" fn drag_leave(_t: *mut DropTarget) -> i32 {
    S_OK
}
unsafe extern "system" fn drop(_t: *mut DropTarget, _d: *mut IDataObject, _k: u32, _p: POINTL, effect: *mut u32) -> i32 {
    // The drag ended on our invisible window: nothing happens on this computer.
    log::debug!("drag ended on the catcher window (no drop on this computer)");
    *effect = DROPEFFECT_NONE;
    hide();
    S_OK
}

static VTBL: DropTargetVtbl = DropTargetVtbl {
    QueryInterface: qi,
    AddRef: add_ref,
    Release: release,
    DragEnter: drag_enter,
    DragOver: drag_over,
    DragLeave: drag_leave,
    Drop: drop,
};

unsafe extern "system" fn wndproc(hwnd: HANDLE, msg: u32, w: usize, l: isize) -> isize {
    DefWindowProcW(hwnd, msg, w, l)
}

/// Create the drop-target window. Must run on the input thread (it has a message loop).
pub fn init(sink: Sender<Vec<String>>) {
    let _ = SINK.set(Mutex::new(sink));
    unsafe {
        let hr = OleInitialize(std::ptr::null_mut());
        if hr < 0 {
            log::warn!("file drag: OleInitialize failed ({hr:#x})");
            return;
        }
        let inst = GetModuleHandleW(std::ptr::null());
        let class = wide("edgehopDropTarget");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: wndproc,
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: inst,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            class.as_ptr(),
            wide("edgehop drop target").as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            inst,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            log::warn!("file drag: couldn't create the drop window");
            return;
        }
        // Nearly fully transparent, but still hit-testable for drag and drop.
        SetLayeredWindowAttributes(hwnd, 0, 1, LWA_ALPHA);
        let target: &'static mut DropTarget = Box::leak(Box::new(DropTarget { vtbl: &VTBL, refs: AtomicU32::new(1) }));
        let hr = RegisterDragDrop(hwnd, target);
        if hr < 0 {
            log::warn!("file drag: RegisterDragDrop failed ({hr:#x})");
            return;
        }
        HWND.store(hwnd as isize, Ordering::SeqCst);
    }
}

fn place(l: i32, t: i32, w: i32, h: i32) {
    let hwnd = HWND.load(Ordering::SeqCst) as HANDLE;
    if !hwnd.is_null() {
        unsafe {
            SetWindowPos(
                hwnd,
                HWND_TOPMOST as HANDLE,
                l,
                t,
                w.max(1),
                h.max(1),
                SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS,
            );
        }
    }
}

/// Is the catcher showing and covering (l, t, w, h)?
fn covers(l: i32, t: i32, w: i32, h: i32) -> bool {
    let hwnd = HWND.load(Ordering::SeqCst) as HANDLE;
    if hwnd.is_null() {
        return false;
    }
    let mut r = RECT::default();
    unsafe {
        IsWindowVisible(hwnd) != 0
            && GetWindowRect(hwnd, &mut r) != 0
            && r.left <= l
            && r.top <= t
            && r.right >= l + w
            && r.bottom >= t + h
    }
}

/// A drag is being pushed against this edge: put the catcher there.
pub fn show_strip(l: i32, t: i32, w: i32, h: i32) {
    REPORTED.store(false, Ordering::SeqCst);
    place(l, t, w, h);
}

pub fn hide() {
    let hwnd = HWND.load(Ordering::SeqCst) as HANDLE;
    if !hwnd.is_null() {
        unsafe {
            ShowWindowAsync(hwnd, 0);
        }
    }
}

/// End the drag on this computer: cover every screen with the catcher and
/// release the button over it, so the drop lands on us and nowhere else.
///
/// Runs on its own thread and returns immediately: the caller holds the
/// switching lock, and the input thread (which owns the catcher window and
/// sees the injected release) needs that lock to make progress.
pub fn end_drag_here() {
    std::thread::spawn(|| {
        let (l, t, w, h) = virtual_screen();
        place(l, t, w, h);
        // Only release once the catcher really covers the screen.
        let mut ready = false;
        for _ in 0..40 {
            if covers(l, t, w, h) {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if ready {
            log::debug!("catcher covers the screen; releasing the button over it");
            left_up();
        } else {
            log::warn!("file drag: catcher didn't cover the screen; cancelling the drag with Esc instead");
            tap_keys(&[(0x1B, true), (0x1B, false)]);
            left_up();
        }
        // Safety net: never leave a screen-covering window behind.
        std::thread::sleep(Duration::from_millis(500));
        hide();
    });
}
