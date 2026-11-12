//! Screen geometry. Everything lives in one coordinate space: the server's own
//! desktop pixels (the same coordinates Windows uses for its monitors). Each
//! client screen is a rectangle in that space, attached to the edge of a
//! monitor or of another client screen, so crossing an edge is plain geometry
//! and position is continuous.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub l: f64,
    pub t: f64,
    /// exclusive
    pub r: f64,
    /// exclusive
    pub b: f64,
}

impl Rect {
    pub fn new(l: f64, t: f64, r: f64, b: f64) -> Rect {
        Rect { l, t, r, b }
    }
    pub fn xywh(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { l: x, t: y, r: x + w, b: y + h }
    }
    pub fn w(&self) -> f64 {
        self.r - self.l
    }
    pub fn h(&self) -> f64 {
        self.b - self.t
    }
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.l < o.r - 0.5 && o.l < self.r - 0.5 && self.t < o.b - 0.5 && o.t < self.b - 0.5
    }
    pub fn union(&self, o: &Rect) -> Rect {
        Rect::new(self.l.min(o.l), self.t.min(o.t), self.r.max(o.r), self.b.max(o.b))
    }
    pub fn center(&self) -> (f64, f64) {
        ((self.l + self.r) / 2.0, (self.t + self.b) / 2.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub rect: Rect,
    /// Monitor model name as Windows reports it (may be empty).
    pub label: String,
    pub primary: bool,
}

/// Where a client screen sits, in server desktop pixels.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ScreenPos {
    pub name: String,
    pub x: f64,
    pub y: f64,
    /// Server pixels per client pixel.
    pub scale: f64,
    /// Last known logical size of the client screen.
    pub w: i32,
    pub h: i32,
    /// Stretched to cover the whole monitor edge (corner-to-corner continuity).
    #[serde(default)]
    pub fit: bool,
}

impl ScreenPos {
    pub fn rect(&self) -> Rect {
        Rect::xywh(self.x, self.y, self.w.max(1) as f64 * self.scale, self.h.max(1) as f64 * self.scale)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

pub const SIDES: [Side; 4] = [Side::Right, Side::Bottom, Side::Left, Side::Top];

/// Place a `w`×`h` screen against `side` of monitor `m`.
/// `fit` stretches it to the monitor's edge length; otherwise it is 1:1 and
/// `along` (its left or top coordinate) is kept, clamped so it still touches `m`.
pub fn attach(m: &Rect, side: Side, w: i32, h: i32, fit: bool, along: Option<f64>) -> (f64, f64, f64) {
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    match side {
        Side::Top | Side::Bottom => {
            let scale = if fit { m.w() / w } else { 1.0 };
            let (sw, sh) = (w * scale, h * scale);
            let x = if fit {
                m.l
            } else {
                along.unwrap_or(m.l + (m.w() - sw) / 2.0).clamp(m.l - sw + 50.0, m.r - 50.0).round()
            };
            let y = if side == Side::Bottom { m.b } else { m.t - sh };
            (x, y, scale)
        }
        Side::Left | Side::Right => {
            let scale = if fit { m.h() / h } else { 1.0 };
            let (sw, sh) = (w * scale, h * scale);
            let y = if fit {
                m.t
            } else {
                along.unwrap_or(m.t + (m.h() - sh) / 2.0).clamp(m.t - sh + 50.0, m.b - 50.0).round()
            };
            let x = if side == Side::Right { m.r } else { m.l - sw };
            (x, y, scale)
        }
    }
}

fn candidate(name: &str, m: &Rect, side: Side, w: i32, h: i32, fit: bool, along: Option<f64>) -> ScreenPos {
    let (x, y, scale) = attach(m, side, w, h, fit, along);
    ScreenPos { name: name.to_string(), x, y, scale, w, h, fit }
}

fn collides(r: &Rect, mons: &[Monitor], others: &[Rect]) -> bool {
    mons.iter().any(|m| m.rect.overlaps(r)) || others.iter().any(|o| o.overlaps(r))
}

/// Sharing part of an edge (a corner alone doesn't count).
fn touches(a: &Rect, b: &Rect) -> bool {
    let beside = a.t < b.b - 0.5 && b.t < a.b - 0.5; // shares some height
    let stacked = a.l < b.r - 0.5 && b.l < a.r - 0.5; // shares some width
    (beside && ((a.r - b.l).abs() < 1.0 || (a.l - b.r).abs() < 1.0))
        || (stacked && ((a.b - b.t).abs() < 1.0 || (a.t - b.b).abs() < 1.0))
}

/// Which of `rects` are joined to a monitor, directly or through each other:
/// the pointer can get to those from this computer.
fn joined(mons: &[Monitor], rects: &[Rect]) -> Vec<bool> {
    let mut ok: Vec<bool> = rects.iter().map(|r| mons.iter().any(|m| touches(&m.rect, r))).collect();
    loop {
        let more: Vec<usize> = (0..rects.len())
            .filter(|&i| !ok[i] && (0..rects.len()).any(|j| ok[j] && touches(&rects[j], &rects[i])))
            .collect();
        if more.is_empty() {
            return ok;
        }
        for i in more {
            ok[i] = true;
        }
    }
}

/// What a screen can be attached to: the monitors (main one first), then the
/// other screens that are themselves joined to a monitor.
fn anchors(mons: &[Monitor], others: &[Rect]) -> Vec<Rect> {
    let mut out: Vec<Rect> = mons.iter().filter(|m| m.primary).map(|m| m.rect).collect();
    out.extend(mons.iter().filter(|m| !m.primary).map(|m| m.rect));
    let ok = joined(mons, others);
    out.extend(others.iter().zip(ok).filter(|(_, ok)| *ok).map(|(r, _)| *r));
    out
}

/// First free edge: the main monitor's, then the other monitors', then those
/// of the screens already placed.
pub fn auto_place(mons: &[Monitor], others: &[Rect], name: &str, w: i32, h: i32) -> ScreenPos {
    let (w, h) = if w > 0 && h > 0 { (w, h) } else { (1920, 1080) };
    for m in &anchors(mons, others) {
        for side in SIDES {
            let c = candidate(name, m, side, w, h, true, None);
            if !collides(&c.rect(), mons, others) {
                return c;
            }
        }
    }
    // Everything is taken: put it to the right of everything.
    let right = mons.iter().map(|m| m.rect.r).chain(others.iter().map(|o| o.r)).fold(0.0, f64::max);
    ScreenPos { name: name.into(), x: right, y: 0.0, scale: 1.0, w, h, fit: false }
}

/// After a drag: attach the screen to the nearest edge where it fits, of a
/// monitor or of another screen (one the pointer can already get to).
pub fn snap(mons: &[Monitor], others: &[Rect], pos: &ScreenPos) -> Option<ScreenPos> {
    let cur = pos.rect();
    let (cx, cy) = cur.center();
    let mut best: Option<(f64, ScreenPos)> = None;
    for m in &anchors(mons, others) {
        for side in SIDES {
            let along = match side {
                Side::Top | Side::Bottom => cur.l,
                Side::Left | Side::Right => cur.t,
            };
            let c = candidate(&pos.name, m, side, pos.w, pos.h, pos.fit, Some(along));
            let r = c.rect();
            if collides(&r, mons, others) {
                continue;
            }
            let (rx, ry) = r.center();
            let d = (rx - cx).powi(2) + (ry - cy).powi(2);
            if best.as_ref().map_or(true, |(bd, _)| d < *bd) {
                best = Some((d, c));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Keep a placement consistent when the client's resolution changes: same
/// fit, and still touching the edge it was attached to, of a monitor or of
/// another screen (a screen to the left of or above what it's attached to
/// would otherwise drift away from it as it shrinks, and past a small gap the
/// pointer can no longer cross).
pub fn resize(pos: &ScreenPos, mons: &[Monitor], others: &[Rect], w: i32, h: i32) -> ScreenPos {
    let mut p = pos.clone();
    let old = pos.rect();
    // Not the screens that are only attached to this one: they follow it.
    let anchors = anchors(mons, others);
    let beside = |m: &Rect| old.t < m.b && m.t < old.b; // shares some height
    let stacked = |m: &Rect| old.l < m.r && m.l < old.r; // shares some width
    if p.fit {
        let horizontal_edge =
            anchors.iter().any(|m| stacked(m) && ((old.t - m.b).abs() < 1.0 || (old.b - m.t).abs() < 1.0));
        p.scale = if horizontal_edge { old.w() / w.max(1) as f64 } else { old.h() / h.max(1) as f64 };
    }
    p.w = w;
    p.h = h;
    let new = p.rect();
    if anchors.iter().any(|m| beside(m) && (old.r - m.l).abs() < 1.0) {
        p.x = old.r - new.w(); // left of it: keep the right edge in place
    }
    if anchors.iter().any(|m| stacked(m) && (old.b - m.t).abs() < 1.0) {
        p.y = old.b - new.h(); // above it: keep the bottom edge in place
    }
    p
}

/// Put `new` into `screens` in place of that screen's old placement. Screens
/// that were joined to a monitor only through it, and no longer touch it
/// where it is now, are attached again at the nearest free edge. Returns
/// every placement that changed, `new` first.
pub fn place(mons: &[Monitor], screens: &mut Vec<ScreenPos>, new: ScreenPos) -> Vec<ScreenPos> {
    let reachable = |screens: &[ScreenPos]| -> Vec<String> {
        let rects: Vec<Rect> = screens.iter().map(|s| s.rect()).collect();
        let ok = joined(mons, &rects);
        screens.iter().zip(ok).filter(|(_, ok)| *ok).map(|(s, _)| s.name.clone()).collect()
    };
    let before = reachable(screens);
    screens.retain(|s| s.name != new.name);
    screens.push(new.clone());
    let mut changed = vec![new.clone()];
    for name in before.iter().filter(|n| **n != new.name) {
        if reachable(screens).contains(name) {
            continue;
        }
        let Some(i) = screens.iter().position(|s| &s.name == name) else { continue };
        let others: Vec<Rect> = screens.iter().filter(|s| &s.name != name).map(|s| s.rect()).collect();
        if let Some(p) = snap(mons, &others, &screens[i]) {
            screens[i] = p.clone();
            changed.push(p);
        }
    }
    changed
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// The user's real setup: 3 | 1 (primary) | 2 (portrait, taller).
    pub fn user_monitors() -> Vec<Monitor> {
        vec![
            Monitor { rect: Rect::new(-1920.0, 2.0, 0.0, 1082.0), label: "3".into(), primary: false },
            Monitor { rect: Rect::new(0.0, 0.0, 1920.0, 1080.0), label: "1".into(), primary: true },
            Monitor { rect: Rect::new(1920.0, -846.0, 3000.0, 1074.0), label: "2".into(), primary: false },
        ]
    }

    #[test]
    fn auto_place_avoids_taken_edges() {
        // Right of monitor 1 is monitor 2, so the first free edge is below monitor 1.
        let p = auto_place(&user_monitors(), &[], "surface", 1520, 1014);
        assert_eq!((p.x, p.y), (0.0, 1080.0));
        assert!((p.rect().w() - 1920.0).abs() < 1e-6, "fit to monitor 1's width");
        assert!(p.fit);
    }

    #[test]
    fn snap_to_bottom_of_monitor_1() {
        let mons = user_monitors();
        let dragged = ScreenPos { name: "s".into(), x: 300.0, y: 1150.0, scale: 1.0, w: 1520, h: 1014, fit: false };
        let p = snap(&mons, &[], &dragged).unwrap();
        assert_eq!(p.y, 1080.0);
        assert_eq!(p.x, 300.0, "keeps where you dropped it along the edge");
    }

    #[test]
    fn a_screen_left_of_a_monitor_stays_attached_when_it_shrinks() {
        let mons = vec![Monitor { rect: Rect::new(0.0, 0.0, 1920.0, 1080.0), label: "1".into(), primary: true }];
        let left = ScreenPos { name: "s".into(), x: -1920.0, y: 0.0, scale: 1.0, w: 1920, h: 1080, fit: false };
        let p = resize(&left, &mons, &[], 1280, 720);
        assert_eq!(p.rect().r, 0.0, "its right edge still touches the monitor");
        let above = ScreenPos { name: "s".into(), x: 0.0, y: -1200.0, scale: 1.0, w: 1920, h: 1200, fit: false };
        let p = resize(&above, &mons, &[], 1920, 1080);
        assert_eq!(p.rect().b, 0.0, "its bottom edge still touches the monitor");
        // To the right or below, the touching edge is the one that doesn't move.
        let right = ScreenPos { name: "s".into(), x: 1920.0, y: 0.0, scale: 1.0, w: 1920, h: 1080, fit: false };
        assert_eq!(resize(&right, &mons, &[], 1280, 720).x, 1920.0);
    }

    /// A friend's setup: one wide monitor, a laptop ("fedora") stretched
    /// below it, and a second laptop ("delacroix") to go left of that one.
    fn wide_monitor() -> Vec<Monitor> {
        vec![Monitor { rect: Rect::new(0.0, 0.0, 3440.0, 1440.0), label: "1".into(), primary: true }]
    }
    fn fedora() -> ScreenPos {
        candidate("fedora", &wide_monitor()[0].rect, Side::Bottom, 3072, 1920, true, None)
    }

    #[test]
    fn a_screen_snaps_to_the_edge_of_another_screen() {
        let mons = wide_monitor();
        let fedora = fedora().rect();
        // Dropped left of fedora, below and left of the monitor.
        let dragged = ScreenPos { name: "delacroix".into(), x: -1500.0, y: 1900.0, scale: 1.0, w: 1408, h: 881, fit: false };
        let p = snap(&mons, &[fedora], &dragged).unwrap();
        assert_eq!(p.rect().r, fedora.l, "its right edge is fedora's left edge");
        assert_eq!(p.y, 1900.0, "keeps where you dropped it along the edge");
        // Stretched, it covers fedora's whole left edge.
        let p = snap(&mons, &[fedora], &ScreenPos { fit: true, ..dragged }).unwrap();
        assert!((p.rect().t - fedora.t).abs() < 1e-6 && (p.rect().b - fedora.b).abs() < 1e-6);
        assert!((p.rect().r - fedora.l).abs() < 1e-6);
    }

    #[test]
    fn a_screen_never_snaps_to_one_the_pointer_cannot_reach() {
        let mons = wide_monitor();
        // A screen floating far away from every monitor is nothing to attach to.
        let adrift = Rect::xywh(9000.0, 9000.0, 1920.0, 1080.0);
        let dragged = ScreenPos { name: "s".into(), x: 7000.0, y: 9000.0, scale: 1.0, w: 1920, h: 1080, fit: false };
        let p = snap(&mons, &[adrift], &dragged).unwrap();
        assert!(touches(&p.rect(), &mons[0].rect), "attached to the monitor instead: {p:?}");
    }

    #[test]
    fn a_screen_left_behind_is_attached_again() {
        let mons = wide_monitor();
        let fedora = fedora();
        let delacroix =
            snap(&mons, &[fedora.rect()], &ScreenPos { name: "delacroix".into(), x: -1500.0, y: 1900.0, scale: 1.0, w: 1408, h: 881, fit: false })
                .unwrap();
        let mut screens = vec![fedora.clone(), delacroix.clone()];
        // fedora moves to the right of the monitor: delacroix would be left floating.
        let moved = candidate("fedora", &mons[0].rect, Side::Right, 3072, 1920, true, None);
        let changed = place(&mons, &mut screens, moved.clone());
        assert_eq!(changed[0], moved);
        assert_eq!(changed.len(), 2, "delacroix moved too");
        let rects: Vec<Rect> = screens.iter().map(|s| s.rect()).collect();
        assert_eq!(joined(&mons, &rects), vec![true, true], "both can be reached: {screens:?}");
        assert!(!rects[0].overlaps(&rects[1]) && !rects.iter().any(|r| r.overlaps(&mons[0].rect)));
        // A move that leaves it attached changes nothing else.
        let mut screens = vec![fedora.clone(), delacroix.clone()];
        let nudged = ScreenPos { w: 3000, ..fedora.clone() };
        assert_eq!(place(&mons, &mut screens, nudged).len(), 1);
        assert!(screens.contains(&delacroix));
    }

    #[test]
    fn a_screen_attached_to_another_follows_its_edge_when_it_shrinks() {
        let mons = wide_monitor();
        let fedora = fedora().rect();
        let left = snap(&mons, &[fedora], &ScreenPos { name: "d".into(), x: -1500.0, y: 1900.0, scale: 1.0, w: 1408, h: 881, fit: false })
            .unwrap();
        let p = resize(&left, &mons, &[fedora], 1280, 800);
        assert_eq!(p.rect().r, fedora.l, "its right edge still touches fedora");
    }

    #[test]
    fn with_every_monitor_edge_taken_a_new_screen_goes_next_to_another_screen() {
        let mons = wide_monitor();
        let mut rects: Vec<Rect> = Vec::new();
        for i in 0..5 {
            let p = auto_place(&mons, &rects, &format!("s{i}"), 1920, 1080);
            assert!(!collides(&p.rect(), &mons, &rects));
            rects.push(p.rect());
        }
        assert_eq!(joined(&mons, &rects), vec![true; 5], "the fifth is still reachable");
    }

    #[test]
    fn snap_never_overlaps_a_monitor() {
        let mons = user_monitors();
        let dragged = ScreenPos { name: "s".into(), x: 500.0, y: 300.0, scale: 1.0, w: 1520, h: 1014, fit: true };
        let p = snap(&mons, &[], &dragged).unwrap();
        assert!(!mons.iter().any(|m| m.rect.overlaps(&p.rect())));
    }
}
