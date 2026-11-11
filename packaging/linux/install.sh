#!/usr/bin/env bash
# Install Edgehop from an unpacked release tarball, on any Linux distribution:
#   ./install.sh
# (Ubuntu/Debian and Fedora have .deb and .rpm packages that do this for you.)
set -euo pipefail
cd "$(dirname "$0")"

sudo install -Dm755 edgehop /usr/local/bin/edgehop
# Let the logged-in user create virtual input devices (to be controlled) and
# read keyboards, mice and touchpads (to share them).
sudo install -Dm644 60-edgehop-uinput.rules /etc/udev/rules.d/60-edgehop-uinput.rules
sudo install -Dm644 70-edgehop-input.rules /etc/udev/rules.d/70-edgehop-input.rules
sudo install -Dm644 edgehop-uinput.conf /etc/modules-load.d/edgehop-uinput.conf
sudo install -Dm644 edgehop.desktop /usr/local/share/applications/edgehop.desktop
# Optional login-screen service (off until you switch it on in Edgehop's Settings).
if [ -d /etc/systemd/system ]; then
    sed 's#/usr/bin/edgehop#/usr/local/bin/edgehop#' edgehop-login.service | sudo tee /etc/systemd/system/edgehop-login.service >/dev/null
    sudo systemctl daemon-reload 2>/dev/null || true
fi
sudo install -Dm644 edgehop.png /usr/local/share/icons/hicolor/256x256/apps/edgehop.png

sudo modprobe uinput 2>/dev/null || true
sudo udevadm control --reload-rules 2>/dev/null || true
sudo udevadm trigger --name-match=uinput 2>/dev/null || true
sudo udevadm trigger --subsystem-match=input 2>/dev/null || true

echo
echo "Edgehop is installed. For clipboard sharing, install wl-clipboard (Wayland)"
echo "or xclip (X11) with your package manager. Then log out and back in once,"
echo "and open Edgehop from your app menu."
