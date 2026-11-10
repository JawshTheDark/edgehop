<p align="center">
  <img src="docs/icon.png" width="96" alt="Edgehop icon">
</p>

<h1 align="center">Edgehop</h1>

<p align="center"><b>One keyboard and mouse for every computer on your desk.</b><br>
Push the pointer off the edge of a screen and it hops onto the next computer.</p>

<p align="center">
  <img src="docs/screens.png" width="720" alt="Edgehop screen layout">
</p>

Edgehop is a software KVM in the spirit of Synergy, Barrier and Input Leap:
a Windows PC shares its keyboard and mouse with Linux, Windows and macOS
computers over your local network.

- **Monitor-level layout**: place each computer against any edge of any monitor,
  like Windows' Display settings. Positions line up across edges, corner to corner.
- **Clipboard sharing**: text *and images* (snip on one computer, paste on another).
- **Drag files across screens**, in both directions.
- **Encrypted and paired**: Noise protocol (X25519 + ChaCha20-Poly1305). A new
  computer must be approved, and both sides show the same 6-digit code.
- **Finds itself**: clients discover the server on the LAN; no IP typing.
- **Edge resistance**, a hidden cursor while you're away, and on KDE Plasma a
  dimmed screen that stays bright while media plays.
- **Self-healing**: recovers from hangs, dropped connections and crashes on its own.
- **Clients**: Linux (X11 *and* Wayland via `uinput`), Windows, macOS.
  **Server**: Windows.

<p align="center">
  <img src="docs/settings.png" width="640" alt="Edgehop settings">
</p>

## Install

### Windows (the computer with the keyboard and mouse)

Needs Rust (`rustup`) and the Visual Studio C++ Build Tools.

```powershell
cargo build --release
powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1
```

The installer copies the app to `%LOCALAPPDATA%\Programs\Edgehop`, adds a Start
Menu entry, and asks for admin once to allow it through the firewall on
**Private** networks. Make sure your Wi-Fi/Ethernet is set to *Private* in
Windows settings.

### Ubuntu (e.g. Surface running Linux)

Copy this folder over (or `git clone` it), then:

```bash
./packaging/linux/build-deb.sh
```

This installs build tools and Rust if needed, builds a `.deb`, and installs it.
The package adds a udev rule so your user can create virtual input devices.
**Log out and back in once** afterwards, then open *Edgehop* from the app menu.

### macOS

```bash
./packaging/macos/build-app.sh
```

Move `dist/edgehop.app` to Applications, open it, and allow it under
System Settings → Privacy & Security → **Accessibility**.

## Using it

1. On Windows, open Edgehop. It starts in **server** mode.
2. On the other computer, open Edgehop. It starts in **client** mode and finds the server.
3. A pairing prompt pops up on Windows. Check that the code matches the one on
   the client, then click **Approve**.
4. The new computer shows up to the right of your PC in **Screen layout**. Drag
   it to wherever it physically sits.
5. Move the mouse off that edge. You're now typing on the other computer.

| | |
|---|---|
| Come back | Move the mouse off the opposite edge, or press **Scroll Lock** |
| Clipboard | Copy, move to the other screen, paste |
| Mouse too fast/slow on the client | *Mouse speed on other screens* (server) or *Override screen size* (client) |
| Keep running | Close the window → it stays in the tray. Tick *Start Edgehop when I log in*. |

Settings live in `%APPDATA%\edgehop\config\config.json` (Windows),
`~/.config/edgehop/config.json` (Linux), or
`~/Library/Application Support/edgehop/config.json` (macOS).

## Troubleshooting

**Debug logging and diagnostics.** In the **Activity** tab, switch on **Debug logging**
(on the server it applies to every connected computer), reproduce the problem,
then click **Save diagnostics**. That writes one file to your Desktop with the
version, settings (keys shortened to fingerprints), monitors, connections and
recent logs from *every* connected computer: attach it to a bug report.
**Include every input event** adds trace-level logging of each key and mouse
push. From a terminal: `edgehop --debug` / `edgehop --trace`, or set
`EDGEHOP_LOG=debug`. Logs live in `%LOCALAPPDATA%\edgehop\cache\edgehop.log`
(Windows) and `~/.cache/edgehop/edgehop.log` (Linux); the previous file is
kept as `edgehop.1.log`.

To measure network transfer speed between two computers:
`edgehop --bench-serve <big file>` on one, `edgehop --bench-fetch <its IP> 1 4 8` on the other.


- **Client says "Searching…" forever**: UDP broadcast may be blocked (guest Wi-Fi,
  VPN, or Windows firewall on a *Public* network). Type the server's IP into
  *Address* on the client instead.
- **Linux: "Can't open /dev/uinput"**: log out and in after installing; or run
  `sudo modprobe uinput && sudo setfacl -m u:$USER:rw /dev/uinput` for a quick test.
- **Clipboard on Ubuntu Wayland** needs `wl-clipboard` (installed by the build script).
- **Admin windows on Windows**: if the mouse won't leave while an elevated window
  (Task Manager, an admin terminal) is focused, run Edgehop as administrator.
- Ports: TCP **24800** (connections) and UDP **24801** (discovery).

## Self-healing

edgehop recovers from failures without you having to step in:

| Problem | What edgehop does |
|---|---|
| The computer your mouse is on stops answering | Mouse returns to the server after 3 s; it won't cross into a silent computer until it answers again |
| Connection drops | Both sides reconnect automatically (dead connections are detected within 12 s) |
| Something inside edgehop gets stuck | The Windows input hook passes input straight through after 100 ms (it can never freeze your mouse); a stuck switching state for 5 s restarts edgehop |
| Windows silently removes the input hooks | Noticed (Windows saw input the hooks didn't) and reinstalled |
| The client's connection loop stalls | Stuck helper processes are killed and it reconnects; if still stuck, edgehop restarts |
| Clipboard tools hang (e.g. on a locked screen) | Given up on after 2 s, off the input path |
| A connection ends with keys or buttons held | They're released |
| A background worker crashes | Logged with its location; the worker keeps running |
| KDE's D-Bus connection drops | Reconnected |
| edgehop crashes or is killed | Cursor is restored; on Linux, systemd restarts it (autostart runs it with `Restart=on-failure`) |

You can also restart it from the tray icon (**Restart edgehop**). Everything is
logged to `edgehop.log` (`%LOCALAPPDATA%\edgehop\cache\` on Windows,
`~/.cache/edgehop/` on Linux).

## How it works

```
 Windows (server)                                Ubuntu (client)
 ┌─────────────────────────┐   Noise XX / TCP    ┌──────────────────────────┐
 │ low-level kbd/mouse hook│ ──────────────────▶ │ uinput virtual keyboard  │
 │ edge detection + layout │  key/move/button/   │ + absolute pointer       │
 │ clipboard sync          │  wheel/clipboard    │ clipboard (wl-clipboard) │
 └─────────────────────────┘ ◀── clipboard ───── └──────────────────────────┘
```

While a client is active, the server parks its own cursor in the middle of the
screen, turns every motion into a delta, and moves a virtual cursor on the
client screen. The client gets absolute coordinates, so the pointer lands
exactly where expected even under Wayland. Keys travel as Linux evdev codes and
are translated to scancodes on Windows and to virtual key codes on macOS.

Run `cargo test` to exercise the switching logic and key maps.
