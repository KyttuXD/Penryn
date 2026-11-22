#!/bin/sh
# Make dist/Penryn.app, the Mac application (Mac OS X 10.6 or later, 64-bit Intel). It is
# meant to live in /Applications (scripts/install-mac.sh puts it there).
#
#   scripts/package-mac.sh [SDL2.framework]
#
# Inside: the program, SDL2 (the 2.0.4 framework runs on 10.6; give its path, else VoxelRust's
# vendored copy beside this repository is used) and a small launcher. The app itself is never
# written to: settings.txt, the sign-in and the game files (made by the relay from your own copy
# of Minecraft, fetched on first start) live in ~/Library/Application Support/Penryn.
set -eu
cd "$(dirname "$0")/.."
SDL="${1:-../../VoxelRust/vendor/sdl2/SDL2.framework}"
[ -d "$SDL" ] || { echo "no SDL2.framework at $SDL (give its path)"; exit 1; }
[ -x dist/mac/penryn ] || sh scripts/build-mac.sh
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/Penryn.app
rm -rf "$APP" dist/Penryn
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"
cp dist/mac/penryn "$APP/Contents/MacOS/penryn-bin"
# Copy the framework without the AppleDouble "._" files a non-Mac copy picks up.
tar -C "$(dirname "$SDL")" --exclude='._*' -cf - "$(basename "$SDL")" | tar -C "$APP/Contents/Frameworks" -xf -
python3 -I tools/make_icon.py "$APP/Contents/Resources/Penryn.icns"
cp README.md LICENSE "$APP/Contents/Resources/"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>Penryn</string>
	<key>CFBundleDisplayName</key><string>Penryn</string>
	<key>CFBundleIdentifier</key><string>org.penryn.client</string>
	<key>CFBundleExecutable</key><string>Penryn</string>
	<key>CFBundleIconFile</key><string>Penryn</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>$VERSION</string>
	<key>CFBundleVersion</key><string>$VERSION</string>
	<key>LSMinimumSystemVersion</key><string>10.6</string>
	<key>LSArchitecturePriority</key><array><string>x86_64</string></array>
	<key>NSHighResolutionCapable</key><false/>
	<key>LSApplicationCategoryType</key><string>public.app-category.games</string>
</dict>
</plist>
EOF

cat > "$APP/Contents/Resources/settings.txt" <<'EOF'
# Penryn's settings. Lines are NAME=value. This file is in
# ~/Library/Application Support/Penryn; open Penryn again after changing it.
# The Minecraft server to join (host or host:port).
SERVER=10.0.0.1:25565
# The computer running penryn-relay (tools/relay.py), for signing in and the game files.
RELAY=10.0.0.1:25580
# Leave empty to play signed in; a name plays offline under it (offline-mode servers only).
NAME=
# Chunks in each direction (2 to 16). 6 to 8 suits a Radeon HD 2600. Only until the game's
# Options... screens save options.txt (here too): its render distance is used from then on.
VIEW=8
# yes: full screen (F11 switches too)
FULLSCREEN=
EOF

# Sign-in through Microsoft's device code needs a window to show the code in: Terminal.
cat > "$APP/Contents/Resources/Sign in.command" <<'EOF'
#!/bin/sh
# Signs in with your Microsoft account (once; it is remembered). Penryn opens this when the
# relay's computer has no account signed in to Horizon's launcher.
DATA="$HOME/Library/Application Support/Penryn"
. "$DATA/settings.txt"
export PENRYN_DATA="$DATA"
"$(dirname "$0")/../MacOS/penryn-bin" login "$RELAY"
echo
echo "Done: open Penryn again. You can close this window."
EOF

# The app's executable: the launcher. Finder starts it with no terminal, so anything the
# player must see goes to a dialog.
cat > "$APP/Contents/MacOS/Penryn" <<'EOF'
#!/bin/sh
HERE="$(cd "$(dirname "$0")" && pwd)"
RES="$HERE/../Resources"
DATA="$HOME/Library/Application Support/Penryn"
mkdir -p "$DATA"
LOG="$DATA/penryn.log"
say() {
  osascript -e "tell application \"System Events\" to activate" \
    -e "tell application \"System Events\" to display dialog \"$1\" with title \"Penryn\" buttons {\"OK\"} default button 1" >/dev/null 2>&1
}
if [ ! -f "$DATA/settings.txt" ]; then
  cp "$RES/settings.txt" "$DATA/settings.txt"
fi
. "$DATA/settings.txt"
if [ "$SERVER" = "10.0.0.1:25565" ] || [ "$RELAY" = "10.0.0.1:25580" ]; then
  open -t "$DATA/settings.txt"
  say "Set SERVER and RELAY in settings.txt (it is open now), save it, then open Penryn again."
  exit 0
fi
export PENRYN_DATA="$DATA" PENRYN_VIEW="${VIEW:-8}"
[ "$FULLSCREEN" = yes ] && export PENRYN_FULLSCREEN=1
if [ -z "$NAME" ] && [ ! -f "$DATA/account.txt" ]; then
  # Through Horizon's launcher on the relay's computer when it has an account; else the
  # device code, which needs Terminal.
  if ! "$HERE/penryn-bin" login "$RELAY" >"$LOG" 2>&1; then
    chmod +x "$RES/Sign in.command"
    open -a Terminal "$RES/Sign in.command"
    exit 0
  fi
fi
if [ -n "$NAME" ]; then
  "$HERE/penryn-bin" --relay "$RELAY" --name "$NAME" "$SERVER" >"$LOG" 2>&1
else
  "$HERE/penryn-bin" --relay "$RELAY" "$SERVER" >"$LOG" 2>&1
fi
code=$?
if [ $code -ne 0 ]; then
  msg=$(grep -v '^perf:' "$LOG" | tail -n 3 | tr '"\\' "''" | tr '\n' ' ')
  say "Penryn stopped: $msg (the whole log is in ~/Library/Application Support/Penryn/penryn.log)"
fi
exit $code
EOF
chmod +x "$APP/Contents/MacOS/Penryn" "$APP/Contents/MacOS/penryn-bin" "$APP/Contents/Resources/Sign in.command"
echo "made $APP ($(du -sh "$APP" | cut -f1))"
