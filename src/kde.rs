//! KDE Plasma integration over D-Bus (Linux client):
//! - which window is under the pointer (a small KWin script calls us back),
//!   so dropped files land in the Dolphin folder / desktop you let go on and
//!   drags are only captured from file managers;
//! - hiding the pointer (KWin's hide-cursor effect) and dimming the screen
//!   (KDE's ScreenBrightness service) while the mouse is on another computer.
use crate::state::lk;
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
use dbus::blocking::Connection;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

const SERVICE: &str = "org.kvmlink.Client";
const SCRIPT_NAME: &str = "kvmlink-window-at";
const TIMEOUT: Duration = Duration::from_millis(1500);

const SCRIPT: &str = r#"// kvmlink: report the window under the pointer.
(function () {
    var p = workspace.cursorPos;
    var wins = workspace.stackingOrder;
    for (var i = wins.length - 1; i >= 0; i--) {
        var w = wins[i];
        try {
            if (w.minimized || w.hidden || w.dock || w.deleted) continue;
            var g = w.frameGeometry;
            if (p.x >= g.x && p.x < g.x + g.width && p.y >= g.y && p.y < g.y + g.height) {
                callDBus("org.kvmlink.Client", "/", "org.kvmlink.Client", "WindowAt",
                         String(w.resourceClass), String(w.caption), String(w.pid), w.desktopWindow ? "1" : "0");
                return;
            }
        } catch (e) {}
    }
    callDBus("org.kvmlink.Client", "/", "org.kvmlink.Client", "WindowAt", "", "", "0", "0");
})();
"#;

#[derive(Debug, Clone)]
pub struct WinInfo {
    pub class: String,
    pub caption: String,
    pub desktop: bool,
}

#[derive(Default)]
struct Away {
    /// The mouse is on another computer.
    is_away: bool,
    /// Requested dim level while away (None = don't dim).
    dim_percent: Option<u8>,
    hid_pointer: bool,
    /// (display object name, brightness before dimming)
    dimmed: Vec<(String, i32)>,
}

struct Inner {
    conn: Mutex<Connection>,
    replies: Mutex<mpsc::Receiver<WinInfo>>,
    script: PathBuf,
    away: Mutex<Away>,
}

pub struct Kde {
    inner: Arc<Inner>,
}

fn kwriteconfig(file: &str, group: &str, key: &str, value: &str) {
    let _ = Command::new("kwriteconfig6")
        .args(["--file", file, "--group", group, "--key", key, value])
        .status();
}

impl Kde {
    /// Connect if we're in a Plasma session.
    pub fn connect() -> Option<Kde> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
        if !desktop.contains("KDE") {
            return None;
        }
        let conn = Connection::new_session().map_err(|e| log::warn!("KDE hooks off: no session bus: {e}")).ok()?;

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("kde-dbus".into())
            .spawn(move || {
                if let Err(e) = serve(tx) {
                    log::warn!("KDE hooks: can't publish {SERVICE}: {e}");
                }
            })
            .ok()?;

        let script = crate::files::received_root().with_file_name("kvmlink-window-at.js");
        if let Some(dir) = script.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&script, SCRIPT).ok()?;

        // Dolphin shows the full folder path in its title, so a drop knows where it landed.
        kwriteconfig("dolphinrc", "General", "ShowFullPathInTitlebar", "true");
        // The hide-cursor effect, used only while the mouse is elsewhere: hide fast.
        kwriteconfig("kwinrc", "Effect-hidecursor", "InactivityDuration", "1");
        kwriteconfig("kwinrc", "Effect-hidecursor", "HideOnTyping", "false");

        let inner = Arc::new(Inner {
            conn: Mutex::new(conn),
            replies: Mutex::new(rx),
            script,
            away: Mutex::new(Away::default()),
        });
        // While away, keep the screen bright whenever media is playing.
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("kde-media".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(2));
                let Some(inner) = weak.upgrade() else { break };
                inner.follow_media();
            })
            .ok()?;
        log::info!("KDE hooks ready (KWin scripting, cursor effect, screen brightness, media-aware dimming)");
        Some(Kde { inner })
    }

    pub fn window_under_pointer(&self) -> Option<WinInfo> {
        self.inner.window_under_pointer()
    }

    pub fn away(&self, hide_pointer: bool, dim_percent: Option<u8>) {
        self.inner.away(hide_pointer, dim_percent)
    }

    pub fn back(&self) {
        self.inner.back()
    }
}

impl Inner {
    /// Ask KWin which window is under the pointer right now.
    fn window_under_pointer(&self) -> Option<WinInfo> {
        let conn = lk(&self.conn);
        let rx = lk(&self.replies);
        while rx.try_recv().is_ok() {}
        let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", TIMEOUT);
        let _: Result<(bool,), _> = scripting.method_call("org.kde.kwin.Scripting", "unloadScript", (SCRIPT_NAME,));
        let path = self.script.to_string_lossy().into_owned();
        let (id,): (i32,) = scripting
            .method_call("org.kde.kwin.Scripting", "loadScript", (path, SCRIPT_NAME))
            .map_err(|e| log::warn!("KWin loadScript failed: {e}"))
            .ok()?;
        let info = if id >= 0 {
            let script = conn.with_proxy("org.kde.KWin", format!("/Scripting/Script{id}"), TIMEOUT);
            let run: Result<(), _> = script.method_call("org.kde.kwin.Script", "run", ());
            if let Err(e) = run {
                log::warn!("KWin script run failed: {e}");
            }
            rx.recv_timeout(Duration::from_millis(1000)).ok()
        } else {
            None
        };
        let _: Result<(bool,), _> = scripting.method_call("org.kde.kwin.Scripting", "unloadScript", (SCRIPT_NAME,));
        info.filter(|w| !w.class.is_empty() || w.desktop)
    }

    /// The mouse went to another computer: hide the pointer, dim the screen
    /// (unless something is playing).
    fn away(&self, hide_pointer: bool, dim_percent: Option<u8>) {
        let conn = lk(&self.conn);
        let mut st = lk(&self.away);
        st.is_away = true;
        st.dim_percent = dim_percent;
        if hide_pointer && !st.hid_pointer {
            let fx = conn.with_proxy("org.kde.KWin", "/Effects", TIMEOUT);
            let loaded = fx
                .method_call::<(bool,), _, _, _>("org.kde.kwin.Effects", "isEffectLoaded", ("hidecursor",))
                .map(|(b,)| b)
                .unwrap_or(true);
            if !loaded {
                let _: Result<(bool,), _> = fx.method_call("org.kde.kwin.Effects", "loadEffect", ("hidecursor",));
                st.hid_pointer = true;
            }
        }
        if dim_percent.is_some() && st.dimmed.is_empty() {
            if media_playing(&conn) {
                log::info!("media is playing: not dimming");
            } else {
                dim(&conn, &mut st);
            }
        }
    }

    /// Called every couple of seconds: undim while media plays, dim again after.
    fn follow_media(&self) {
        let conn = lk(&self.conn);
        let mut st = lk(&self.away);
        if !st.is_away || st.dim_percent.is_none() {
            return;
        }
        let playing = media_playing(&conn);
        if playing && !st.dimmed.is_empty() {
            log::info!("media started playing: restoring brightness");
            undim(&conn, &mut st);
        } else if !playing && st.dimmed.is_empty() {
            dim(&conn, &mut st);
        }
    }

    /// The mouse is back: show the pointer, restore brightness.
    fn back(&self) {
        let conn = lk(&self.conn);
        let mut st = lk(&self.away);
        st.is_away = false;
        if st.hid_pointer {
            let fx = conn.with_proxy("org.kde.KWin", "/Effects", TIMEOUT);
            let _: Result<(), _> = fx.method_call("org.kde.kwin.Effects", "unloadEffect", ("hidecursor",));
            st.hid_pointer = false;
        }
        undim(&conn, &mut st);
    }
}

impl Drop for Kde {
    fn drop(&mut self) {
        self.inner.back();
    }
}

fn dim(conn: &Connection, st: &mut Away) {
    let Some(pct) = st.dim_percent else { return };
    for name in displays(conn) {
        let d = conn.with_proxy("org.kde.ScreenBrightness", format!("/org/kde/ScreenBrightness/{name}"), TIMEOUT);
        let (Ok(cur), Ok(max)) = (
            d.get::<i32>("org.kde.ScreenBrightness.Display", "Brightness"),
            d.get::<i32>("org.kde.ScreenBrightness.Display", "MaxBrightness"),
        ) else {
            continue;
        };
        let target = (cur as i64 * pct as i64 / 100).max(max as i64 / 50) as i32;
        if target < cur {
            let r: Result<(), _> = d.method_call("org.kde.ScreenBrightness.Display", "SetBrightness", (target, 1u32));
            if r.is_ok() {
                st.dimmed.push((name, cur));
            }
        }
    }
}

fn undim(conn: &Connection, st: &mut Away) {
    for (name, level) in st.dimmed.drain(..) {
        let d = conn.with_proxy("org.kde.ScreenBrightness", format!("/org/kde/ScreenBrightness/{name}"), TIMEOUT);
        let _: Result<(), _> = d.method_call("org.kde.ScreenBrightness.Display", "SetBrightness", (level, 1u32));
    }
}

/// Is any media player (MPRIS: browsers, Spotify, VLC, ...) playing right now?
fn media_playing(conn: &Connection) -> bool {
    let bus = conn.with_proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", TIMEOUT);
    let Ok((names,)) = bus.method_call::<(Vec<String>,), _, _, _>("org.freedesktop.DBus", "ListNames", ()) else {
        return false;
    };
    names.iter().filter(|n| n.starts_with("org.mpris.MediaPlayer2.")).any(|n| {
        conn.with_proxy(n.as_str(), "/org/mpris/MediaPlayer2", Duration::from_millis(500))
            .get::<String>("org.mpris.MediaPlayer2.Player", "PlaybackStatus")
            .map_or(false, |s| s == "Playing")
    })
}

fn displays(conn: &Connection) -> Vec<String> {
    conn.with_proxy("org.kde.ScreenBrightness", "/org/kde/ScreenBrightness", TIMEOUT)
        .get::<Vec<String>>("org.kde.ScreenBrightness", "DisplaysDBusNames")
        .unwrap_or_default()
}

/// Publish org.kvmlink.Client so the KWin script can report back.
fn serve(tx: mpsc::Sender<WinInfo>) -> Result<(), dbus::Error> {
    let c = Connection::new_session()?;
    c.request_name(SERVICE, false, true, false)?;
    let mut cr = dbus_crossroads::Crossroads::new();
    let iface = cr.register(SERVICE, |b: &mut dbus_crossroads::IfaceBuilder<mpsc::Sender<WinInfo>>| {
        b.method(
            "WindowAt",
            ("class", "caption", "pid", "desktop"),
            (),
            |_, tx, (class, caption, _pid, desktop): (String, String, String, String)| {
                let _ = tx.send(WinInfo { class, caption, desktop: desktop == "1" });
                Ok(())
            },
        );
    });
    cr.insert("/", &[iface], tx);
    cr.serve(&c)
}

/// Is this window a place files can be dragged from / dropped onto?
pub fn is_file_manager(w: &WinInfo) -> bool {
    let c = w.class.to_lowercase();
    w.desktop || ["dolphin", "nautilus", "nemo", "thunar", "pcmanfm", "caja", "krusader"].iter().any(|f| c.contains(f))
}

/// The folder a drop onto this window should land in.
pub fn drop_folder(w: &WinInfo) -> Option<PathBuf> {
    let dirs = directories::UserDirs::new()?;
    if w.desktop {
        return dirs.desktop_dir().map(|d| d.to_path_buf());
    }
    if !w.class.to_lowercase().contains("dolphin") {
        return None;
    }
    // "/home/me/Pictures — Dolphin" (full path in title) or "Pictures — Dolphin".
    let title = w.caption.rsplit_once(" — ").or_else(|| w.caption.rsplit_once(" - ")).map_or(&*w.caption, |(t, _)| t);
    let title = title.trim().trim_end_matches(" (Modified)");
    let p = PathBuf::from(title);
    if p.is_absolute() && p.is_dir() {
        return Some(p);
    }
    let home = dirs.home_dir().to_path_buf();
    if title == "Home" {
        return Some(home);
    }
    let guess = home.join(title);
    guess.is_dir().then_some(guess)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dolphin_titles_resolve_to_folders() {
        let tmp = std::env::temp_dir();
        let w = WinInfo { class: "org.kde.dolphin".into(), caption: format!("{} — Dolphin", tmp.display()), desktop: false };
        assert_eq!(drop_folder(&w), Some(tmp));
        let other = WinInfo { class: "firefox".into(), caption: "x".into(), desktop: false };
        assert!(drop_folder(&other).is_none());
        assert!(!is_file_manager(&other));
        assert!(is_file_manager(&WinInfo { class: "plasmashell".into(), caption: String::new(), desktop: true }));
    }
}
