//! "Start at login" for each OS.

pub fn set(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().to_string();
    imp(enabled, &exe)
}

#[cfg(windows)]
fn imp(enabled: bool, exe: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let mut cmd = std::process::Command::new("reg");
    if enabled {
        cmd.args(["add", key, "/v", "kvmlink", "/t", "REG_SZ", "/d", &format!("\"{exe}\" --minimized"), "/f"]);
    } else {
        cmd.args(["delete", key, "/v", "kvmlink", "/f"]);
    }
    let out = cmd.creation_flags(CREATE_NO_WINDOW).output().map_err(|e| e.to_string())?;
    if out.status.success() || !enabled {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

#[cfg(target_os = "linux")]
fn imp(enabled: bool, exe: &str) -> Result<(), String> {
    let dir = directories::BaseDirs::new()
        .map(|b| b.config_dir().join("autostart"))
        .ok_or("no config dir")?;
    let file = dir.join("kvmlink.desktop");
    if enabled {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let body = format!(
            // Run as a systemd user service so it's restarted if it ever dies.
            "[Desktop Entry]\nType=Application\nName=kvmlink\nExec=systemd-run --user --collect --unit=kvmlink-app -p Restart=on-failure -p RestartSec=2 {exe} --minimized\nIcon=kvmlink\nX-GNOME-Autostart-enabled=true\n"
        );
        std::fs::write(&file, body).map_err(|e| e.to_string())
    } else {
        let _ = std::fs::remove_file(&file);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn imp(enabled: bool, exe: &str) -> Result<(), String> {
    let dir = directories::BaseDirs::new()
        .map(|b| b.home_dir().join("Library/LaunchAgents"))
        .ok_or("no home dir")?;
    let file = dir.join("com.kvmlink.agent.plist");
    if enabled {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.kvmlink.agent</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>--minimized</string></array>
  <key>RunAtLoad</key><true/>
</dict></plist>
"#
        );
        std::fs::write(&file, body).map_err(|e| e.to_string())
    } else {
        let _ = std::fs::remove_file(&file);
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn imp(_enabled: bool, _exe: &str) -> Result<(), String> {
    Err("not supported on this OS".into())
}
