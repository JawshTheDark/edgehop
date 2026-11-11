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
    relaunch()
}

/// Start the newly installed version in place of this one.
pub fn restart_after_update(version: &str) -> ! {
    log::info!("restarting into Edgehop {version}");
    relaunch()
}

fn relaunch() -> ! {
    crate::platform::restore_cursor();
    // Under a systemd service that restarts us (Restart=on-failure), systemd
    // starts us again; otherwise launch the new copy ourselves.
    if !systemd_restarts_us() {
        // Linux reports a program replaced by an update as "... (deleted)".
        if let Some(exe) = crate::update::own_exe() {
            let mut args: Vec<String> =
                std::env::args().skip(1).filter(|a| a != "--minimized" && a != "--restarting").collect();
            args.push("--minimized".into());
            // The new copy waits for this one to exit (one Edgehop at a time).
            args.push("--restarting".into());
            let mut cmd = std::process::Command::new(&exe);
            // Inside a systemd service that won't restart us (e.g. a desktop's
            // autostart entry), a child would be stopped along with this one:
            // start the new copy as a unit of its own.
            #[cfg(target_os = "linux")]
            if std::env::var_os("INVOCATION_ID").is_some() && which("systemd-run") {
                cmd = std::process::Command::new("systemd-run");
                if unsafe { libc::geteuid() } != 0 {
                    cmd.arg("--user");
                }
                cmd.args(["--collect", "--quiet", "--"]).arg(&exe);
            }
            if let Err(e) = cmd.args(&args).spawn() {
                log::error!("couldn't start Edgehop again ({}): {e}", exe.display());
            }
        }
    }
    crate::state::flush_logs();
    std::process::exit(75);
}

#[cfg(target_os = "linux")]
fn which(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// Linux: running as a systemd service that starts us again after exit code 75.
#[cfg(target_os = "linux")]
fn systemd_restarts_us() -> bool {
    if std::env::var_os("INVOCATION_ID").is_none() {
        return false;
    }
    let unit = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|c| c.lines().find_map(|l| l.strip_prefix("0::").map(|p| p.rsplit('/').next().unwrap_or("").to_string())));
    let Some(unit) = unit.filter(|u| u.ends_with(".service")) else { return false };
    let mut c = std::process::Command::new("systemctl");
    if unsafe { libc::geteuid() } != 0 {
        c.arg("--user");
    }
    let restart = c
        .args(["show", "-p", "Restart", "--value", &unit])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    log::info!("running as systemd unit {unit} (Restart={restart})");
    matches!(restart.as_str(), "on-failure" | "always")
}

#[cfg(not(target_os = "linux"))]
fn systemd_restarts_us() -> bool {
    false
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
