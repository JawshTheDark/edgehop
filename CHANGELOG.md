# Changelog

Each release's section below becomes its notes on the
[Releases page](https://github.com/JawshTheDark/edgehop/releases).

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
