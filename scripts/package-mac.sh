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
	<key>NSMicrophoneUsageDescription</key><string>Penryn uses the microphone for voice chat, only when you turn it on in Options.</string>
</dict>
</plist>
EOF

cat > "$APP/Contents/Resources/settings.txt" <<'EOF'
# Penryn's settings. Lines are NAME=value. This file is in
# ~/Library/Application Support/Penryn; open Penryn again after changing it.
# Penryn opens at its title screen: Multiplayer has the server list (add servers there).
# A server here (host or host:port) is put in that list for you. Optional.
SERVER=
# The computer running penryn-relay (tools/relay.py), for signing in and the game files.
RELAY=10.0.0.1:25580
# Leave empty to play signed in (the title screen's "Sign in..." button, or Sign in.command);
# a name plays offline under it (offline-mode servers only).
NAME=
# Chunks in each direction (2 to 16). 6 to 8 suits a Radeon HD 2600. Only until the game's
# Options... screens save options.txt (here too): its render distance is used from then on.
VIEW=8
# yes: full screen (F11 switches too)
FULLSCREEN=
# no: do not look for a newer nightly build when Penryn starts (only the downloaded nightly build looks;
# the relay must be the one from the same download).
UPDATE=
EOF

# Signing in from Terminal, for those who prefer it to the title screen's button.
cat > "$APP/Contents/Resources/Sign in.command" <<'EOF'
#!/bin/sh
# Signs in with your Microsoft account (once; it is remembered), in Terminal. The title
# screen's "Sign in..." button does the same in the game's window.
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
if [ "$RELAY" = "10.0.0.1:25580" ]; then
  open -t "$DATA/settings.txt"
  say "Set RELAY in settings.txt (it is open now), save it, then open Penryn again."
  exit 0
fi
export PENRYN_DATA="$DATA" PENRYN_VIEW="${VIEW:-8}"
# Apple's multithreaded OpenGL engine (driver work off the render thread): 45% more frames on a
# 2015 MacBook Air. Not for Mac OS X 10.6 and 10.7, whose drivers are not trusted with it.
case "$(sw_vers -productVersion 2>/dev/null)" in
  10.[0-7]|10.[0-7].*|"") ;;
  *) export PENRYN_GLMT="${PENRYN_GLMT:-1}" ;;
esac
[ "$FULLSCREEN" = yes ] && export PENRYN_FULLSCREEN=1
# The app updates itself in place from the nightly release (through the relay): it needs to know where it is.
export PENRYN_APP="$(cd "$HERE/../.." && pwd)"
[ "$UPDATE" = no ] && export PENRYN_NO_UPDATE=1
# SERVER (optional) goes into the Multiplayer list (servers.dat) once.
if [ -n "$SERVER" ]; then
  "$HERE/penryn-bin" add-server "$SERVER" >"$LOG" 2>&1
fi
# The title screen. Signing in is its "Sign in..." button (or Sign in.command).
if [ -n "$NAME" ]; then
  "$HERE/penryn-bin" --relay "$RELAY" --name "$NAME" >>"$LOG" 2>&1
else
  "$HERE/penryn-bin" --relay "$RELAY" >>"$LOG" 2>&1
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
