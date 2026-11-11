//! "Start at login" for each OS.

pub fn set(enabled: bool) -> Result<(), String> {
    // (Linux calls a program replaced by an update "... (deleted)".)
    let exe = crate::update::own_exe().ok_or("can't tell where Edgehop is")?;
    let exe = exe.to_string_lossy().to_string();
    if enabled && exe.contains("/AppTranslocation/") {
        // macOS is running a temporary copy; that path is gone after a restart.
        return Err("move Edgehop.app to your Applications folder and open it from there first".into());
    }
    imp(enabled, &exe)
}

/// Windows: a Task Scheduler task that starts Edgehop in the tray when you
/// sign in. (The registry "Run" list it used before isn't dependable: Windows
/// was seen skipping entries in it at sign-in.)
#[cfg(windows)]
fn imp(enabled: bool, exe: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const TASK: &str = "Edgehop";
    let run = |program: &str, args: &[&str]| {
        std::process::Command::new(program).args(args).creation_flags(CREATE_NO_WINDOW).output().map_err(|e| e.to_string())
    };
    // The old way, from 0.2.7 and before.
    let _ = run("reg", &["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "Edgehop", "/f"]);
    if !enabled {
        let _ = run("schtasks", &["/Delete", "/TN", TASK, "/F"]);
        return Ok(());
    }
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => return Err("can't tell who's signed in".into()),
    };
    let xml_escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts Edgehop (keyboard and mouse sharing) in the tray when you sign in.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{}</Command>
      <Arguments>--minimized</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        xml_escape(exe),
        user = xml_escape(&user)
    );
    // schtasks reads task XML as UTF-16 with a byte-order mark.
    let mut bytes = vec![0xFF, 0xFE];
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let file = std::env::temp_dir().join(format!("edgehop-task-{}.xml", std::process::id()));
    std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
    let out = run("schtasks", &["/Create", "/TN", TASK, "/XML", &file.to_string_lossy(), "/F"]);
    let _ = std::fs::remove_file(&file);
    let out = out?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Task Scheduler said: {}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(target_os = "linux")]
fn imp(enabled: bool, exe: &str) -> Result<(), String> {
    let dir = directories::BaseDirs::new()
        .map(|b| b.config_dir().join("autostart"))
        .ok_or("no config dir")?;
    let file = dir.join("edgehop.desktop");
    if enabled {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Under systemd: as a user service, so it's restarted if it ever dies.
        let run = if std::path::Path::new("/run/systemd/system").exists() {
            format!("systemd-run --user --collect --unit=edgehop -p Restart=on-failure -p RestartSec=2 \"{exe}\" --minimized")
        } else {
            format!("\"{exe}\" --minimized")
        };
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=Edgehop\nExec={run}\nIcon=edgehop\nX-GNOME-Autostart-enabled=true\n"
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
    let file = dir.join("com.edgehop.agent.plist");
    if enabled {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.edgehop.agent</string>
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
