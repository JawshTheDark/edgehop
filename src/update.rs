//! Updates from GitHub Releases: a check soon after start and every few
//! hours, a notification when there's a newer version, and a one-click update.
//!
//! Downloads use the system's own curl (Windows 10 and later, macOS and most
//! Linux systems have it; wget otherwise), so the system's certificates and
//! proxy settings apply and no TLS library is built into Edgehop. Every
//! download is checked against the SHA-256 GitHub publishes for the file.
//!
//! Installing: Windows runs the installer silently (it stops Edgehop, updates
//! it and starts it again in the tray). A Mac swaps its Edgehop.app for the new
//! one and reopens it. Linux installs the new .deb or .rpm through pkexec
//! (which asks for your password), then restarts Edgehop. Anything else (the
//! tarball, a build from source) is pointed to the download page.
use crate::state::{lk, AppState};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

const REPO: &str = "JawshTheDark/edgehop";
pub const RELEASES_PAGE: &str = "https://github.com/JawshTheDark/edgehop/releases";
const FIRST_CHECK: Duration = Duration::from_secs(20);
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// After a failed check (no network yet at sign-in, say): try again sooner.
const RETRY: Duration = Duration::from_secs(15 * 60);
/// How often the checker wakes to see whether a check is due.
const TICK: Duration = Duration::from_secs(5 * 60);
/// A release this new may still be uploading its files.
const UPLOAD_GRACE_SECS: i64 = 2 * 3600;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    /// The newest release, which is what's running (or older).
    UpToDate(String),
    Available,
    /// Downloading or installing: what's happening.
    Working(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub page: String,
    /// When it was published (seconds since 1970).
    pub published: Option<i64>,
    /// The file for this computer, if it can update itself.
    pub asset: Option<Asset>,
    /// Why it can't, if it can't.
    pub manual: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Default)]
pub struct UpdateState {
    pub status: Status,
    pub release: Option<Release>,
    /// The version we last sent a desktop notification about.
    notified: Option<String>,
}

/// The running version (EDGEHOP_PRETEND_VERSION overrides it, for testing updates).
pub fn current() -> String {
    std::env::var("EDGEHOP_PRETEND_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().unwrap_or(Some(0))?, parts.next().unwrap_or(Some(0))?))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(a), Some(b)) if a > b)
}

// ---------------------------------------------------------------------------
// Downloading
// ---------------------------------------------------------------------------

fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// GET a URL's body. Files come through memory too (they're a few MB), so
/// curl never has to open our paths: Windows' curl.exe can't open a path with
/// characters outside the system code page (a user name in another script).
fn fetch(url: &str, big: bool, api: bool) -> Result<Vec<u8>, String> {
    let agent = format!("Edgehop/{} (update check)", env!("CARGO_PKG_VERSION"));
    let timeout = if big { "900" } else { "30" };
    let mut curl = command(system_tool("curl"));
    curl.args(["-fsSL", "--retry", "2", "--max-time", timeout, "-A", &agent]);
    if api {
        curl.args(["-H", "Accept: application/vnd.github+json"]);
    }
    curl.arg(url);
    match curl.output() {
        Ok(o) if o.status.success() => return Ok(o.stdout),
        Ok(o) => return Err(format!("curl: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("couldn't run curl: {e}")),
    }
    let mut wget = command("wget");
    wget.args(["-q", "-T", "30", "-U", &agent]);
    if api {
        wget.arg("--header=Accept: application/vnd.github+json");
    }
    wget.args(["-O", "-"]).arg(url);
    match wget.output() {
        Ok(o) if o.status.success() => Ok(o.stdout),
        Ok(o) => Err(format!("wget: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err("neither curl nor wget is installed, so Edgehop can't download updates".into())
        }
        Err(e) => Err(format!("couldn't run wget: {e}")),
    }
}

/// Windows looks for a program next to the one starting it before anywhere
/// else, and Edgehop may be run from Downloads: name the system's own.
fn system_tool(name: &str) -> PathBuf {
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        let p = Path::new(&root).join("System32").join(format!("{name}.exe"));
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(name)
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// A new folder only this user can use, for one download. Never a fixed name
/// in a shared folder: on Linux, /tmp is shared, and someone else could have
/// made that folder first and swap the package between our checksum test and
/// it being installed as root.
fn private_dir(version: &str) -> Result<PathBuf, String> {
    let base = std::env::temp_dir();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let template = base.join(format!("edgehop-update-{version}-XXXXXX"));
        let mut t = std::ffi::CString::new(template.as_os_str().as_bytes()).map_err(|e| e.to_string())?.into_bytes_with_nul();
        // mkdtemp: created new, mode 0700, or fails.
        let p = unsafe { libc::mkdtemp(t.as_mut_ptr() as *mut libc::c_char) };
        if p.is_null() {
            return Err(format!("couldn't make a folder in {}: {}", base.display(), std::io::Error::last_os_error()));
        }
        t.pop();
        return Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&t)));
    }
    // Windows: %TEMP% is the user's own; still start from an empty folder.
    #[allow(unreachable_code)]
    {
        let dir = base.join(format!("edgehop-update-{version}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(dir)
    }
}

/// Download an asset and make sure it's the published file.
fn download(asset: &Asset, dir: &Path) -> Result<PathBuf, String> {
    let file = dir.join(&asset.name);
    let body = fetch(&asset.url, true, false)?;
    std::fs::write(&file, &body).map_err(|e| format!("couldn't save {}: {e}", file.display()))?;
    let size = body.len() as u64;
    drop(body);
    if asset.size != 0 && size != asset.size {
        return Err(format!("the download is {size} bytes; GitHub says {} (try again)", asset.size));
    }
    let hash = sha256_hex(&file)?;
    match &asset.sha256 {
        Some(want) if !want.eq_ignore_ascii_case(&hash) => {
            let _ = std::fs::remove_file(&file);
            return Err(format!("the download's SHA-256 is {hash}, not the published {want}; not installing it"));
        }
        Some(_) => log::info!("downloaded {} ({size} bytes), SHA-256 matches GitHub's: {hash}", asset.name),
        None => {
            let _ = std::fs::remove_file(&file);
            return Err(format!("GitHub published no checksum for {} to check the download against; not installing it", asset.name));
        }
    }
    Ok(file)
}

// ---------------------------------------------------------------------------
// What to install on this computer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)] // each platform makes only its own
pub enum Kind {
    WindowsInstaller,
    /// The Edgehop.app this is running from.
    MacApp(PathBuf),
    Deb,
    Rpm,
    /// Can't update itself; why.
    Manual(String),
}

/// This program's file (Linux reports a replaced one as "... (deleted)").
pub fn own_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let s = exe.to_string_lossy();
    Some(s.strip_suffix(" (deleted)").map(PathBuf::from).unwrap_or(exe))
}

pub fn install_kind() -> Kind {
    #[cfg(windows)]
    return Kind::WindowsInstaller;
    #[cfg(target_os = "macos")]
    {
        let exe = own_exe().unwrap_or_default();
        return match exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")) {
            Some(bundle) => Kind::MacApp(bundle.to_path_buf()),
            None => Kind::Manual("Edgehop isn't running from Edgehop.app".into()),
        };
    }
    #[cfg(target_os = "linux")]
    {
        let exe = own_exe().unwrap_or_default();
        if exe != Path::new("/usr/bin/edgehop") {
            return Kind::Manual(format!("Edgehop runs from {} (not the .deb or .rpm package)", exe.display()));
        }
        let owned_by = |cmd: &str, args: &[&str]| {
            Command::new(cmd).args(args).output().is_ok_and(|o| o.status.success())
        };
        if owned_by("dpkg-query", &["-S", "/usr/bin/edgehop"]) {
            return Kind::Deb;
        }
        if owned_by("rpm", &["-qf", "/usr/bin/edgehop"]) {
            if Path::new("/run/ostree-booted").exists() {
                // Silverblue, Kinoite...: /usr is read-only; packages are layered.
                return Kind::Manual("this system installs packages with rpm-ostree".into());
            }
            return Kind::Rpm;
        }
        return Kind::Manual("/usr/bin/edgehop doesn't belong to a package".into());
    }
    #[allow(unreachable_code)]
    Kind::Manual("updates aren't supported on this system".into())
}

/// The release file for this kind of install.
pub fn asset_name(kind: &Kind, version: &str, arch: &str) -> Option<String> {
    let deb_arch = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        a => a,
    };
    match kind {
        Kind::WindowsInstaller => Some(format!("Edgehop-Setup-{version}.exe")),
        Kind::MacApp(_) => Some(format!("Edgehop-v{version}-macos-universal.zip")),
        Kind::Deb => Some(format!("edgehop_{version}_{deb_arch}.deb")),
        Kind::Rpm => Some(format!("edgehop-{version}-1.{arch}.rpm")),
        Kind::Manual(_) => None,
    }
}

/// The newest release, from GitHub's API response.
fn parse_release(json: &[u8], kind: &Kind, arch: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("GitHub's answer has no version")?;
    let version = tag.trim_start_matches('v').to_string();
    // It becomes part of file and folder names: nothing but a plain version.
    let plain = !version.is_empty()
        && version.len() <= 40
        && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        && !version.contains("..");
    if !plain {
        return Err(format!("GitHub's answer has an odd version ({tag:?})"));
    }
    let page = v["html_url"].as_str().filter(|u| u.starts_with("https://github.com/")).unwrap_or(RELEASES_PAGE).to_string();
    let published = v["published_at"].as_str().and_then(unix_time);
    let want = asset_name(kind, &version, arch);
    let asset = want.as_ref().and_then(|want| {
        v["assets"].as_array()?.iter().find(|a| a["name"].as_str() == Some(want)).map(|a| Asset {
            name: want.clone(),
            url: a["browser_download_url"].as_str().unwrap_or_default().to_string(),
            size: a["size"].as_u64().unwrap_or(0),
            sha256: a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_string),
        })
    });
    let asset = asset.filter(|a| a.url.starts_with("https://"));
    let manual = match (kind, &asset, &want) {
        (Kind::Manual(why), ..) => Some(why.clone()),
        (_, None, Some(want)) => Some(format!("this release has no {want}")),
        _ => None,
    };
    Ok(Release { version, page, published, asset, manual })
}

/// Seconds since 1970 for GitHub's "2026-11-11T04:30:00Z".
fn unix_time(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub fn latest() -> Result<Release, String> {
    let body = fetch(&format!("https://api.github.com/repos/{REPO}/releases/latest"), false, true)?;
    parse_release(&body, &install_kind(), std::env::consts::ARCH)
}

// ---------------------------------------------------------------------------
// Checking (in the background, and on request)
// ---------------------------------------------------------------------------

fn set_status(app: &AppState, s: Status) {
    lk(&app.update).status = s;
    app.repaint();
}

/// Set the status unless an update is being installed (a check that ran
/// alongside must not bring the Update button back mid-install).
fn set_status_unless_busy(app: &AppState, s: Status) -> bool {
    let mut u = lk(&app.update);
    if matches!(u.status, Status::Working(_)) {
        return false;
    }
    u.status = s;
    drop(u);
    app.repaint();
    true
}

/// An update found earlier is still on offer when a later check can't
/// reach GitHub: keep saying so rather than `otherwise`.
fn known_or(app: &AppState, otherwise: Status) -> Status {
    if lk(&app.update).release.is_some() {
        Status::Available
    } else {
        otherwise
    }
}

/// Downloads left behind (a failed update, an installer that has finished):
/// clear out those more than a day old.
fn sweep_old_downloads() {
    let Ok(dir) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in dir.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(24 * 3600));
        if old && e.file_name().to_string_lossy().starts_with("edgehop-update-") {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Ask GitHub for the newest release; on a new one, say so (once per version).
/// Returns false if it should be asked again soon.
pub fn check(app: &Arc<AppState>, by_hand: bool) -> bool {
    if !set_status_unless_busy(app, Status::Checking) {
        return true;
    }
    let cur = current();
    match latest() {
        Ok(rel)
            if is_newer(&rel.version, &cur)
                && rel.asset.is_none()
                && !matches!(install_kind(), Kind::Manual(_))
                && rel.published.is_some_and(|t| now_unix() - t < UPLOAD_GRACE_SECS) =>
        {
            // Just published: its files may still be uploading. Look again soon.
            log::info!("Edgehop {} was just published; waiting for its files to finish uploading", rel.version);
            set_status_unless_busy(app, known_or(app, Status::Idle));
            false
        }
        Ok(rel) if is_newer(&rel.version, &cur) => {
            log::info!(
                "Edgehop {} is available (this is {cur}); {}",
                rel.version,
                match (&rel.asset, &rel.manual) {
                    (Some(a), _) => format!("one click installs {}", a.name),
                    (None, Some(why)) => format!("download it from the release page ({why})"),
                    (None, None) => "download it from the release page".into(),
                }
            );
            let first = {
                let mut u = lk(&app.update);
                if matches!(u.status, Status::Working(_)) {
                    return true; // installing already
                }
                let first = u.notified.as_deref() != Some(rel.version.as_str());
                u.notified = Some(rel.version.clone());
                u.release = Some(rel.clone());
                u.status = Status::Available;
                first
            };
            app.repaint();
            crate::tray::refresh();
            if first && !by_hand {
                notify(
                    &format!("Edgehop {} is available", rel.version),
                    if rel.asset.is_some() {
                        "Open Edgehop and click Update now."
                    } else {
                        "Open Edgehop for the download page."
                    },
                );
            }
            true
        }
        Ok(rel) => {
            if by_hand {
                log::info!("Edgehop {cur} is up to date (newest release {})", rel.version);
            } else {
                log::debug!("Edgehop {cur} is up to date (newest release {})", rel.version);
            }
            if set_status_unless_busy(app, Status::UpToDate(rel.version)) {
                lk(&app.update).release = None;
                crate::tray::refresh();
            }
            true
        }
        Err(e) => {
            log::warn!("update check failed: {e}");
            // A failed background check isn't worth a red message.
            let failed = if by_hand { Status::Failed(format!("Couldn't check for updates: {e}")) } else { Status::Idle };
            set_status_unless_busy(app, known_or(app, failed));
            false
        }
    }
}

/// Check soon after start, then every few hours, while switched on in
/// Settings. Time is measured on the wall clock, so hours a laptop spends
/// asleep count; after a failed check (no network yet), it tries again sooner.
pub fn start_checker(app: Arc<AppState>) {
    let _ = std::thread::Builder::new().name("updates".into()).spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        sweep_old_downloads();
        let mut last: Option<std::time::SystemTime> = None;
        let mut ok = true;
        loop {
            let wait = if ok { EVERY } else { RETRY };
            // A clock that went backwards counts as due.
            let due = last.map_or(true, |t| t.elapsed().map_or(true, |e| e >= wait));
            if due && app.cfg().check_updates {
                ok = crate::recovery::guard("update check", || check(&app, false)).unwrap_or(false);
                last = Some(std::time::SystemTime::now());
            }
            std::thread::sleep(TICK);
        }
    });
}

pub fn check_now(app: &Arc<AppState>) {
    let app = app.clone();
    std::thread::spawn(move || {
        check(&app, true);
    });
}

// ---------------------------------------------------------------------------
// Installing
// ---------------------------------------------------------------------------

/// What to do once the new version is in place.
#[derive(Debug)]
pub enum Done {
    /// Windows: the installer is running; it stops this copy when it starts
    /// installing, and starts the new one when it's done.
    Installer(std::process::Child),
    /// A helper takes over once this copy has quit: quit now.
    Quit,
    /// Installed: restart to run it.
    Restart,
}

/// Download and install `rel`, reporting progress through `say`.
/// `autostart`: the current "start at sign-in" setting (the Windows installer
/// would otherwise set it back to what was chosen at the first install).
pub fn install_release(rel: &Release, autostart: bool, say: &dyn Fn(String)) -> Result<Done, String> {
    let asset = rel.asset.as_ref().ok_or_else(|| {
        rel.manual.clone().unwrap_or_else(|| "there's no download for this computer in that release".into())
    })?;
    let kind = install_kind();
    if let Kind::MacApp(bundle) = &kind {
        mac_can_replace(bundle)?; // before downloading anything
    }
    let dir = private_dir(&rel.version)?;
    say(format!("Downloading Edgehop {}…", rel.version));
    let file = match download(asset, &dir) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    say(format!("Installing Edgehop {}…", rel.version));
    let result = match kind {
        Kind::WindowsInstaller => {
            let tasks = installer_tasks(autostart);
            log::info!("running the installer silently ({tasks})");
            let child = Command::new(&file)
                .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", &tasks])
                .spawn()
                .map_err(|e| format!("couldn't start the installer: {e}"))?;
            log::info!("the installer is running; it stops this copy and starts {} in the tray", rel.version);
            return Ok(Done::Installer(child));
        }
        Kind::MacApp(bundle) => return install_mac(&file, &dir, &bundle),
        Kind::Deb if on_path("apt-get") => {
            // apt-get also installs anything a new version newly depends on,
            // and waits for another package manager (automatic updates) to finish.
            let args = ["install", "-y", "--allow-downgrades", "-o", "DPkg::Lock::Timeout=180"];
            as_admin("apt-get", &args, &file)
        }
        Kind::Deb => as_admin("dpkg", &["-i"], &file),
        Kind::Rpm if on_path("dnf") => as_admin("dnf", &["install", "-y"], &file),
        Kind::Rpm => as_admin("rpm", &["-U", "--replacepkgs"], &file),
        Kind::Manual(why) => Err(why),
    };
    // On failure the package stays (for a day): the message may name it for
    // installing by hand.
    if result.is_ok() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result.map(|()| Done::Restart)
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// Windows installer tasks for an update: as things are now, not as they
/// were chosen at the first install.
#[cfg_attr(not(windows), allow(dead_code))]
fn installer_tasks(autostart: bool) -> String {
    let desktop_icon = directories::UserDirs::new()
        .and_then(|u| u.desktop_dir().map(|d| d.join("Edgehop.lnk").exists()))
        .unwrap_or(true);
    // The firewall rule names the program's path. The installed copy has one;
    // a copy run from elsewhere (Downloads, say) moves to the installed path
    // and needs it (that asks for admin once).
    let installed = directories::BaseDirs::new().map(|b| b.data_local_dir().join("Programs\\Edgehop\\edgehop.exe"));
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let is_installed = match (own_exe(), installed) {
        (Some(me), Some(inst)) => canon(&me) == canon(&inst),
        _ => true,
    };
    let task = |on: bool, name: &str| if on { name.to_string() } else { format!("!{name}") };
    format!(
        "/MERGETASKS={},{},{}",
        task(autostart, "autostart"),
        task(desktop_icon, "desktopicon"),
        task(!is_installed, "firewall")
    )
}

/// Linux: run a package tool as root: pkexec in a desktop session (it asks for
/// your password), else sudo if it needs no password.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn as_admin(tool: &str, args: &[&str], file: &Path) -> Result<(), String> {
    let desktop = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    let sudo_ok = Command::new("sudo").args(["-n", "true"]).output().is_ok_and(|o| o.status.success());
    let mut cmd = if sudo_ok {
        let mut c = Command::new("sudo");
        c.args(["-n", tool]);
        c
    } else if desktop {
        let mut c = Command::new("pkexec");
        c.arg(tool);
        c
    } else {
        return Err(format!(
            "installing needs administrator rights: run sudo {tool} {} {}",
            args.join(" "),
            file.display()
        ));
    };
    let out = cmd.args(args).arg(file).output().map_err(|e| format!("couldn't run {tool}: {e}"))?;
    if out.status.success() {
        log::info!("{tool} installed {}", file.display());
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("no details").to_string();
    log::warn!("{tool} failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    match out.status.code() {
        Some(126) => Err("the password prompt was cancelled; nothing was installed".into()),
        Some(127) if !sudo_ok => Err(format!(
            "couldn't get administrator rights (not allowed, or no password prompt on this desktop): {detail}"
        )),
        _ => Err(format!("{tool} failed: {detail}")),
    }
}

/// Mac: unpack the new Edgehop.app; once this copy has quit, a helper swaps
/// it in for the running one and opens it. The app's signature names its
/// bundle identifier, so macOS keeps its permissions for the new copy.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn install_mac(zip: &Path, dir: &Path, bundle: &Path) -> Result<Done, String> {
    let unpacked = dir.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    let out = Command::new("ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(&unpacked)
        .output()
        .map_err(|e| format!("couldn't run ditto: {e}"))?;
    if !out.status.success() {
        return Err(format!("couldn't unpack {}: {}", zip.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    let new_app = unpacked.join("Edgehop.app");
    if !new_app.join("Contents/MacOS/edgehop").exists() {
        return Err(format!("{} has no Edgehop.app in it", zip.display()));
    }
    mac_can_replace(bundle)?;
    // $1: our process id, $2: the running app, $3: the new one. Waits at most
    // a minute for this copy to quit; puts the old app back if the swap fails.
    const SWAP: &str = r#"echo "$(date): waiting for Edgehop ($1) to quit"
n=0
while kill -0 "$1" 2>/dev/null && [ $n -lt 300 ]; do sleep 0.2; n=$((n+1)); done
if kill -0 "$1" 2>/dev/null; then
    echo "$(date): Edgehop is still running after a minute; not replacing it"
    exit 1
fi
old="$2.old-$$"
if mv "$2" "$old"; then
    if mv "$3" "$2"; then
        rm -rf "$old"
        echo "$(date): replaced $2"
    else
        echo "$(date): couldn't move the new app into place; putting the old one back"
        rm -rf "$2"
        mv "$old" "$2"
    fi
else
    echo "$(date): couldn't move $2 aside"
fi
rm -rf "$4"
open "$2" --args --minimized && echo "$(date): opened $2""#;
    let log = crate::state::log_path().with_file_name("update-helper.log");
    let out = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let mut sh = Command::new("/bin/sh");
    sh.args(["-c", SWAP, "edgehop-update"])
        .arg(std::process::id().to_string())
        .arg(bundle)
        .arg(&new_app)
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        sh.process_group(0); // outlives this copy
    }
    sh.spawn().map_err(|e| format!("couldn't start the update helper: {e}"))?;
    log::info!("swapping {} for the new version once this copy quits (helper log: {})", bundle.display(), log.display());
    Ok(Done::Quit)
}

/// Mac: can this account put a new Edgehop.app where this one is?
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mac_can_replace(bundle: &Path) -> Result<(), String> {
    if bundle.to_string_lossy().contains("/AppTranslocation/") {
        return Err("macOS is running Edgehop from a temporary read-only copy (it was opened straight from \
                    Downloads): move Edgehop.app to your Applications folder, open it from there, then update"
            .into());
    }
    let parent = bundle.parent().ok_or("Edgehop.app has no folder")?;
    if !writable(parent) {
        return Err(format!(
            "this account can't replace {} (move Edgehop.app to your Applications folder, or download the update yourself)",
            bundle.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn writable(_dir: &Path) -> bool {
    true
}

/// Left behind by an update started from the window, for the new version to
/// find: it opens its window too. Started in the tray (as it is after
/// signing in), Edgehop would seem to have vanished halfway through.
fn reopen_marker() -> PathBuf {
    crate::state::log_path().with_file_name("reopen-window-after-update")
}

/// Is the marker there, and recent? Either way it's used up.
fn take_marker(path: &Path, max_age: std::time::Duration) -> bool {
    let fresh = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < max_age);
    std::fs::remove_file(path).is_ok() && fresh
}

/// Starting up: did an update, clicked in the window, just restart Edgehop?
pub fn reopen_window_after_update() -> bool {
    take_marker(&reopen_marker(), std::time::Duration::from_secs(600))
}

/// The Update button: download, install, then quit or restart into the new version.
pub fn install(app: &Arc<AppState>) {
    // Claim it in one step, so a double click starts one update.
    let rel = {
        let mut u = lk(&app.update);
        let Some(rel) = u.release.clone() else { return };
        if matches!(u.status, Status::Working(_)) {
            return;
        }
        if lk(&app.transfer).is_some() {
            u.status = Status::Failed("Files are being copied; update once that's done (Edgehop restarts to update).".into());
            drop(u);
            app.repaint();
            return;
        }
        u.status = Status::Working(format!("Getting Edgehop {} …", rel.version));
        rel
    };
    app.repaint();
    let app = app.clone();
    std::thread::spawn(move || {
        log::info!("updating from {} to {}", current(), rel.version);
        let a = app.clone();
        let say = move |s: String| set_status(&a, Status::Working(s));
        // Written first: an installer may stop this copy the moment it starts.
        if let Err(e) = std::fs::write(reopen_marker(), current()) {
            log::debug!("couldn't note that the window was open: {e}");
        }
        match install_release(&rel, app.cfg().autostart, &say) {
            Ok(Done::Installer(mut child)) => {
                // Stop sharing and save the log: the installer stops this copy
                // when it starts installing. Still here after it finished?
                // Then it didn't get that far: carry on with this version.
                app.stop_engine();
                crate::state::flush_logs();
                match child.wait() {
                    Ok(st) if st.success() => {
                        log::info!("the installer finished; the new version is starting");
                        crate::state::flush_logs();
                        std::process::exit(0);
                    }
                    other => {
                        let why = match other {
                            Ok(st) => format!("the installer stopped ({st})"),
                            Err(e) => format!("couldn't wait for the installer: {e}"),
                        };
                        log::error!("updating to {} failed: {why}; still running {}", rel.version, current());
                        let _ = std::fs::remove_file(reopen_marker());
                        app.start_engine();
                        set_status(&app, Status::Failed(format!("Update failed: {why}. Edgehop {} is still running.", current())));
                    }
                }
            }
            Ok(Done::Quit) => {
                app.stop_engine();
                crate::state::flush_logs();
                std::process::exit(0);
            }
            Ok(Done::Restart) => {
                app.stop_engine();
                crate::recovery::restart_after_update(&rel.version);
            }
            Err(e) => {
                log::error!("updating to {} failed: {e}", rel.version);
                let _ = std::fs::remove_file(reopen_marker());
                set_status(&app, Status::Failed(format!("Update failed: {e}")));
            }
        }
    });
}

/// `edgehop --check-update` / `edgehop --update`, for scripts and tests.
pub fn cli(install_it: bool) -> i32 {
    let cur = current();
    let kind = install_kind();
    println!("running:  {cur} ({kind:?})");
    let rel = match latest() {
        Ok(r) => r,
        Err(e) => {
            println!("check failed: {e}");
            return 1;
        }
    };
    println!("newest:   {} ({})", rel.version, rel.page);
    match (&rel.asset, &rel.manual) {
        (Some(a), _) => println!("download: {} ({} bytes, sha256 {})", a.url, a.size, a.sha256.as_deref().unwrap_or("not published")),
        (None, why) => println!("download: none for this computer ({})", why.as_deref().unwrap_or("?")),
    }
    if !is_newer(&rel.version, &cur) {
        println!("up to date");
        return 0;
    }
    println!("update available");
    if !install_it {
        return 0;
    }
    let autostart = crate::config::Config::load().autostart;
    match install_release(&rel, autostart, &|s| println!("{s}")) {
        Ok(Done::Installer(_)) | Ok(Done::Quit) => {
            println!("the installer takes it from here");
            0
        }
        Ok(Done::Restart) => {
            println!("installed {}; restart Edgehop to run it", rel.version);
            0
        }
        Err(e) => {
            println!("update failed: {e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// Desktop notifications
// ---------------------------------------------------------------------------

/// A desktop notification (best effort; failures are only logged).
pub fn notify(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Err(e) = notify_now(&title, &body) {
            log::debug!("couldn't show a notification: {e}");
        }
    });
}

#[cfg(target_os = "linux")]
fn notify_now(title: &str, body: &str) -> Result<(), String> {
    use dbus::blocking::Connection;
    let conn = Connection::new_session().map_err(|e| e.to_string())?;
    let proxy = conn.with_proxy("org.freedesktop.Notifications", "/org/freedesktop/Notifications", Duration::from_secs(5));
    let (_id,): (u32,) = proxy
        .method_call(
            "org.freedesktop.Notifications",
            "Notify",
            ("Edgehop", 0u32, "edgehop", title, body, Vec::<String>::new(), dbus::arg::PropMap::new(), -1i32),
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn notify_now(title: &str, body: &str) -> Result<(), String> {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title));
    let st = Command::new("osascript").args(["-e", &script]).status().map_err(|e| e.to_string())?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("osascript exited with {st}"))
    }
}

/// Windows: a notification from a temporary notification-area icon (shown
/// as a toast on Windows 10 and 11).
#[cfg(windows)]
fn notify_now(title: &str, body: &str) -> Result<(), String> {
    use crate::win32::{GetModuleHandleW, HANDLE};
    use std::ffi::c_void;
    #[repr(C)]
    struct Guid {
        a: u32,
        b: u16,
        c: u16,
        d: [u8; 8],
    }
    #[repr(C)]
    struct NotifyIconData {
        cb_size: u32,
        hwnd: HANDLE,
        id: u32,
        flags: u32,
        callback_message: u32,
        icon: HANDLE,
        tip: [u16; 128],
        state: u32,
        state_mask: u32,
        info: [u16; 256],
        version: u32,
        info_title: [u16; 64],
        info_flags: u32,
        guid: Guid,
        balloon_icon: HANDLE,
    }
    #[link(name = "shell32")]
    extern "system" {
        fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconData) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: HANDLE,
            menu: HANDLE,
            inst: HANDLE,
            param: *mut c_void,
        ) -> HANDLE;
        fn DestroyWindow(hwnd: HANDLE) -> i32;
        fn LoadIconW(inst: HANDLE, name: *const u16) -> HANDLE;
    }
    fn wide<const N: usize>(s: &str) -> [u16; N] {
        let mut out = [0u16; N];
        for (i, c) in s.encode_utf16().take(N - 1).enumerate() {
            out[i] = c;
        }
        out
    }
    const NIM_ADD: u32 = 0;
    const NIM_DELETE: u32 = 2;
    const NIF_ICON: u32 = 2;
    const NIF_TIP: u32 = 4;
    const NIF_INFO: u32 = 0x10;
    const NIIF_INFO: u32 = 1;
    unsafe {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let null = std::ptr::null_mut();
        let hwnd = CreateWindowExW(0x80, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, null, null, null, null);
        if hwnd.is_null() {
            return Err("couldn't create a window for the notification".into());
        }
        let inst = GetModuleHandleW(std::ptr::null());
        // The exe's own icon (resource 1), else the generic one.
        let mut icon = LoadIconW(inst, 1 as *const u16);
        if icon.is_null() {
            icon = LoadIconW(null, 32512 as *const u16);
        }
        let mut d = NotifyIconData {
            cb_size: std::mem::size_of::<NotifyIconData>() as u32,
            hwnd,
            id: 0xED6E,
            flags: NIF_ICON | NIF_TIP | NIF_INFO,
            callback_message: 0,
            icon,
            tip: wide("Edgehop"),
            state: 0,
            state_mask: 0,
            info: wide(body),
            version: 0,
            info_title: wide(title),
            info_flags: NIIF_INFO,
            guid: Guid { a: 0, b: 0, c: 0, d: [0; 8] },
            balloon_icon: null,
        };
        let ok = Shell_NotifyIconW(NIM_ADD, &mut d) != 0;
        if ok {
            std::thread::sleep(Duration::from_secs(15));
            Shell_NotifyIconW(NIM_DELETE, &mut d);
        }
        DestroyWindow(hwnd);
        if ok {
            Ok(())
        } else {
            Err("Shell_NotifyIcon refused the notification".into())
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn notify_now(_title: &str, _body: &str) -> Result<(), String> {
    Err("no notifications on this system".into())
}

/// Open a web page in the default browser.
pub fn open_page(url: &str) {
    #[cfg(windows)]
    let r = command("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let r = Command::new("xdg-open").arg(url).spawn();
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    let r: std::io::Result<std::process::Child> = Err(std::io::ErrorKind::Unsupported.into());
    if let Err(e) = r {
        log::warn!("couldn't open {url}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert!(is_newer("v0.5.1", "0.5.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.0"));
        assert!(!is_newer("v0.5.0", "0.5.0"));
        assert!(!is_newer("0.4.9", "0.5.0"));
        assert!(!is_newer("garbage", "0.5.0"));
        assert!(!is_newer("0.5.1-beta", "0.5.1"));
    }

    #[test]
    fn asset_names_match_the_release_files() {
        assert_eq!(asset_name(&Kind::WindowsInstaller, "0.5.0", "x86_64").unwrap(), "Edgehop-Setup-0.5.0.exe");
        assert_eq!(asset_name(&Kind::MacApp("/A/Edgehop.app".into()), "0.5.0", "aarch64").unwrap(), "Edgehop-v0.5.0-macos-universal.zip");
        assert_eq!(asset_name(&Kind::Deb, "0.5.0", "x86_64").unwrap(), "edgehop_0.5.0_amd64.deb");
        assert_eq!(asset_name(&Kind::Deb, "0.5.0", "aarch64").unwrap(), "edgehop_0.5.0_arm64.deb");
        assert_eq!(asset_name(&Kind::Rpm, "0.5.0", "aarch64").unwrap(), "edgehop-0.5.0-1.aarch64.rpm");
        assert_eq!(asset_name(&Kind::Manual("x".into()), "0.5.0", "x86_64"), None);
    }

    #[test]
    fn github_answer_is_read() {
        let json = br#"{"tag_name":"v0.6.0","html_url":"https://github.com/JawshTheDark/edgehop/releases/tag/v0.6.0",
            "assets":[{"name":"edgehop_0.6.0_amd64.deb","size":1234,
              "browser_download_url":"https://github.com/JawshTheDark/edgehop/releases/download/v0.6.0/edgehop_0.6.0_amd64.deb",
              "digest":"sha256:abc123"},
             {"name":"Edgehop-Setup-0.6.0.exe","size":99,"browser_download_url":"https://example/x.exe","digest":null}]}"#;
        let r = parse_release(json, &Kind::Deb, "x86_64").unwrap();
        assert_eq!(r.version, "0.6.0");
        let a = r.asset.unwrap();
        assert_eq!((a.size, a.sha256.as_deref()), (1234, Some("abc123")));
        let r = parse_release(json, &Kind::WindowsInstaller, "x86_64").unwrap();
        assert_eq!(r.asset.unwrap().sha256, None);
        let r = parse_release(json, &Kind::Rpm, "x86_64").unwrap();
        assert!(r.asset.is_none());
        assert_eq!(r.manual.as_deref(), Some("this release has no edgehop-0.6.0-1.x86_64.rpm"));
    }

    #[cfg(windows)]
    #[test]
    fn installer_tasks_follow_the_settings_now() {
        let on = installer_tasks(true);
        // (and, while here: the note an update leaves for the new version)
        let note = std::env::temp_dir().join(format!("edgehop-test-reopen-{}", std::process::id()));
        assert!(!take_marker(&note, std::time::Duration::from_secs(600)), "nothing left: start as asked");
        std::fs::write(&note, "x").unwrap();
        assert!(take_marker(&note, std::time::Duration::from_secs(600)), "just updated from the window: open it");
        assert!(!note.exists(), "used up, so the next start is in the tray again");
        std::fs::write(&note, "x").unwrap();
        assert!(!take_marker(&note, std::time::Duration::ZERO), "an old one doesn't count");
        assert!(!note.exists());
        assert!(on.starts_with("/MERGETASKS=autostart,"), "{on}");
        assert!(installer_tasks(false).starts_with("/MERGETASKS=!autostart,"));
        assert!(on.contains("desktopicon"), "{on}");
        // The test program isn't the installed copy: it would need the firewall rule.
        assert!(on.ends_with(",firewall"), "{on}");
    }

    #[test]
    fn github_times() {
        assert_eq!(unix_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_time("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(unix_time("2026-11-11T04:30:15Z"), Some(1_794_371_415));
        assert_eq!(unix_time("soon"), None);
    }

    #[test]
    fn checksum_of_a_file() {
        let p = std::env::temp_dir().join(format!("edgehop-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(sha256_hex(&p).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = std::fs::remove_file(&p);
    }
}
