#!/usr/bin/env bash
# Smoke-test a built Edgehop.app on a Mac nobody is sitting at (GitHub Actions):
#   1. checks the binary, its architectures and its signature;
#   2. runs `edgehop --self-test --type`: clipboard text and images, moving the
#      pointer, and typing a sentence into TextEdit, then copying it back;
#   3. opens the app the way a user would, checks it's still running 20 s
#      later, and takes screenshots.
# Everything it collects (screenshots, logs, self-test output) lands in smoke/.
# Usage: packaging/macos/smoke-test.sh [path/to/Edgehop.app]
set -o pipefail
cd "$(dirname "$0")/../.."
APP=${1:-dist/Edgehop.app}
BIN="$APP/Contents/MacOS/edgehop"
OUT=smoke
rm -rf "$OUT" && mkdir -p "$OUT"
failures=""
fail() { failures="$failures\n- $1"; echo "FAILED: $1"; }
step() { printf '\n== %s\n' "$1"; }
shot() { screencapture -x "$OUT/$1.png" 2>/dev/null || echo "(no screenshot: $1)"; }

step "System"
sw_vers

step "Binary"
file "$BIN"
lipo -info "$BIN"
otool -L "$BIN" | sed 1d
if codesign --verify --deep --strict "$APP"; then echo "signature: valid (ad-hoc)"; else fail "code signature"; fi
requirement=$(codesign -d -r- "$APP" 2>&1 | grep designated)
echo "$requirement"
case "$requirement" in
    *'identifier "com.edgehop.app"'*) echo "permissions survive updates (requirement is the bundle identifier)" ;;
    *) fail "designated requirement isn't the bundle identifier" ;;
esac

step "Self-test (clipboard, pointer, keyboard, sharing)"
# Something to type into: an empty document in TextEdit, in front.
: > "$OUT/typing.txt"
open -a TextEdit "$OUT/typing.txt"
sleep 5
shot 1-textedit-ready
"$BIN" --self-test --type --sharing | tee "$OUT/self-test.txt" || fail "self-test"
shot 2-after-typing
pkill -x TextEdit

step "Share this Mac's keyboard and mouse (server role)"
# Run straight from the shell (which may control input here), with its own settings.
server_cfg="$(mktemp -d)/config.json"
echo '{"role": "Server", "name": "ci-mac"}' > "$server_cfg"
EDGEHOP_CONFIG="$server_cfg" "$BIN" > "$OUT/server-output.txt" 2>&1 &
server=$!
sleep 12
if kill -0 "$server" 2> /dev/null; then
    echo "Edgehop is sharing and still running 12 s later"
else
    fail "Edgehop quit or crashed while sharing"
fi
shot 3-sharing-screens-tab
kill "$server" 2> /dev/null
wait "$server" 2> /dev/null

step "Launch Edgehop.app"
open "$APP"
for _ in $(seq 20); do
    sleep 1
    pgrep -x edgehop > /dev/null || break
done
if pgrep -x edgehop > /dev/null; then
    echo "Edgehop is still running 20 s after launch"
else
    fail "Edgehop quit or crashed within 20 s of launch"
fi
shot 4-edgehop-window

step "Logs"
log=$(sed -n 's/^info  *log file  *//p' "$OUT/self-test.txt")
if [ -f "$log" ]; then
    cp "$log" "$OUT/edgehop.log"
    cat "$OUT/edgehop.log"
    if grep -qi 'panicked' "$OUT/edgehop.log"; then fail "panic in the log"; fi
    if grep -q 'input capture running' "$OUT/edgehop.log"; then
        echo "sharing: input capture started"
    else
        fail "sharing: input capture never started (see edgehop.log)"
    fi
else
    echo "no log file at '$log'"
fi
crashes=$(find "$HOME/Library/Logs/DiagnosticReports" -iname '*edgehop*' 2>/dev/null)
if [ -n "$crashes" ]; then
    echo "$crashes" | while read -r f; do cp "$f" "$OUT/"; done
    fail "macOS crash report"
fi
pkill -x edgehop

step "Update from inside Edgehop to the newest published release (pretending to be 0.0.1)"
# Needs GitHub's API and the last release's files: a release build sets
# UPDATE_TEST_OPTIONAL so trouble there can't block shipping a fix.
failures_before="$failures"
latest=$(curl -fsSL https://api.github.com/repos/JawshTheDark/edgehop/releases/latest |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["tag_name"].lstrip("v"))')
upd=$(mktemp -d)
cp -R "$APP" "$upd/Edgehop.app"
EDGEHOP_PRETEND_VERSION=0.0.1 "$upd/Edgehop.app/Contents/MacOS/edgehop" --update 2>&1 | tee "$OUT/update.txt" ||
    fail "edgehop --update"
version() { /usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$upd/Edgehop.app/Contents/Info.plist" 2>/dev/null; }
for _ in $(seq 90); do
    [ "$(version)" = "$latest" ] && break
    sleep 1
done
echo "Edgehop.app is now $(version) (newest release: $latest)"
[ "$(version)" = "$latest" ] || fail "the update didn't replace Edgehop.app"
ls "$upd"
if ls "$upd" | grep -q '\.old-'; then fail "the update left the old Edgehop.app behind"; fi
if codesign --verify --deep --strict "$upd/Edgehop.app"; then echo "updated app's signature: valid"; else fail "updated app's signature"; fi
sleep 5
if pgrep -f "$upd/Edgehop.app" > /dev/null; then echo "the updated Edgehop was opened"; else echo "(the updated Edgehop isn't running)"; fi
pkill -f "$upd/Edgehop.app" 2> /dev/null
rm -rf "$upd"

# GitHub answers only so many questions an hour from one address, and CI's
# Macs share theirs with everyone's builds: being turned away (403/429) says
# nothing about Edgehop.
if [ "$failures" != "$failures_before" ] && grep -Eq 'returned error: (403|429)' "$OUT/update.txt"; then
    echo "::warning::The update test couldn't run: GitHub is turning away requests from this runner's address (rate limit)"
    failures="$failures_before"
fi
if [ -n "${UPDATE_TEST_OPTIONAL:-}" ] && [ "$failures" != "$failures_before" ]; then
    echo "::warning::The update test failed (not required for a release)"
    failures="$failures_before"
fi

step "Reset permissions (what the button in the app runs)"
if "$BIN" --reset-permissions; then
    echo "macOS accepted the permission reset"
else
    fail "permission reset (tccutil) failed"
fi

step "Result"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        echo "### macOS smoke test"
        echo '```'
        cat "$OUT/self-test.txt"
        echo '```'
        [ -z "$failures" ] && echo "App launched and kept running." || printf "**Failed:**%b\n" "$failures"
        echo "Screenshots and logs: the *smoke-macos* artifact."
    } >> "$GITHUB_STEP_SUMMARY"
fi
if [ -n "$failures" ]; then
    printf "macOS smoke test failed:%b\n" "$failures"
    exit 1
fi
echo "macOS smoke test passed"
