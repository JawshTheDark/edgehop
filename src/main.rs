#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod clipboard;
mod client;
mod config;
#[cfg(windows)]
mod dragdrop;
mod diag;
mod files;
mod flight;
mod gui;
mod icon;
mod inject;
#[cfg(target_os = "linux")]
mod kde;
#[cfg(target_os = "linux")]
mod keyfix;
mod keymap;
mod layout;
#[cfg(target_os = "macos")]
mod mac_files;
#[cfg(target_os = "linux")]
mod login_screen;
mod net;
mod platform;
mod proto;
mod recovery;
mod selftest;
mod server;
mod state;
mod transfer;
mod tray;
mod update;
#[cfg(windows)]
mod win32;

fn main() {
    flight::init();
    platform::init_dpi();
    // If a previous run died while the cursor was hidden, bring it back.
    platform::restore_cursor();
    recovery::install_panic_hook();
    let args: Vec<String> = std::env::args().collect();
    let minimized = args.iter().any(|a| a == "--minimized");
    let headless = args.iter().any(|a| a == "--headless");
    // For a computer with no screen to click "pair" on: trust the first server that answers.
    if args.iter().any(|a| a == "--accept-pairing") {
        client::ACCEPT_PAIRING.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // Transfer speed test between two computers (see transfer.rs).
    if let Some(i) = args.iter().position(|a| a == "--bench-serve") {
        let path = std::path::PathBuf::from(args.get(i + 1).expect("--bench-serve <file or folder>"));
        if let Err(e) = transfer::bench_serve(&path) {
            state::print_err(&format!("bench: {e}"));
        }
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--bench-fetch") {
        let host = args.get(i + 1).expect("--bench-fetch <host> [connections...]");
        let counts: Vec<usize> = args[i + 2..].iter().filter_map(|a| a.parse().ok()).collect();
        for n in if counts.is_empty() { vec![4] } else { counts } {
            if let Err(e) = transfer::bench_fetch(host, n) {
                state::print_err(&format!("bench with {n} connection(s): {e}"));
            }
        }
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--write-icon") {
        let path = args.get(i + 1).map(String::as_str).unwrap_or("edgehop.png");
        let size = args.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(256).clamp(16, 1024);
        // "mac": on Apple's icon grid (clear space round the artwork), for the app bundle.
        let mac = args.get(i + 3).map(String::as_str) == Some("mac");
        std::fs::write(path, if mac { icon::png_mac(size) } else { icon::png(size) }).expect("write icon");
        return;
    }
    // Linux: the login-screen service (see login_screen.rs).
    #[cfg(target_os = "linux")]
    if args.iter().any(|a| a == "--login-screen") {
        login_screen::run(&args);
        return;
    }
    // Linux: switch that service on or off for the account asking (as root, via pkexec).
    #[cfg(target_os = "linux")]
    if let Some(i) = args.iter().position(|a| a == "--login-service") {
        std::process::exit(login_screen::service(args.get(i + 1).map(String::as_str) != Some("off")));
    }
    if let Some(i) = args.iter().position(|a| a == "--write-ico") {
        let path = args.get(i + 1).map(String::as_str).unwrap_or("edgehop.ico");
        std::fs::write(path, icon::ico()).expect("write icon");
        return;
    }
    // Windows Firewall rule for this copy of Edgehop (the installer runs this as admin).
    #[cfg(windows)]
    if let Some(i) = args.iter().position(|a| a == "--firewall") {
        let on = args.get(i + 1).map(String::as_str) != Some("off");
        match platform::set_firewall_rule(on) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                state::print_err(&e);
                std::process::exit(1);
            }
        }
    }
    // Where Edgehop keeps things, and whether it can write its log (diagnostics).
    if args.iter().any(|a| a == "--paths") {
        let log = state::log_path();
        println!("settings: {}", config::Config::path().display());
        println!("log:      {}", log.display());
        match std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            Ok(_) => println!("log opens for writing: yes"),
            Err(e) => println!("log opens for writing: no ({e})"),
        }
        println!("working directory: {}", std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default());
        return;
    }
    // Updates from GitHub Releases (see update.rs).
    if args.iter().any(|a| a == "--check-update" || a == "--update") {
        std::process::exit(update::cli(args.iter().any(|a| a == "--update")));
    }
    // Check clipboard and input injection on this machine (see selftest.rs).
    if args.iter().any(|a| a == "--self-test") {
        std::process::exit(selftest::run(&args));
    }
    // macOS: forget Edgehop's input permissions so the next launch asks again.
    #[cfg(target_os = "macos")]
    if args.iter().any(|a| a == "--reset-permissions") {
        let done = platform::reset_mac_permissions();
        for (service, ok) in &done {
            println!("{service}: {}", if *ok { "reset" } else { "failed" });
        }
        std::process::exit(if done.iter().all(|(_, ok)| *ok) { 0 } else { 1 });
    }

    let mut cfg = config::Config::load();
    // Start at sign-in on or off (the installer and uninstaller run this).
    if let Some(i) = args.iter().position(|a| a == "--autostart") {
        let on = args.get(i + 1).map(String::as_str) != Some("off");
        let result = autostart::set(on);
        if result.is_ok() {
            cfg.autostart = on;
            cfg.save();
        }
        match result {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                state::print_err(&e);
                std::process::exit(1);
            }
        }
    }
    // One Edgehop at a time: opening it again brings up the one that's running.
    let Some(mut instance) = platform::single_instance(args.iter().any(|a| a == "--restarting")) else {
        return;
    };
    let app = state::AppState::new(cfg);
    state::init_logger(app.clone(), &args);
    instance.listen(app.clone());
    // Restarted by an update that was clicked in the window: open the window
    // again, even though restarts and installers start Edgehop in the tray.
    let reopen = update::reopen_window_after_update();
    if reopen && minimized {
        log::info!("just updated from the window: opening it again instead of starting in the tray");
    }
    let minimized = minimized && !reopen;
    if let Some(problem) = config::Config::load_problem() {
        log::error!("{problem}");
    }
    if let Some(e) = state::log_open_error() {
        log::error!("can't write the log file {}: {e}", state::log_path().display());
    }
    log::info!(
        "Edgehop {} starting on {} (config: {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        config::Config::path().display()
    );
    {
        let c = app.cfg();
        log::info!(
            "{} · {} {} · role {:?} · name {:?} · log level {}",
            platform::local_datetime(),
            std::env::consts::OS,
            std::env::consts::ARCH,
            c.role,
            c.name,
            state::log_level_name()
        );
        log::info!("system: {}", platform::environment());
        for m in platform::monitors() {
            log::info!(
                "monitor {:?} {}x{} at ({}, {}){}",
                m.label,
                m.rect.w() as i32,
                m.rect.h() as i32,
                m.rect.l as i32,
                m.rect.t as i32,
                if m.primary { " (main)" } else { "" }
            );
        }
    }

    if args.iter().any(|a| a == "--which-window") {
        // Diagnostic: ask the desktop which window is under the pointer.
        match platform::client_hooks() {
            Some(h) => println!("drop folder under pointer: {:?}", h.drop_folder()),
            None => println!("no desktop hooks on this system"),
        }
        state::flush_logs();
        return;
    }

    files::cleanup_old();
    {
        // An Edgehop that starts at sign-in may run for weeks.
        let _ = std::thread::Builder::new().name("cleanup".into()).spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
            files::cleanup_old();
        });
    }
    #[cfg(unix)]
    quit_on_signals(app.clone());
    // Keep "start at login" pointing at this copy of the app (after updates,
    // renames, moves), off the startup path.
    if app.cfg().autostart {
        std::thread::spawn(|| match autostart::set(true) {
            Ok(()) => log::debug!("start at sign-in is set up"),
            Err(e) => log::warn!("couldn't refresh start at sign-in: {e}"),
        });
    }
    app.start_engine();
    update::start_checker(app.clone());

    if headless {
        log::info!("running headless (no window); press Ctrl+C to quit");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    run_windows(&app, minimized);
    app.stop_engine();
    state::flush_logs();
}

/// Unix: on SIGTERM, SIGINT or SIGHUP (logging out, shutting down, Ctrl+C in
/// a terminal), stop sharing first: release held keys, show the pointer and
/// undo screen dimming, which would otherwise be left as they are.
#[cfg(unix)]
fn quit_on_signals(app: std::sync::Arc<state::AppState>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ASKED: AtomicBool = AtomicBool::new(false);
    extern "C" fn on_signal(_: libc::c_int) {
        ASKED.store(true, Ordering::Relaxed); // all a signal handler may safely do
    }
    unsafe {
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::signal(sig, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    let _ = std::thread::Builder::new().name("signals".into()).spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if ASKED.load(Ordering::Relaxed) {
            log::info!("asked to quit (signal); stopping");
            app.quit();
        }
    });
}

/// Show the window, until Edgehop quits. On Linux the window closes into the
/// tray and a new one opens from the tray icon (a hidden window doesn't
/// reliably come back there); with --minimized, it starts in the tray.
fn run_windows(app: &std::sync::Arc<state::AppState>, minimized: bool) {
    if !platform::CLOSE_TO_TRAY {
        if let Err(e) = gui::run(app.clone(), minimized) {
            log::error!("the window failed: {e}");
        }
        return;
    }
    #[cfg(target_os = "linux")]
    let _tray = tray::create(app.clone());
    // The desktop needs a moment to pick up the tray icon.
    let start = std::time::Instant::now();
    while minimized && !app.tray_is_shown() && start.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let mut open = !minimized || !app.tray_is_shown();
    if !open {
        log::info!("started in the tray");
    }
    loop {
        if open {
            // No tray to come back to: start minimized rather than invisible.
            if let Err(e) = gui::run(app.clone(), minimized && !app.tray_is_shown()) {
                log::error!("the window failed: {e}");
            }
        }
        if !app.tray_is_shown() {
            return; // closing the window quits when there's no tray
        }
        app.wait_for_window_request();
        open = true;
    }
}
