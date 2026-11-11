//! macOS: files on the clipboard (Finder's Copy and Paste), the files a drag
//! carries, and the folder under the pointer when files are dropped here.
//!
//! A drag in progress keeps what it carries on the drag pasteboard, the same
//! way the clipboard holds a copy (Synergy finds dragged files the same way).
//! Its change count goes up when a new drag starts, which tells a drag that
//! started after the button went down from what an earlier drag left there.
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{class, msg_send};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};

#[link(name = "AppKit", kind = "framework")]
extern "C" {
    static NSPasteboardNameDrag: *mut AnyObject;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *mut AnyObject;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(p: *const c_void);
}

type Id = *mut AnyObject;

fn nsstring(s: &str) -> Id {
    let c = CString::new(s).unwrap_or_default();
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

/// An NSString's text ("" for nil or anything else).
fn text(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    unsafe {
        let p: *const c_char = msg_send![s, UTF8String];
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

fn general() -> Id {
    unsafe { msg_send![class!(NSPasteboard), generalPasteboard] }
}

fn drag() -> Id {
    unsafe { msg_send![class!(NSPasteboard), pasteboardWithName: NSPasteboardNameDrag] }
}

/// File paths on a pasteboard (file URLs only, resolved).
fn files_on(pb: Id) -> Vec<PathBuf> {
    if pb.is_null() {
        return Vec::new();
    }
    autoreleasepool(|_| unsafe {
        let url_class = class!(NSURL) as *const AnyClass as Id;
        let classes: Id = msg_send![class!(NSArray), arrayWithObject: url_class];
        let items: Id = msg_send![pb, readObjectsForClasses: classes, options: std::ptr::null_mut::<AnyObject>()];
        if items.is_null() {
            return Vec::new();
        }
        let n: usize = msg_send![items, count];
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let url: Id = msg_send![items, objectAtIndex: i];
            let is_file: Bool = msg_send![url, isFileURL];
            if !is_file.as_bool() {
                continue;
            }
            // Finder may hand over file reference URLs (/.file/id=...): resolve them.
            let resolved: Id = msg_send![url, filePathURL];
            let target = if resolved.is_null() { url } else { resolved };
            let path = text(msg_send![target, path]);
            if !path.is_empty() {
                out.push(PathBuf::from(path));
            }
        }
        out
    })
}

fn write_files(pb: Id, paths: &[PathBuf]) -> bool {
    if pb.is_null() || paths.is_empty() {
        return false;
    }
    autoreleasepool(|_| unsafe {
        let urls: Id = msg_send![class!(NSMutableArray), array];
        for p in paths {
            let url: Id = msg_send![class!(NSURL), fileURLWithPath: nsstring(&p.to_string_lossy())];
            if !url.is_null() {
                let _: () = msg_send![urls, addObject: url];
            }
        }
        let _: isize = msg_send![pb, clearContents];
        let ok: Bool = msg_send![pb, writeObjects: urls];
        ok.as_bool()
    })
}

/// Files on the clipboard (None: no files).
pub fn clipboard_files() -> Option<Vec<PathBuf>> {
    let f = files_on(general());
    (!f.is_empty()).then_some(f)
}

/// Put files on the clipboard, for Paste in Finder.
pub fn set_clipboard_files(paths: &[PathBuf]) -> bool {
    write_files(general(), paths)
}

/// Goes up each time a drag starts.
pub fn drag_change_count() -> isize {
    // In a pool of its own: this runs on long-lived threads, once per click.
    autoreleasepool(|_| {
        let pb = drag();
        if pb.is_null() {
            return -1;
        }
        unsafe { msg_send![pb, changeCount] }
    })
}

/// What the current (or last) drag carries.
pub fn drag_files() -> Vec<PathBuf> {
    autoreleasepool(|_| files_on(drag()))
}

/// Self-test: put files on the drag pasteboard as a Finder drag would.
pub fn put_on_drag_pasteboard(paths: &[PathBuf]) -> bool {
    write_files(drag(), paths)
}

/// What's under the point (global top-left coordinates), front to back.
enum Under {
    /// No window: the desktop.
    Desktop,
    Finder,
    Other(String),
}

fn under(x: i32, y: i32) -> Under {
    const ON_SCREEN_ONLY: u32 = 1;
    const EXCLUDE_DESKTOP: u32 = 16;
    autoreleasepool(|_| unsafe {
        let list = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0);
        if list.is_null() {
            return Under::Other("window list unavailable".into());
        }
        let n: usize = msg_send![list, count];
        let num = |d: Id, k: &str| -> f64 {
            let v: Id = msg_send![d, objectForKey: nsstring(k)];
            if v.is_null() {
                0.0
            } else {
                msg_send![v, doubleValue]
            }
        };
        let mut found = Under::Desktop;
        for i in 0..n {
            let w: Id = msg_send![list, objectAtIndex: i];
            let b: Id = msg_send![w, objectForKey: nsstring("kCGWindowBounds")];
            if b.is_null() {
                continue;
            }
            let (bx, by, bw, bh) = (num(b, "X"), num(b, "Y"), num(b, "Width"), num(b, "Height"));
            let (px, py) = (x as f64, y as f64);
            if px < bx || py < by || px >= bx + bw || py >= by + bh {
                continue;
            }
            // Only ordinary app windows: the menu bar, the Dock and invisible
            // system overlays sit on other layers.
            if num(w, "kCGWindowLayer") as i64 != 0 || num(w, "kCGWindowAlpha") <= 0.0 {
                continue;
            }
            let owner = text(msg_send![w, objectForKey: nsstring("kCGWindowOwnerName")]);
            found = if owner == "Finder" { Under::Finder } else { Under::Other(owner) };
            break;
        }
        CFRelease(list as *const c_void);
        found
    })
}

/// The folder files dropped at (x, y) belong in: the Desktop, or the Finder
/// window there. None for anything else (they go on the clipboard instead).
pub fn folder_at(x: i32, y: i32) -> Option<PathBuf> {
    match under(x, y) {
        Under::Desktop => {
            let d = directories::UserDirs::new().and_then(|u| u.desktop_dir().map(Path::to_path_buf));
            log::info!("dropped on the desktop: {d:?}");
            d
        }
        Under::Finder => {
            let f = finder_folder_at(x, y);
            log::info!("dropped on a Finder window: {f:?}");
            f
        }
        Under::Other(what) => {
            log::info!("dropped on {what}: not a folder; the files go on the clipboard");
            None
        }
    }
}

/// Ask Finder which of its windows (front to back) contains the point.
/// macOS asks the user once whether Edgehop may do this.
fn finder_folder_at(x: i32, y: i32) -> Option<PathBuf> {
    const SCRIPT: &str = r#"tell application "Finder"
	set out to ""
	repeat with w in (every Finder window)
		try
			set b to bounds of w
			set p to POSIX path of (target of w as alias)
			set out to out & (item 1 of b) & tab & (item 2 of b) & tab & (item 3 of b) & tab & (item 4 of b) & tab & p & linefeed
		end try
	end repeat
	return out
end tell"#;
    let out = std::process::Command::new("osascript").args(["-e", SCRIPT]).output().ok()?;
    if !out.status.success() {
        log::warn!("couldn't ask Finder for its windows: {}", String::from_utf8_lossy(&out.stderr).trim());
        return None;
    }
    finder_pick(&String::from_utf8_lossy(&out.stdout), x, y)
}

/// From "left\ttop\tright\tbottom\tpath" lines (front to back): the first window containing (x, y).
fn finder_pick(listing: &str, x: i32, y: i32) -> Option<PathBuf> {
    listing.lines().find_map(|line| {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 {
            return None;
        }
        let n: Vec<i32> = f[..4].iter().filter_map(|v| v.trim().parse().ok()).collect();
        (n.len() == 4 && x >= n[0] && x < n[2] && y >= n[1] && y < n[3]).then(|| PathBuf::from(f[4..].join("\t")))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn finder_window_under_the_point() {
        let listing = "0\t25\t800\t600\t/Users/a/Documents/\n100\t100\t1500\t900\t/Users/a/Downloads/\n";
        assert_eq!(super::finder_pick(listing, 400, 300), Some("/Users/a/Documents/".into()), "front window wins");
        assert_eq!(super::finder_pick(listing, 1000, 800), Some("/Users/a/Downloads/".into()));
        assert_eq!(super::finder_pick(listing, 1600, 800), None);
    }
}
