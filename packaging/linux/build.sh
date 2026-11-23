#!/bin/sh
# Builds the Linux x86_64 release and packs dist/penryn-linux-x86_64.tar.gz: the program, the
# relay and the game-file maker it needs (tools/), the README and the licence.
# Needs only Rust (no crates) and a C library; at run time Penryn loads SDL2 (libSDL2) itself.
set -eu
cd "$(dirname "$0")/../.."
cargo build --release --locked
OUT=dist/penryn-linux
rm -rf "$OUT" dist/penryn-linux-x86_64.tar.gz
mkdir -p "$OUT/tools"
cp target/release/penryn "$OUT/penryn"
cp tools/relay.py tools/make_assets.py tools/make_icon.py "$OUT/tools/"
cp README.md LICENSE "$OUT/"
tar -C dist -czf dist/penryn-linux-x86_64.tar.gz penryn-linux
rm -rf "$OUT"
( cd dist && sha256sum penryn-linux-x86_64.tar.gz > penryn-linux-x86_64.tar.gz.sha256 )
echo "made dist/penryn-linux-x86_64.tar.gz ($(du -sh dist/penryn-linux-x86_64.tar.gz | cut -f1))"
