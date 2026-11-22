#!/bin/sh
# Installs dist/Penryn.app into /Applications on a Mac over SSH, and moves an older
# ~/Penryn folder's settings, sign-in and game files into ~/Library/Application Support/Penryn.
#
#   scripts/install-mac.sh HOST [SERVER RELAY]
#
# HOST is an SSH destination (e.g. KT-MBA). SERVER and RELAY, when given, are written into
# settings.txt (e.g. 10.0.0.185:25599 10.0.0.185:25580).
set -eu
cd "$(dirname "$0")/.."
HOST="$1"
[ -d dist/Penryn.app ] || sh scripts/package-mac.sh
STAGE='Library/Caches/penryn-install'
ssh "$HOST" "rm -rf ~/$STAGE && mkdir -p ~/$STAGE"
tar -C dist -cf - Penryn.app | ssh "$HOST" "tar -C ~/$STAGE -xf -"
ssh "$HOST" sh -s -- "${2:-}" "${3:-}" <<'EOF'
set -eu
SERVER="$1"; RELAY="$2"
STAGE="$HOME/Library/Caches/penryn-install"
DATA="$HOME/Library/Application Support/Penryn"
mkdir -p "$DATA"
# /Applications is writable by admin users; else ~/Applications.
DEST=/Applications
[ -w "$DEST" ] || { DEST="$HOME/Applications"; mkdir -p "$DEST"; }
rm -rf "$DEST/Penryn.app"
mv "$STAGE/Penryn.app" "$DEST/Penryn.app"
rm -rf "$STAGE"
# An older folder install: keep its data, drop the rest.
if [ -d "$HOME/Penryn" ] && [ -x "$HOME/Penryn/penryn" ]; then
  for f in settings.txt account.txt blocks.bin atlas.bin gui.bin entities.bin sounds.bin; do
    [ -f "$HOME/Penryn/$f" ] && [ ! -f "$DATA/$f" ] && mv "$HOME/Penryn/$f" "$DATA/$f"
  done
  rm -rf "$HOME/Penryn"
fi
[ -f "$DATA/settings.txt" ] || cp "$DEST/Penryn.app/Contents/Resources/settings.txt" "$DATA/settings.txt"
if [ -n "$SERVER" ]; then
  sed -i '' -e "s/^SERVER=.*/SERVER=$SERVER/" -e "s/^RELAY=.*/RELAY=$RELAY/" "$DATA/settings.txt"
fi
# Finder and the Dock pick up the new icon and version.
touch "$DEST/Penryn.app"
echo "installed $DEST/Penryn.app; data in $DATA"
EOF
