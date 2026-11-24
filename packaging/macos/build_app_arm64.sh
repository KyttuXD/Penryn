#!/bin/sh
# Builds dist/Penryn.app for Apple Silicon (macOS 11 and newer, arm64) and dist/penryn-macos-arm64.zip.
# Run it on an Apple Silicon Mac (the CI job runs it on macos-15): a normal native build, `cargo build --release`
# for aarch64-apple-darwin linked by Xcode's tools against the real libSystem (the Intel build's text stub and
# Mach-O rewrite, scripts/build-mac.sh, are only for Mac OS X 10.6). The app is made by scripts/package-mac.sh, so
# it has the same launcher and settings as the Intel app. It bundles SDL2 2.30.x (a universal framework):
# SDL2_FRAMEWORK is the path of that SDL2.framework (from the SDL project's SDL2-2.30.x.dmg).
#
# Apple Silicon kills a program without a code signature, so the whole app is signed ad hoc (codesign -s -): no
# developer account is needed. The app runs on the Mac that made it, and on others once the quarantine flag is not
# on it (a download in a browser sets it: right-click, Open, the first time).
set -eu
cd "$(dirname "$0")/../.."
SDL="${SDL2_FRAMEWORK:?SDL2_FRAMEWORK must be the path of SDL2.framework}"
TARGET="${CARGO_TARGET_DIR:-target}"
export MACOSX_DEPLOYMENT_TARGET=11.0
rm -rf dist/Penryn.app dist/penryn-macos-arm64.zip
cargo build --release --locked --target aarch64-apple-darwin
BIN="$TARGET/aarch64-apple-darwin/release/penryn"
[ "$(lipo -archs "$BIN")" = arm64 ] || { echo "$BIN is not an arm64 program: $(lipo -archs "$BIN")" >&2; exit 1; }
PENRYN_BIN="$BIN" PENRYN_MIN_OS=11.0 PENRYN_ARCH=arm64 sh scripts/package-mac.sh "$SDL"
# Signed after everything is in place (any later change to the app breaks the seal).
codesign --force --deep -s - dist/Penryn.app
codesign --verify --deep --strict dist/Penryn.app
# zip -y keeps the framework's symbolic links as links.
( cd dist && zip -qry penryn-macos-arm64.zip Penryn.app )
echo "made dist/penryn-macos-arm64.zip ($(du -sh dist/penryn-macos-arm64.zip | cut -f1))"
