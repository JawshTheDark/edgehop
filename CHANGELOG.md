# Changelog

Each release's section below becomes its notes on the
[Releases page](https://github.com/JawshTheDark/edgehop/releases).

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
