//! Screen geometry. Everything lives in one coordinate space: the server's own
//! desktop pixels (the same coordinates Windows uses for its monitors). Each
//! client screen is a rectangle in that space, attached to the edge of a
//! monitor, so crossing an edge is plain geometry and position is continuous.
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

/// First free monitor edge, starting with the primary monitor.
pub fn auto_place(mons: &[Monitor], others: &[Rect], name: &str, w: i32, h: i32) -> ScreenPos {
    let (w, h) = if w > 0 && h > 0 { (w, h) } else { (1920, 1080) };
    let mut order: Vec<&Monitor> = mons.iter().filter(|m| m.primary).collect();
    order.extend(mons.iter().filter(|m| !m.primary));
    for m in &order {
        for side in SIDES {
            let c = candidate(name, &m.rect, side, w, h, true, None);
            if !collides(&c.rect(), mons, others) {
                return c;
            }
        }
    }
    // Everything is taken: put it to the right of everything.
    let right = mons.iter().map(|m| m.rect.r).chain(others.iter().map(|o| o.r)).fold(0.0, f64::max);
    ScreenPos { name: name.into(), x: right, y: 0.0, scale: 1.0, w, h, fit: false }
}

/// After a drag: attach the screen to the nearest monitor edge where it fits.
pub fn snap(mons: &[Monitor], others: &[Rect], pos: &ScreenPos) -> Option<ScreenPos> {
    let cur = pos.rect();
    let (cx, cy) = cur.center();
    let mut best: Option<(f64, ScreenPos)> = None;
    for m in mons {
        for side in SIDES {
            let along = match side {
                Side::Top | Side::Bottom => cur.l,
                Side::Left | Side::Right => cur.t,
            };
            let c = candidate(&pos.name, &m.rect, side, pos.w, pos.h, pos.fit, Some(along));
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
/// fit, and still touching the monitor edge it was attached to (a screen to
/// the left of or above a monitor would otherwise drift away from it as it
/// shrinks, and past a small gap the pointer can no longer cross).
pub fn resize(pos: &ScreenPos, mons: &[Monitor], w: i32, h: i32) -> ScreenPos {
    let mut p = pos.clone();
    let old = pos.rect();
    let beside = |m: &Rect| old.t < m.b && m.t < old.b; // shares some height
    let stacked = |m: &Rect| old.l < m.r && m.l < old.r; // shares some width
    if p.fit {
        let horizontal_edge =
            mons.iter().any(|m| stacked(&m.rect) && ((old.t - m.rect.b).abs() < 1.0 || (old.b - m.rect.t).abs() < 1.0));
        p.scale = if horizontal_edge { old.w() / w.max(1) as f64 } else { old.h() / h.max(1) as f64 };
    }
    p.w = w;
    p.h = h;
    let new = p.rect();
    if mons.iter().any(|m| beside(&m.rect) && (old.r - m.rect.l).abs() < 1.0) {
        p.x = old.r - new.w(); // left of a monitor: keep the right edge in place
    }
    if mons.iter().any(|m| stacked(&m.rect) && (old.b - m.rect.t).abs() < 1.0) {
        p.y = old.b - new.h(); // above a monitor: keep the bottom edge in place
    }
    p
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
        let p = resize(&left, &mons, 1280, 720);
        assert_eq!(p.rect().r, 0.0, "its right edge still touches the monitor");
        let above = ScreenPos { name: "s".into(), x: 0.0, y: -1200.0, scale: 1.0, w: 1920, h: 1200, fit: false };
        let p = resize(&above, &mons, 1920, 1080);
        assert_eq!(p.rect().b, 0.0, "its bottom edge still touches the monitor");
        // To the right or below, the touching edge is the one that doesn't move.
        let right = ScreenPos { name: "s".into(), x: 1920.0, y: 0.0, scale: 1.0, w: 1920, h: 1080, fit: false };
        assert_eq!(resize(&right, &mons, 1280, 720).x, 1920.0);
    }

    #[test]
    fn snap_never_overlaps_a_monitor() {
        let mons = user_monitors();
        let dragged = ScreenPos { name: "s".into(), x: 500.0, y: 300.0, scale: 1.0, w: 1520, h: 1014, fit: true };
        let p = snap(&mons, &[], &dragged).unwrap();
        assert!(!mons.iter().any(|m| m.rect.overlaps(&p.rect())));
    }
}
