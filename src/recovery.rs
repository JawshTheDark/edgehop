//! Self-healing: contain crashes in background workers, notice when parts of
//! edgehop get stuck, and recover without the user having to step in.
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;
use std::time::Instant;

/// Milliseconds on a monotonic clock (for "last heard from" bookkeeping).
/// Starts at a large offset so "N seconds ago" never clamps to zero right
/// after startup (which would read as "just now").
pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    1_000_000_000 + START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Run one unit of work; a panic is logged (by the panic hook) and contained,
/// so the worker thread that called us keeps running.
pub fn guard<T>(what: &str, f: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(_) => {
            log::error!("{what} crashed; recovered and carrying on");
            None
        }
    }
}

/// Log panics (with thread and location) into edgehop.log, and put the
/// cursor back if we had hidden it.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        log::error!("crash in thread '{thread}': {info}");
        crate::state::flush_logs();
        crate::platform::restore_cursor();
        prev(info);
    }));
}

/// Kill helper processes we started (clipboard tools etc.) that are stuck.
pub fn kill_child_helpers() {
    #[cfg(target_os = "linux")]
    {
        let me = std::process::id().to_string();
        let Ok(procs) = std::fs::read_dir("/proc") else { return };
        for p in procs.flatten() {
            let pid = p.file_name().to_string_lossy().into_owned();
            if !pid.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            // /proc/<pid>/stat: "pid (comm) state ppid ..."; comm may contain spaces.
            let Ok(stat) = std::fs::read_to_string(p.path().join("stat")) else { continue };
            let Some(after) = stat.rsplit_once(')').map(|(_, rest)| rest) else { continue };
            if after.split_whitespace().nth(1) == Some(me.as_str()) {
                let name = stat.split_once('(').and_then(|(_, r)| r.rsplit_once(')')).map(|(n, _)| n).unwrap_or("?");
                log::warn!("killing stuck helper {name} ({pid})");
                if let Ok(pid) = pid.parse::<i32>() {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
}

/// Last resort: start a fresh edgehop and exit this one.
pub fn restart_self(reason: &str) -> ! {
    log::error!("restarting Edgehop: {reason}");
    crate::platform::restore_cursor();
    // Under a systemd service (Restart=on-failure) systemd starts us again;
    // otherwise launch the new copy ourselves.
    if std::env::var_os("INVOCATION_ID").is_none() {
        if let Ok(exe) = std::env::current_exe() {
            let mut args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--minimized").collect();
            args.push("--minimized".into());
            let _ = std::process::Command::new(exe).args(&args).spawn();
        }
    }
    crate::state::flush_logs();
    std::process::exit(75);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_contains_panics() {
        assert_eq!(guard("ok", || 7), Some(7));
        assert_eq!(guard("boom", || -> i32 { panic!("test panic") }), None);
    }

    #[test]
    fn clock_moves_forward() {
        let a = now_ms();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(now_ms() > a);
    }
}
