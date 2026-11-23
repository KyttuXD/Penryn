#!/bin/sh
# Build Penryn for Mac OS X 10.6 and later (64-bit Intel), from Linux (or any host).
#
# Needs: Rust with the x86_64-apple-darwin target (rustup target add x86_64-apple-darwin) and
# python3. No Xcode, no Apple SDK, no cross-compiler: Penryn imports a few dozen functions from
# libSystem, all present in 10.6, so a text stub naming them is all the linker needs, and
# Rust's own linker (rust-lld) does the rest. The recipe is VoxelRust's
# (scripts/build-macos-legacy.sh), which runs on the same 2008 Mac Pro.
#
#   scripts/build-mac.sh
#
# Result: dist/mac/penryn. Beside it on the Mac: SDL2.framework (2.0.4 for 10.6), blocks.bin
# and atlas.bin (tools/make_assets.py). See README.md.
set -eu
cd "$(dirname "$0")/.."
TARGET="${CARGO_TARGET_DIR:-target}"
SDK="$(mkdir -p "$TARGET/mac-sdk" && cd "$TARGET/mac-sdk" && pwd)"
OUT="dist/mac"
mkdir -p "$SDK/usr/lib" "$OUT"

# Every libSystem function Penryn may call. A symbol missing from this list fails the link,
# so nothing newer than 10.6 can slip in unnoticed.
cat > "$SDK/usr/lib/libSystem.tbd" <<'TBD'
--- !tapi-tbd
tbd-version: 4
targets: [ x86_64-macos ]
install-name: '/usr/lib/libSystem.B.dylib'
current-version: 1.0
compatibility-version: 1.0
exports:
  - targets: [ x86_64-macos ]
    symbols: [ _malloc, _calloc, _free, _realloc, _posix_memalign, _write, _read, _lseek, _open, _rename, _close, _exit,
               _abort, _getenv, _usleep, _gettimeofday, _socket, _connect, _send, _recv,
               _getaddrinfo, _freeaddrinfo, _poll, _dlopen, _dlsym, _pthread_create, _pthread_detach,
               _pthread_attr_init, _pthread_attr_setstacksize, _pthread_attr_destroy, _bind, _setsockopt, _recvfrom,
               _localtime_r, _system, _sysconf, _mkdir, _symlink, _unlink,
               _memcpy, _memmove, _memset, _memcmp, _bcmp, _bzero, ___bzero, _memset_pattern16,
               dyld_stub_binder ]
...
TBD

# target-cpu=core2: every 64-bit Intel Mac (the Xeon E5462 is Penryn, SSE4.1).
SDKROOT="$SDK" MACOSX_DEPLOYMENT_TARGET=10.6 \
RUSTFLAGS="-C linker=rust-lld -C linker-flavor=ld64.lld -C target-cpu=core2 \
  -C link-arg=-e -C link-arg=_penryn_start \
  -C link-arg=-headerpad -C link-arg=0x400 -C link-arg=-no_pie" \
  cargo build --release --target x86_64-apple-darwin --target-dir "$TARGET/mac"

python3 scripts/macho_legacy.py "$TARGET/mac/x86_64-apple-darwin/release/penryn" "$OUT/penryn"
chmod +x "$OUT/penryn"
echo "built $OUT/penryn"
