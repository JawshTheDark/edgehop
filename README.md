# kvmlink

Share one keyboard and mouse across every computer on your network — push the
pointer off the edge of your Windows screen and it appears on your Ubuntu
Surface (or a laptop, or a Mac). Same idea as Synergy / Barrier / Input Leap.

- **GUI + tray app**: drag screens into place, approve new computers, see who's connected.
- **Encrypted & paired**: traffic uses the Noise protocol (X25519 + ChaCha20-Poly1305).
  A new computer must be approved on the server, and both sides show the same 6-digit code.
- **Auto-discovery**: clients find the server on the LAN by themselves; no IP typing.
- **Shared clipboard**: copy on one machine, paste on another (text).
- **Clients**: Ubuntu/Linux (X11 *and* Wayland via `uinput`), Windows, macOS.
- **Server**: Windows (captures input with low-level hooks).

## Install

### Windows (the computer with the keyboard and mouse)

Needs Rust (`rustup`) and the Visual Studio C++ Build Tools.

```powershell
cargo build --release
powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1
```

The installer copies the app to `%LOCALAPPDATA%\Programs\kvmlink`, adds a Start
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
**Log out and back in once** afterwards, then open *kvmlink* from the app menu.

### macOS

```bash
./packaging/macos/build-app.sh
```

Move `dist/kvmlink.app` to Applications, open it, and allow it under
System Settings → Privacy & Security → **Accessibility**.

## Using it

1. On Windows, open kvmlink. It starts in **server** mode.
2. On the other computer, open kvmlink. It starts in **client** mode and finds the server.
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
| Keep running | Close the window → it stays in the tray. Tick *Start kvmlink when I log in*. |

Settings live in `%APPDATA%\kvmlink\config\config.json` (Windows),
`~/.config/kvmlink/config.json` (Linux), or
`~/Library/Application Support/kvmlink/config.json` (macOS).

## Troubleshooting

- **Client says "Searching…" forever**: UDP broadcast may be blocked (guest Wi-Fi,
  VPN, or Windows firewall on a *Public* network). Type the server's IP into
  *Address* on the client instead.
- **Linux: "Can't open /dev/uinput"**: log out and in after installing; or run
  `sudo modprobe uinput && sudo setfacl -m u:$USER:rw /dev/uinput` for a quick test.
- **Clipboard on Ubuntu Wayland** needs `wl-clipboard` (installed by the build script).
- **Admin windows on Windows**: if the mouse won't leave while an elevated window
  (Task Manager, an admin terminal) is focused, run kvmlink as administrator.
- Ports: TCP **24800** (connections) and UDP **24801** (discovery).

## Self-healing

kvmlink recovers from failures without you having to step in:

| Problem | What kvmlink does |
|---|---|
| The computer your mouse is on stops answering | Mouse returns to the server after 3 s; it won't cross into a silent computer until it answers again |
| Connection drops | Both sides reconnect automatically (dead connections are detected within 12 s) |
| Something inside kvmlink gets stuck | The Windows input hook passes input straight through after 100 ms (it can never freeze your mouse); a stuck switching state for 5 s restarts kvmlink |
| Windows silently removes the input hooks | Noticed (Windows saw input the hooks didn't) and reinstalled |
| The client's connection loop stalls | Stuck helper processes are killed and it reconnects; if still stuck, kvmlink restarts |
| Clipboard tools hang (e.g. on a locked screen) | Given up on after 2 s, off the input path |
| A connection ends with keys or buttons held | They're released |
| A background worker crashes | Logged with its location; the worker keeps running |
| KDE's D-Bus connection drops | Reconnected |
| kvmlink crashes or is killed | Cursor is restored; on Linux, systemd restarts it (autostart runs it with `Restart=on-failure`) |

You can also restart it from the tray icon (**Restart kvmlink**). Everything is
logged to `kvmlink.log` (`%LOCALAPPDATA%\kvmlink\cache\` on Windows,
`~/.cache/kvmlink/` on Linux).

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
