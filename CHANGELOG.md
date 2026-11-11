# Changelog

Each release's section below becomes its notes on the
[Releases page](https://github.com/JawshTheDark/edgehop/releases).

## [0.5.2] - 2026-11-10

Edge cases in updating, restarting and the login-screen service.

### Updating
- **Windows: updates keep your current settings.** The installer used to
  replay the choices from the first install, so an update turned *Start
  Edgehop when I log in* back on (or off) whatever Settings said, and put back
  a desktop icon you'd deleted. A copy run from outside the install folder
  (Downloads, say) gets its firewall rule when an update moves it there.
- **Windows: a failed installer leaves Edgehop running.** Edgehop now waits
  for the installer; if it stops before installing, Edgehop carries on with
  the current version and says so (it used to quit, leaving nothing running).
- **Linux: downloads go to a new private folder.** They used a fixed name in
  the shared /tmp, where another account could have made that folder first
  and swapped the package between the checksum test and its install as root.
- Linux installs with apt-get where there is one (it waits for automatic
  updates holding the package lock, and installs anything a new version
  newly needs) and dnf on Fedora. rpm-ostree systems (Silverblue, Kinoite)
  get the download page.
- A double click on *Update now*, or a check running at the same moment, no
  longer starts a second update.
- Checks go by the wall clock, so hours a laptop spends asleep count; a failed
  check (no network yet at sign-in) is retried after 15 minutes, not 6 hours.
- A release spotted while its files are still uploading (within 2 hours of
  publishing) is checked again later instead of offering the download page.
- Windows: updating works for user names in other scripts (Windows' curl
  can't open those paths; downloads now come through Edgehop itself).
- Updating waits while files are being copied between computers.
- Mac: if macOS runs Edgehop from a temporary copy (opened straight from
  Downloads), Edgehop says to move it to Applications before downloading
  anything. The helper that swaps the app logs to `update-helper.log`.
- A clearer message when Linux can't ask for your password (no polkit agent).
- The update banner stays up while a background check runs.

### Restarting (Linux)
- Edgehop counted on systemd to restart it inside any systemd unit, even one
  where it wasn't the main program (started from a terminal inside some
  other service, say), and then simply quit. It now checks it's the unit's
  main program; otherwise it starts the new copy itself, as a unit of its own
  with this session's display and settings, falling back to a plain start.

### Login-screen service
- Before this computer is paired, it waits for the pairing instead of failing
  and being restarted by systemd every 3 seconds.
- It rereads your settings each time it takes over, so a new pairing or
  screen size is picked up.

### Keyboard (Linux)
- GNOME's layout options only count on GNOME-family desktops (on Sway or
  Hyprland they'd be leftovers), and keyboard settings are looked at again
  each time Edgehop connects.

## [0.5.1] - 2026-11-10

### Updates
- **Edgehop tells you when there's a new version**: it checks GitHub soon
  after it starts and every 6 hours, and shows a desktop notification (once per
  version), a banner at the top of its window, and (on Linux) an *Update to
  Edgehop x.y.z* entry in the tray menu.
- **Update now** downloads the new version, checks it against the SHA-256 that
  GitHub publishes for the file, installs it and restarts Edgehop:
  - Windows runs the installer silently (your choices from the first install
    are kept; no admin prompt) and starts Edgehop again in the tray.
  - macOS swaps Edgehop.app for the new one and reopens it; its permissions
    carry over.
  - Linux installs the new .deb or .rpm (asks for your password) and restarts.
    The login-screen service restarts on the new version too. Installs from
    the tarball get a link to the download page.
- Settings → Updates: switch the check off, *Check now*, and a link to all
  releases. The tray menu has *Check for updates* too. Downloads use the
  system's own curl (or wget), so its certificates and proxy settings apply.
- From a terminal: `edgehop --check-update`, `edgehop --update`.

### Fixes
- Linux: *Restart Edgehop* (and restarts after a problem) quit instead of
  restarting when Edgehop was started by the desktop's autostart under systemd
  (as KDE Plasma 6 does). It now starts the new copy as a systemd unit of its own.

## [0.5.0] - 2026-11-10

### Linux: type into the login screen
- **Settings → Login screen → *Also work at the login screen***: a small
  system service (`edgehop-login`) starts at boot, so the sharing computer's
  keyboard and mouse work at the login screen, before anyone signs in (no more
  on-screen keyboard). It borrows your Edgehop settings read-only (same
  pairing, same name), steps aside as soon as your own Edgehop starts, and
  takes over again when you sign out. Switching it asks for your password; from
  a terminal: `sudo systemctl enable --now edgehop-login`.

### Keyboard
- **Linux: Ctrl+C copies even where Super works as Ctrl.** People coming from
  a Mac often set Linux up so the Command (Super) key works as Ctrl. That
  setting applies to Edgehop's virtual keyboard too, so Ctrl from the other
  computer arrived as Super. Edgehop now reads the layout options in KDE's and
  GNOME's keyboard settings (and keyd's setup) and sends whichever key this
  computer turns into the one you pressed. Settings → Keyboard → *Swap Ctrl and
  Super* sets it by hand; the log says what was found (`keyboard: ...`).

### Mac
- **Drag files from Finder onto another computer.** At the edge, Edgehop reads
  what the drag carries from the drag pasteboard, cancels the drag on the Mac
  (so nothing is dropped at the edge, and macOS doesn't switch Spaces), and the
  drag carries on over there.
- **Files dragged onto the Mac** land in the Finder window under the pointer, or
  on the Desktop (macOS asks once whether Edgehop may ask Finder which folder
  that is); dropped anywhere else, they're on the clipboard to paste.
- **Copy and paste files and folders** between a Mac and other computers, with
  Finder's Copy and Paste.
- **Trackpad gestures stay off the Mac while you're on another computer**: a
  three-finger swipe no longer opens Mission Control or switches Spaces behind
  your back (which also left a second pointer on the Mac). Gestures don't
  carry across to the other computer yet; two-finger scrolling does.

### Connections
- **Two copies of Edgehop on one computer** no longer knock each other off every
  few seconds ("Not connected" / "Connected" flicker). The older copy is told it
  was replaced, says so, and waits; the sharing computer logs which computer
  runs two copies.
- Clients log every 5 minutes that they're still connected and when they last
  heard from the sharing computer, to pin down silent network drops.
- Linux: *Override screen size* now also places the pointer correctly (it only
  scaled the movement before).

## [0.4.0] - 2026-11-10

### Game mode
- **The mouse stays on the sharing computer**: edges don't lead anywhere, so
  whipping the mouse around in a game can't land you on another screen.
- **On Windows, Edgehop's mouse hook steps aside completely** while game mode
  is on: games get raw, untouched mouse input with nothing in between (no added
  latency, nothing for anti-cheat to notice). The keyboard hook stays, for the
  toggle key.
- **Turns on by itself** while a fullscreen game, video or presentation is in
  front (Windows' own fullscreen detection, as Focus Assist uses), and off
  again when it closes. It never pulls the mouse back from another computer.
  Settings → Game mode → *Turn on for fullscreen games* switches this off.
- **Scroll Lock** on the sharing computer turns it on and off (F14 on a Mac
  keyboard). Pressed during a fullscreen game, it stays off until that game
  closes. Scroll Lock on another computer still brings the mouse home.
- Also in the tray menu and in Settings; a *Game mode* pill in the header
  says when it's on and why, and every change is logged with the reason.

## [0.3.0] - 2026-11-10

### Windows installer
- **`Edgehop-Setup-<version>.exe`**: installs for you without admin rights,
  with Start menu and desktop icons, an entry in Apps & Features to uninstall
  it, and an option to add the Windows Firewall rule (that step asks for admin
  once). Running a newer one updates Edgehop in place.
- **Starts at sign-in, in the tray.** Windows was seen skipping its "Run" list
  at sign-in, so Edgehop now starts through Task Scheduler instead (the switch in
  Settings uses it too, and moves you over from the old way).
- `edgehop.exe` carries its icon, so Explorer, the taskbar and shortcuts show
  Edgehop's icon instead of a blank program.

### Tray and windows
- Starting in the tray works: the Windows tray icon is created right away, not
  when the window first draws (a window that starts hidden may never draw).
- **Linux: closing the window keeps Edgehop in the tray** (it used to stay
  minimized on the taskbar); the tray icon opens a fresh window. Started at
  login it starts in the tray. If the desktop has no tray, closing quits.
- **Linux: the window has Edgehop's icon** (it showed a generic "W" on Wayland).
- One Edgehop at a time: opening it again brings up the one that's running.

### Fixes
- Edgehop could stop logging for good when started by Task Scheduler or
  another program without a console: writing a log line to the (missing)
  console made the log writer give up. It now only echoes to a real terminal or
  the systemd journal, and says so if it can't open the log file.
- `edgehop --paths` prints where settings and the log live.

## [0.2.7] - 2026-11-10

### Sharing from a Mac
- Fixes the pointer "swooping" and most movement being lost while on the other
  computer (seen on macOS 27). Edgehop kept moving the Mac's hidden pointer
  back to the middle of the screen, macOS didn't keep it there, and every
  report then looked like a huge jump and was thrown away. Movement now comes
  from macOS's own per-event movement, and the hidden pointer is only put back
  in the middle when it nears a screen edge. Edgehop checks that each move
  stuck; if macOS keeps ignoring them, it stops trying and says so.

### Errors in the log
- Every macOS pointer call is checked: a refused or ignored move or hide is
  logged as an error with macOS's error code and where the pointer really is.
- On any computer, 20 or more nonsense pointer reports within 2 seconds is
  logged as one error explaining what's happening, with the input flight
  recorder, instead of a page of "ignoring stale mouse position" lines.

## [0.2.6] - 2026-11-10

### Windows
- The input thread now runs at the highest priority, so it answers Windows
  in time even when the computer is busy. Windows removes an input hook that
  answers too slowly, letting keys through to this computer as well.
- When Windows does drop the hooks, the log now says which one went quiet
  (mouse or keyboard), how long ago each last ran, and which window was in
  front and whether it runs as administrator (a normal app's hooks don't see
  input meant for those). The input flight recorder is written to the log
  too.

## [0.2.5] - 2026-11-10

### Fixes
- **Windows key (and other keys) reaching both computers.** With detailed
  logging on, every key and mouse event was written to disk and to the
  Activity tab from inside Windows' input hook. When that stalled for a moment
  (antivirus scanning the log, the log file rotating, the Activity tab
  drawing), Windows stopped waiting and delivered the key to the sharing
  computer too. Logging now happens on its own thread; the hooks only hand
  lines over. Hooks that ever take 50 ms or more are logged as a warning and
  kept in the flight recorder, so this can't go unnoticed again.

## [0.2.4] - 2026-11-10

### Stronger logs
- **Input flight recorder**: every computer keeps its last 2000 raw input
  events and switching decisions in memory: exactly what the OS reported
  (positions, macOS's own deltas, Linux device events, Windows hook messages),
  whether Edgehop kept or passed each one, the virtual pointer while away, and
  every switch. **Save diagnostics** now includes it from every computer.
- **Bounce detector**: if the pointer switches screens 6 times within 3 seconds,
  Edgehop logs a warning with the full screen layout and writes the flight
  recorder into the log, no debug mode needed.
- Every switch says why: which edge, where on the desktop, and, coming back,
  how hard you pushed and the last movement.
- The log starts with the system (OS version; on Linux the distribution,
  kernel, session type and desktop; on macOS whether Accessibility is allowed)
  and every monitor.

## [0.2.3] - 2026-11-10

### Fixes
- **Sharing from a Mac**: the pointer could get flung back and forth between
  the Mac and the other computer, sometimes many times a second. macOS counts
  Edgehop's own re-centring of the hidden pointer as mouse movement; Edgehop
  now measures movement from where it parked the pointer instead, as it does
  on Windows.
- Coming back to the sharing computer, the edge you came through stays shut
  until the pointer moves off it (or for half a second), so a wobble as you
  lift your finger from a trackpad doesn't send you straight back.

### More Linux downloads
- **Fedora**: an `.rpm` (`sudo dnf install ./edgehop-*.rpm`). CI installs it in
  a Fedora container and runs it before every release.
- **ARM64 Linux** (e.g. Asahi Linux on Apple-silicon Macs, Raspberry Pi 4/5):
  `.deb`, `.rpm` and a tarball, built and tested on ARM.
- The tarball now has an `install.sh` that sets everything up on any
  distribution.

## [0.2.2] - 2026-11-10

### Mac
- **Reset permissions** button: when macOS shows Edgehop as allowed but still
  refuses it (a permission left over from an older copy), one click clears
  Edgehop's Accessibility, Input Monitoring and input-posting entries and
  reopens it, so macOS asks again. Newer macOS has no way to remove an app from
  those lists by hand. Also available as `edgehop --reset-permissions`.
- Error messages no longer tell you to remove Edgehop with − (newer macOS
  doesn't offer that); they point at the button instead.

## [0.2.1] - 2026-11-10

### Mac
- macOS permissions now carry over to future updates. Until now each update
  looked like a new app to macOS, which could keep showing Edgehop as allowed
  while refusing it. **Coming from 0.2.0 or earlier**: once, select Edgehop
  under Privacy & Security → Accessibility (*Device Control and Data Access* on
  newer macOS), remove it with −, add it back with +, and restart Edgehop.
- When macOS refuses to let Edgehop read the keyboard and mouse, Edgehop now
  checks Accessibility and Input Monitoring separately, asks for the missing
  one, and says exactly what to do.

## [0.2.0] - 2026-11-10

### Share from a Mac or Linux computer
Until now only Windows could share its keyboard and mouse. Now any of them can:
choose **Share this keyboard & mouse** under Settings → Role.

- **macOS**: a Quartz event tap reads the keyboard and mouse (allow Edgehop under
  Accessibility). *Command works as Ctrl* (on by default) makes Command+C and
  Command+V copy and paste on Windows and Linux. F14 brings the mouse home, as
  Mac keyboards have no Scroll Lock.
- **Linux**: works on X11 and on KDE Plasma (Wayland), reading keyboards, mice
  and touchpads straight from the kernel and taking them over only while the
  mouse is on another computer. The package now includes a udev rule that lets
  the logged-in user read them. GNOME on Wayland isn't supported yet (use
  *GNOME on Xorg*).
- Mac clients hide the pointer while the mouse is on another computer.

### Fixes
- The Mac's Accessibility message had a run of stray spaces in the middle.

## [0.1.1] - 2026-11-10

Mac fixes, found by testing on a real Mac in CI.

### Mac
- Asks for Accessibility permission on first launch (macOS's own prompt), and
  the Connection tab shows a button to the right settings page until it's allowed.
- **Ctrl works as Command**: Ctrl+C copies and Ctrl+V pastes on the Mac, as on
  Windows; the Windows key becomes Control. On by default; switch it off under
  Settings → Keyboard.
- Explains why it asks to use the local network (macOS 15 and later ask).

### Everything else
- `edgehop --self-test` checks a computer's clipboard and input without a server;
  `--type` also types a sentence and copies it back.
- Every change is now tested on a real Mac: the app is launched, types into
  TextEdit and copies the text back, and screenshots are kept with each CI run.

## [0.1.0] - 2026-11-10

The first release.

### Keyboard and mouse
- A Windows PC shares its keyboard and mouse with Linux, Windows and macOS computers.
- Monitor-level layout: drag each computer against any edge of any monitor; the
  pointer keeps its position across edges, corner to corner.
- Edge resistance so clicks near a shared edge don't jump screens by accident.
- The pointer hides on the screen it left; on KDE Plasma the client screen dims
  while you're away (and stays bright while media is playing).
- Linux input through `uinput`: works on X11, Wayland, the lock screen and
  password prompts.

### Clipboard and files
- Shared clipboard for text and images, including screenshots from the snipping tool.
- Copy files and folders on one computer and paste them on another; large
  transfers use several parallel connections (about 935 Mbit/s on gigabit Ethernet).
- Drag files off the edge of the screen onto the other computer, in both directions.

### Security and setup
- Encrypted with the Noise protocol (X25519, ChaCha20-Poly1305); new computers
  are approved by matching a 6-digit pairing code.
- Clients find the server on the LAN automatically.
- Starts with your computer, from the tray.

### Reliability
- Self-healing: watchdogs recover from hung input hooks, dropped connections,
  stuck clipboards and crashes.
- Debug logging that can be switched on for every computer at once, and a
  one-click diagnostics report.
