#!/usr/bin/env bash
# Build Edgehop.app on a Mac:  ./packaging/macos/build-app.sh
# Needs the Xcode command line tools and rustup.
# UNIVERSAL=1 builds one app that runs on both Apple silicon and Intel Macs
# (rustup target add aarch64-apple-darwin x86_64-apple-darwin).
set -euo pipefail
cd "$(dirname "$0")/../.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)

if [ "${UNIVERSAL:-0}" = 1 ]; then
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    BIN=target/edgehop-universal
    lipo -create -output "$BIN" \
        target/aarch64-apple-darwin/release/edgehop \
        target/x86_64-apple-darwin/release/edgehop
    RUN=target/aarch64-apple-darwin/release/edgehop
    [ "$(uname -m)" = arm64 ] || RUN=target/x86_64-apple-darwin/release/edgehop
else
    cargo build --release
    BIN=target/release/edgehop
    RUN=$BIN
fi

APP=dist/Edgehop.app
rm -rf "$APP" dist/edgehop.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/edgehop"

# App icon, drawn by the binary itself.
ICONSET=$(mktemp -d)/Edgehop.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    "$RUN" --write-icon "$ICONSET/icon_${s}x${s}.png" "$s"
    "$RUN" --write-icon "$ICONSET/icon_${s}x${s}@2x.png" "$((s * 2))"
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Edgehop.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Edgehop</string>
  <key>CFBundleDisplayName</key><string>Edgehop</string>
  <key>CFBundleIdentifier</key><string>com.edgehop.app</string>
  <key>CFBundleExecutable</key><string>edgehop</string>
  <key>CFBundleIconFile</key><string>Edgehop</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSUIElement</key><false/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSAppleEventsUsageDescription</key><string>Edgehop asks Finder which folder you dropped files into, when you drag them over from another computer.</string>
  <key>NSLocalNetworkUsageDescription</key><string>Edgehop finds and connects to the computer sharing its keyboard and mouse on your local network.</string>
</dict></plist>
PLIST
# Ad-hoc signature: required to run on Apple silicon at all. An ad-hoc
# signature normally identifies one exact build, so macOS would treat every
# update as a new app and drop its Accessibility permission. Pinning the
# designated requirement to the bundle identifier keeps the permission
# across updates.
codesign --force --deep -s - --requirements '=designated => identifier "com.edgehop.app"' "$APP"
echo "Built $APP ($VERSION). Move it to /Applications, open it, then allow it under"
echo "System Settings → Privacy & Security → Accessibility."
