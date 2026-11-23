#!/bin/sh
# Builds dist/Penryn.app (Intel, Mac OS X 10.6 and newer) and dist/penryn-macos-x86_64.zip.
# Penryn is built for the Mac from any host with Rust's x86_64-apple-darwin target and python3
# (scripts/build-mac.sh: no Xcode, no Apple SDK). The app bundles SDL2 2.0.4, the one that runs on
# 10.6: SDL2_FRAMEWORK is the path of that SDL2.framework (KyttuXD/VoxelRust's vendor/sdl2 has it;
# without the variable a VoxelRust checkout beside this repository is used).
set -eu
cd "$(dirname "$0")/../.."
SDL="${SDL2_FRAMEWORK:-../../VoxelRust/vendor/sdl2/SDL2.framework}"
rm -rf dist/mac dist/Penryn.app dist/penryn-macos-x86_64.zip
sh scripts/build-mac.sh
sh scripts/package-mac.sh "$SDL"
( cd dist && zip -qry penryn-macos-x86_64.zip Penryn.app )
echo "made dist/penryn-macos-x86_64.zip ($(du -sh dist/penryn-macos-x86_64.zip | cut -f1))"
