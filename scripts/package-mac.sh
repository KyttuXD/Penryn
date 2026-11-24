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
#
# The Apple Silicon app (packaging/macos/build_app_arm64.sh) is made by this same script, so the launcher and
# the settings are one thing: PENRYN_BIN is the program (default dist/mac/penryn, built by build-mac.sh),
# PENRYN_MIN_OS the LSMinimumSystemVersion (default 10.6) and PENRYN_ARCH the processor (default x86_64;
# arm64 for Apple Silicon).
set -eu
cd "$(dirname "$0")/.."
BIN="${PENRYN_BIN:-dist/mac/penryn}"
MIN_OS="${PENRYN_MIN_OS:-10.6}"
ARCH="${PENRYN_ARCH:-x86_64}"
SDL="${1:-../../VoxelRust/vendor/sdl2/SDL2.framework}"
[ -d "$SDL" ] || { echo "no SDL2.framework at $SDL (give its path)"; exit 1; }
if [ -z "${PENRYN_BIN:-}" ]; then
  [ -x "$BIN" ] || sh scripts/build-mac.sh
fi
[ -x "$BIN" ] || { echo "no program at $BIN"; exit 1; }
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/Penryn.app
rm -rf "$APP" dist/Penryn
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/penryn-bin"
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
	<key>LSMinimumSystemVersion</key><string>$MIN_OS</string>
	<key>LSArchitecturePriority</key><array><string>$ARCH</string></array>
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
# Empty: offline play, no relay at all (offline-mode servers only; the title screen's "Change name..." sets the
# name). The game files must then be in this folder already (see README.md). Or the computer running penryn-relay
# (tools/relay.py): Microsoft sign-in for online-mode servers, skins, server resource packs, the game files and
# updates.
RELAY=
# Offline player name; empty: the one saved by "Change name..." (else the user name of this Mac). With a RELAY,
# empty plays signed in (the "Sign in..." button, or Sign in.command).
NAME=
# Chunks in each direction (2 to 16). 6 to 8 suits a Radeon HD 2600. Only until the game's
# Options... screens save options.txt (here too): its render distance is used from then on.
VIEW=8
# yes: full screen (F11 switches too)
FULLSCREEN=
# no: do not look for a newer nightly build when Penryn starts (only the downloaded nightly build with a RELAY looks;
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
if [ -z "$RELAY" ]; then
  echo "Set RELAY in settings.txt first: signing in needs the relay (without one Penryn plays offline)."
  exit 1
fi
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
# An old settings.txt may still hold the placeholder relay: that is no relay.
[ "$RELAY" = "10.0.0.1:25580" ] && RELAY=
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
# The title screen. With a RELAY signing in is its "Sign in..." button (or Sign in.command); without one it is
# offline play ("Change name..." there).
if [ -z "$RELAY" ]; then
  if [ -n "$NAME" ]; then
    "$HERE/penryn-bin" --offline --name "$NAME" >>"$LOG" 2>&1
  else
    "$HERE/penryn-bin" --offline >>"$LOG" 2>&1
  fi
elif [ -n "$NAME" ]; then
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
