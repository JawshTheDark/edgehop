#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod clipboard;
mod client;
mod config;
#[cfg(windows)]
mod dragdrop;
mod files;
mod gui;
mod icon;
mod inject;
#[cfg(target_os = "linux")]
mod kde;
mod keymap;
mod layout;
mod net;
mod platform;
mod proto;
mod recovery;
mod server;
mod state;
mod transfer;
mod tray;
#[cfg(windows)]
mod win32;

fn main() {
    platform::init_dpi();
    // If a previous run died while the cursor was hidden, bring it back.
    platform::restore_cursor();
    recovery::install_panic_hook();
    let args: Vec<String> = std::env::args().collect();
    let minimized = args.iter().any(|a| a == "--minimized");
    let headless = args.iter().any(|a| a == "--headless");
    // Transfer speed test between two computers (see transfer.rs).
    if let Some(i) = args.iter().position(|a| a == "--bench-serve") {
        let path = std::path::PathBuf::from(args.get(i + 1).expect("--bench-serve <file or folder>"));
        if let Err(e) = transfer::bench_serve(&path) {
            eprintln!("bench: {e}");
        }
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--bench-fetch") {
        let host = args.get(i + 1).expect("--bench-fetch <host> [connections...]");
        let counts: Vec<usize> = args[i + 2..].iter().filter_map(|a| a.parse().ok()).collect();
        for n in if counts.is_empty() { vec![4] } else { counts } {
            if let Err(e) = transfer::bench_fetch(host, n) {
                eprintln!("bench with {n} connection(s): {e}");
            }
        }
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--write-icon") {
        let path = args.get(i + 1).map(String::as_str).unwrap_or("edgehop.png");
        std::fs::write(path, icon::png(256)).expect("write icon");
        return;
    }

    let cfg = config::Config::load();
    let app = state::AppState::new(cfg);
    state::init_logger(app.clone());
    log::info!(
        "Edgehop {} starting on {} (config: {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        config::Config::path().display()
    );

    if args.iter().any(|a| a == "--which-window") {
        // Diagnostic: ask the desktop which window is under the pointer.
        match platform::client_hooks() {
            Some(h) => println!("drop folder under pointer: {:?}", h.drop_folder()),
            None => println!("no desktop hooks on this system"),
        }
        return;
    }

    files::cleanup_old();
    // Keep "start at login" pointing at this copy of the app (after updates, renames, moves).
    if app.cfg().autostart {
        if let Err(e) = autostart::set(true) {
            log::warn!("couldn't refresh start-at-login: {e}");
        }
    }
    app.start_engine();

    if headless {
        log::info!("running headless (no window); press Ctrl+C to quit");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    if let Err(e) = gui::run(app.clone(), minimized) {
        eprintln!("GUI failed: {e}");
    }
    app.stop_engine();
}
