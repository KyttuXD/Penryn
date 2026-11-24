#!/bin/sh
# Builds the Linux release for this machine's processor and packs dist/penryn-linux-<arch>.tar.gz
# (<arch> is `uname -m`: x86_64, or aarch64 on an ARM 64-bit machine): the program, the
# relay and the game-file maker it needs (tools/), the README and the licence. The x86_64 name is the
# long-standing one (penryn-linux-x86_64.tar.gz), unchanged.
# Needs only Rust (no crates) and a C library; at run time Penryn loads SDL2 (libSDL2) itself.
set -eu
cd "$(dirname "$0")/../.."
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|aarch64) ;;
    arm64) ARCH=aarch64 ;;
    *) echo "unsupported processor: $ARCH (x86_64 and aarch64 are built)" >&2; exit 1 ;;
esac
NAME="penryn-linux-$ARCH.tar.gz"
cargo build --release --locked
OUT=dist/penryn-linux
rm -rf "$OUT" "dist/$NAME" "dist/$NAME.sha256"
mkdir -p "$OUT/tools"
cp "${CARGO_TARGET_DIR:-target}/release/penryn" "$OUT/penryn"
cp tools/relay.py tools/make_assets.py tools/make_icon.py "$OUT/tools/"
cp README.md LICENSE "$OUT/"
tar -C dist -czf "dist/$NAME" penryn-linux
rm -rf "$OUT"
( cd dist && sha256sum "$NAME" > "$NAME.sha256" )
echo "made dist/$NAME ($(du -sh "dist/$NAME" | cut -f1))"
