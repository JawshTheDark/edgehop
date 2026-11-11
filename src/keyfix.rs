//! Linux clients: undo this computer's own modifier-key rearrangements for
//! keys typed on the sharing computer.
//!
//! People who come from a Mac often set Linux up so the key beside the space
//! bar (Super, Command on a Mac keyboard) works as Ctrl. Keyboard layout
//! options (KDE's and GNOME's keyboard settings) and remappers such as keyd
//! apply to every keyboard, Edgehop's virtual one included, so a Ctrl from
//! the sharing computer would arrive as Super and Ctrl+C wouldn't copy.
//! Edgehop sends whichever key this computer turns into the one pressed.
use std::path::Path;
use std::sync::Mutex;

const LCTRL: u16 = 29;
const RCTRL: u16 = 97;
const LALT: u16 = 56;
const RALT: u16 = 100;
const LMETA: u16 = 125;
const RMETA: u16 = 126;

/// Ctrl and Super swapped, both sides (the manual switch).
pub const SWAP: &[(u16, u16)] = &[(LCTRL, LMETA), (LMETA, LCTRL), (RCTRL, RMETA), (RMETA, RCTRL)];

/// What this computer does to its modifier keys.
#[derive(Clone, Debug, Default)]
pub struct Detected {
    /// (key pressed on the sharing computer, key to send so that's what arrives).
    pub pairs: Vec<(u16, u16)>,
    /// Where that came from, for the settings page and the log.
    pub why: String,
    /// Everything found, for the log.
    pub report: String,
}

static FOUND: Mutex<Option<Detected>> = Mutex::new(None);

/// What was found last (looks the first time).
pub fn detected() -> Detected {
    let mut f = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    f.get_or_insert_with(detect).clone()
}

/// Look again (asks gsettings, reads a few files): keyboard settings may
/// have changed since Edgehop started.
pub fn refresh() -> Detected {
    let d = detect();
    *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = Some(d.clone());
    d
}

/// The keys to send instead, given the user's switch (None = as detected).
pub fn effective(setting: Option<bool>) -> Vec<(u16, u16)> {
    match setting {
        Some(true) => SWAP.to_vec(),
        Some(false) => Vec::new(),
        None => detected().pairs,
    }
}

/// Keys to send instead, for one XKB option.
fn option_fix(opt: &str) -> &'static [(u16, u16)] {
    match opt {
        "ctrl:swap_lwin_lctl" => &[(LCTRL, LMETA), (LMETA, LCTRL)],
        "ctrl:swap_rwin_rctl" => &[(RCTRL, RMETA), (RMETA, RCTRL)],
        "ctrl:swap_lalt_lctl" => &[(LCTRL, LALT), (LALT, LCTRL)],
        "ctrl:swap_ralt_rctl" => &[(RCTRL, RALT), (RALT, RCTRL)],
        "altwin:swap_lalt_lwin" => &[(LALT, LMETA), (LMETA, LALT)],
        "altwin:swap_alt_win" => &[(LALT, LMETA), (LMETA, LALT), (RALT, RMETA), (RMETA, RALT)],
        // Left Alt as Ctrl, Left Ctrl as Win, Left Win as Left Alt.
        "ctrl:swap_lalt_lctl_lwin" => &[(LCTRL, LALT), (LMETA, LCTRL), (LALT, LMETA)],
        _ => &[],
    }
}

fn add(pairs: &mut Vec<(u16, u16)>, more: &[(u16, u16)]) {
    for &(want, send) in more {
        if !pairs.iter().any(|&(w, _)| w == want) {
            pairs.push((want, send));
        }
    }
}

fn detect() -> Detected {
    let mut d = Detected::default();
    let mut notes = Vec::new();
    let (opts, source) = xkb_options();
    notes.push(format!("layout options {opts:?} ({source})"));
    for o in &opts {
        let fix = option_fix(o);
        if !fix.is_empty() {
            add(&mut d.pairs, fix);
            d.why = format!("{source} use {o}");
        }
    }
    match keyd() {
        Some((fix, what)) => {
            notes.push(format!("keyd: {what}"));
            if !fix.is_empty() {
                add(&mut d.pairs, &fix);
                if d.why.is_empty() {
                    d.why = format!("keyd {what}");
                }
            }
        }
        None => notes.push("keyd: not running".into()),
    }
    if let Some(home) = std::env::var_os("HOME") {
        if Path::new(&home).join(".config/toshy").exists() {
            notes.push(
                "Toshy is installed (it may treat Edgehop's keyboard as a PC keyboard; if keys come out wrong, set \
                 its type to Apple in Toshy's settings)"
                    .into(),
            );
        }
    }
    if let Ok(v) = std::fs::read_to_string("/sys/module/hid_apple/parameters/swap_ctrl_cmd") {
        let on = matches!(v.trim(), "1" | "Y" | "y");
        notes.push(format!("Apple keyboard driver swap_ctrl_cmd={} (built-in keyboard only)", if on { "on" } else { "off" }));
    }
    d.report = notes.join("; ");
    d
}

/// XKB options in force, and where they're set.
fn xkb_options() -> (Vec<String>, &'static str) {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    let system = system_options();
    // Started without the desktop's environment (e.g. by systemd): KDE's file still counts.
    if desktop.contains("KDE") || desktop.is_empty() {
        if let Some((opts, reset)) = kde_options() {
            if reset {
                return (opts, "KDE's keyboard settings");
            }
            let mut all = system;
            all.extend(opts);
            return (all, "KDE's keyboard settings");
        }
    } else if ["GNOME", "UNITY", "BUDGIE", "PANTHEON"].iter().any(|d| desktop.contains(d)) {
        // Desktops that keep their layout options in GNOME's settings (others,
        // like Sway or Hyprland, have their own; GNOME's would be stale there).
        if let Some(opts) = gnome_options() {
            return (opts, "GNOME's keyboard settings");
        }
    }
    (system, "the system keyboard settings")
}

/// ~/.config/kxkbrc: (options, whether they replace the system's).
fn kde_options() -> Option<(Vec<String>, bool)> {
    let home = std::env::var_os("HOME")?;
    let text = std::fs::read_to_string(Path::new(&home).join(".config/kxkbrc")).ok()?;
    parse_kxkbrc(&text)
}

fn parse_kxkbrc(text: &str) -> Option<(Vec<String>, bool)> {
    let mut in_layout = false;
    let (mut opts, mut reset) = (None, false);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_layout = line == "[Layout]";
            continue;
        }
        if !in_layout {
            continue;
        }
        if let Some(v) = line.strip_prefix("Options=") {
            opts = Some(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
        } else if let Some(v) = line.strip_prefix("ResetOldOptions=") {
            reset = v.trim() == "true";
        }
    }
    opts.map(|o| (o, reset))
}

fn gnome_options() -> Option<Vec<String>> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.input-sources", "xkb-options"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(quoted(&String::from_utf8_lossy(&out.stdout)))
}

/// 'a', 'b' from gsettings' ['a', 'b'].
fn quoted(s: &str) -> Vec<String> {
    s.split('\'').skip(1).step_by(2).map(str::to_string).filter(|o| !o.is_empty()).collect()
}

fn system_options() -> Vec<String> {
    if let Ok(t) = std::fs::read_to_string("/etc/X11/xorg.conf.d/00-keyboard.conf") {
        for line in t.lines().filter(|l| !l.trim_start().starts_with('#')) {
            let parts: Vec<&str> = line.split('"').collect();
            if parts.len() >= 4 && parts[1] == "XkbOptions" {
                return parts[3].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            }
        }
    }
    if let Ok(t) = std::fs::read_to_string("/etc/default/keyboard") {
        for line in t.lines() {
            if let Some(v) = line.trim().strip_prefix("XKBOPTIONS=") {
                return v.trim_matches('"').split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            }
        }
    }
    Vec::new()
}

fn process_running(name: &str) -> bool {
    let Ok(procs) = std::fs::read_dir("/proc") else { return false };
    procs.flatten().any(|p| std::fs::read_to_string(p.path().join("comm")).is_ok_and(|c| c.trim() == name))
}

/// keyd remaps every keyboard it matches, Edgehop's too: what it does to Ctrl
/// and Super. None if it isn't running.
fn keyd() -> Option<(Vec<(u16, u16)>, String)> {
    if !process_running("keyd") {
        return None;
    }
    // Each file is a configuration of its own, for the keyboards in its own
    // [ids]: only one that says "all keyboards" applies to Edgehop's.
    let mut all = (Vec::new(), "no configuration for all keyboards".to_string());
    for e in std::fs::read_dir("/etc/keyd").ok()?.flatten() {
        if e.path().extension().is_some_and(|x| x == "conf") {
            let (fix, what) = parse_keyd(&std::fs::read_to_string(e.path()).unwrap_or_default());
            if what.contains("(all keyboards)") {
                all = (fix, what);
            }
        }
    }
    Some(all)
}

/// The modifier a keyd action ends up as: 'C', 'M', 'A' or 'S'.
fn keyd_modifier(action: &str, layers: &[(String, String)]) -> Option<char> {
    let a = action.trim();
    let inner = a
        .strip_prefix("layer(")
        .or_else(|| a.strip_prefix("oneshot("))
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(a)
        .trim();
    match inner {
        "control" | "leftcontrol" | "rightcontrol" | "C" => Some('C'),
        "meta" | "leftmeta" | "rightmeta" | "M" => Some('M'),
        "alt" | "leftalt" | "A" => Some('A'),
        "shift" | "leftshift" | "S" => Some('S'),
        name => layers.iter().find(|(n, _)| n == name).and_then(|(_, m)| m.chars().next()),
    }
}

fn parse_keyd(text: &str) -> (Vec<(u16, u16)>, String) {
    // Layers that act as a modifier: [name:C].
    let mut layers = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(h) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if let Some((name, m)) = h.split_once(':') {
                layers.push((name.trim().to_string(), m.trim().to_string()));
            }
        }
    }
    let (mut section, mut all_keyboards) = (String::new(), false);
    let (mut ctrl_as, mut meta_as) = (None, None);
    for line in text.lines().map(str::trim) {
        if let Some(h) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = h.trim().to_string();
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if section == "ids" && line == "*" {
            all_keyboards = true;
        }
        if section != "main" {
            continue;
        }
        let Some((key, action)) = line.split_once('=') else { continue };
        match key.trim() {
            "leftcontrol" | "control" => ctrl_as = keyd_modifier(action, &layers),
            "leftmeta" | "meta" => meta_as = keyd_modifier(action, &layers),
            _ => {}
        }
    }
    let what = format!(
        "Ctrl acts as {}, Super acts as {}{}",
        ctrl_as.map_or("Ctrl".into(), |c| c.to_string()),
        meta_as.map_or("Super".into(), |c| c.to_string()),
        if all_keyboards { " (all keyboards)" } else { " (listed keyboards only)" }
    );
    let mut pairs = Vec::new();
    if all_keyboards {
        // Only needed where the key the sharing computer pressed no longer
        // does its own job here.
        if ctrl_as.is_some_and(|m| m != 'C') && meta_as == Some('C') {
            pairs.push((LCTRL, LMETA));
        }
        if meta_as.is_some_and(|m| m != 'M') && ctrl_as == Some('M') {
            pairs.push((LMETA, LCTRL));
        }
    }
    (pairs, what)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kde_swap_is_undone() {
        let (opts, reset) = parse_kxkbrc("[Layout]\nLayoutList=us\nOptions=ctrl:swap_lwin_lctl,compose:ralt\nResetOldOptions=true\n").unwrap();
        assert!(reset);
        assert_eq!(opts, ["ctrl:swap_lwin_lctl", "compose:ralt"]);
        assert_eq!(option_fix(&opts[0]), &[(LCTRL, LMETA), (LMETA, LCTRL)]);
        assert!(parse_kxkbrc("[Layout]\nLayoutList=us\n").is_none());
    }

    #[test]
    fn gnome_options_parse() {
        assert_eq!(quoted("['ctrl:swap_lwin_lctl', 'compose:ralt']\n"), ["ctrl:swap_lwin_lctl", "compose:ralt"]);
        assert!(quoted("@as []\n").is_empty());
    }

    #[test]
    fn keyd_mac_style_swap() {
        let conf = "[ids]\n*\n\n[main]\nleftmeta = layer(meta_mac)\nleftcontrol = layer(meta)\n\n[meta_mac:C]\nc = C-insert\n";
        let (pairs, what) = parse_keyd(conf);
        assert_eq!(pairs, [(LCTRL, LMETA), (LMETA, LCTRL)], "{what}");
        // Super works as Ctrl but Ctrl is still Ctrl: nothing to undo.
        let (pairs, _) = parse_keyd("[ids]\n*\n[main]\nleftmeta = layer(control)\n");
        assert!(pairs.is_empty());
    }
}
