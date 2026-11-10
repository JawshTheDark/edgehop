#!/usr/bin/env bash
# Build edgehop.app on a Mac:  ./packaging/macos/build-app.sh   (needs Xcode command line tools + rustup)
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build --release
APP=dist/edgehop.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/edgehop "$APP/Contents/MacOS/edgehop"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>edgehop</string>
  <key>CFBundleIdentifier</key><string>com.edgehop.app</string>
  <key>CFBundleExecutable</key><string>edgehop</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSUIElement</key><false/>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --deep -s - "$APP" 2>/dev/null || true
echo "Built $APP. Move it to /Applications, open it, then allow it under"
echo "System Settings → Privacy & Security → Accessibility."
