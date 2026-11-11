//! Input flight recorder: the last couple of thousand raw input events and
//! switching decisions, kept in memory at almost no cost, whatever the log
//! level. It's written out when something looks wrong (the pointer bouncing
//! between screens), and into every diagnostics report, so a problem can be
//! understood from what actually happened rather than guessed at.
use crate::state::lk;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const CAPACITY: usize = 2000;

#[derive(Clone)]
struct Entry {
    /// Milliseconds since Edgehop started.
    ms: u64,
    what: &'static str,
    /// Space-separated names for `v`'s values, e.g. "x y dx dy".
    fields: &'static str,
    v: [i64; 5],
    /// For rare events (switches): free text. Empty otherwise (no allocation).
    text: String,
}

static RING: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());
static START: OnceLock<Instant> = OnceLock::new();

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Start the clock (call early, so times read as "since Edgehop started").
pub fn init() {
    let _ = now_ms();
}

fn push(e: Entry) {
    let mut ring = lk(&RING);
    if ring.len() >= CAPACITY {
        ring.pop_front();
    }
    ring.push_back(e);
}

/// Record a raw event: `fields` names the values, in order.
pub fn rec(what: &'static str, fields: &'static str, v: &[i64]) {
    let mut vals = [0i64; 5];
    for (d, s) in vals.iter_mut().zip(v) {
        *d = *s;
    }
    push(Entry { ms: now_ms(), what, fields, v: vals, text: String::new() });
}

/// Keys that type something (letters, digits, punctuation, space, the number
/// pad), by Linux key code. The recorder notes *that* one was pressed, never
/// which: it ends up in logs and in diagnostics reports people share, and
/// would otherwise hold whatever was typed, passwords included. Modifiers,
/// function keys and navigation keys keep their codes: they're what switching
/// problems are about.
pub fn types_text(evdev: u16) -> bool {
    matches!(evdev, 2..=13 | 16..=27 | 30..=41 | 43..=53 | 55 | 57 | 71..=83 | 86 | 98)
}

/// The key code to record: 0 for keys that type something.
#[cfg_attr(windows, allow(dead_code))]
pub fn key_id(evdev: u16) -> i64 {
    if types_text(evdev) {
        0
    } else {
        evdev as i64
    }
}

/// Record a decision or rare event, in words.
pub fn note(what: &'static str, text: impl Into<String>) {
    push(Entry { ms: now_ms(), what, fields: "", v: [0; 5], text: text.into() });
}

/// Everything recorded, oldest first, one event per line.
pub fn dump() -> String {
    // Copy, then format without holding the lock the input hooks record into.
    let ring: Vec<Entry> = lk(&RING).iter().cloned().collect();
    let mut out = String::with_capacity(ring.len() * 64);
    let _ = writeln!(
        out,
        "input flight recorder: last {} events (times are seconds since Edgehop started, now +{:.3}s)",
        ring.len(),
        now_ms() as f64 / 1000.0
    );
    for e in ring.iter() {
        let _ = write!(out, "+{:>10.3}s  {:<14}", e.ms as f64 / 1000.0, e.what);
        for (name, v) in e.fields.split_whitespace().zip(e.v.iter()) {
            let _ = write!(out, " {name}={v}");
        }
        if !e.text.is_empty() {
            let _ = write!(out, " {}", e.text);
        }
        out.push('\n');
    }
    out
}

/// Write the recorder to the log (something looked wrong).
pub fn log_dump(reason: &str) {
    log::warn!("{reason}: writing the input flight recorder to the log");
    for line in dump().lines() {
        log::warn!("  {line}");
    }
}

#[cfg(test)]
mod tests {
    // The recorder is global: these two mustn't interleave.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn records_and_formats_events() {
        let _g = ONE_AT_A_TIME.lock();
        super::rec("test move", "x y dx dy", &[10, 20, -3, 4]);
        super::note("test switch", "a -> b");
        let d = super::dump();
        assert!(d.contains("test move") && d.contains("x=10 y=20 dx=-3 dy=4"), "{d}");
        assert!(d.contains("test switch") && d.contains("a -> b"), "{d}");
    }

    #[test]
    fn keeps_only_the_latest() {
        let _g = ONE_AT_A_TIME.lock();
        for i in 0..(super::CAPACITY + 50) {
            super::rec("fill", "i", &[i as i64]);
        }
        assert!(super::lk(&super::RING).len() <= super::CAPACITY);
    }
}
