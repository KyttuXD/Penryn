//! The Windows arm of `sys.rs` (x86_64-pc-windows-msvc and aarch64-pc-windows-msvc only; the file is
//! not compiled anywhere else). It gives the POSIX-looking names the rest of the program calls
//! (`open`, `read`, `socket`, `poll`, `pthread_create`, `dlopen`, ...) on top of the Universal CRT,
//! kernel32 and Winsock, so the Linux and Mac paths in `sys.rs` and the callers stay as they are.
//!
//! Decisions worth knowing:
//! - Text is UTF-8 inside the program. Paths, environment variables and the command line are converted
//!   to and from UTF-16 here (`_wopen`, `GetEnvironmentVariableW`, `GetCommandLineW`), so a user name
//!   with umlauts does not break the data folder.
//! - A socket is a `SOCKET` (pointer sized handle). Callers keep an `i32`: handle values are 32-bit
//!   clean on Windows (also in 64-bit processes), and INVALID_SOCKET maps to -1, so `fd < 0` still means
//!   failure. `close` is for files, `sock_close` for sockets (they are different things here).
//! - `poll` is `WSAPoll`; `POLLIN` of the callers (1) is translated to POLLRDNORM and back.
//! - Threads are `CreateThread` with the stack size `task.rs` asks for.
//! - Link directives: the program has no `main` of its own to call: with `#![no_main]` the MSVC linker
//!   uses the CRT's `mainCRTStartup`, which calls our `main(argc, argv)`. The CRT is the static
//!   one (`crt-static`, see `.cargo/config.toml`). The subsystem stays "console" so the logs show; a later
//!   release can switch to /SUBSYSTEM:WINDOWS (with /ENTRY:mainCRTStartup) to hide the console.

use super::{AddrInfo, PollFd, SizeT, SsizeT, TimeVal};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_void, CStr};
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

// The C runtime needs no `#[link]` here: rustc itself adds /defaultlib:msvcrt (or libcmt when the
// build is `crt-static`, which `.cargo/config.toml` asks for, so the zip runs on a PC without the
// "Visual C++ Redistributable"). `mainCRTStartup` of the CRT calls our `main`.

mod ffi {
    use core::ffi::{c_char, c_int, c_void};

    extern "C" {
        pub fn malloc(n: usize) -> *mut c_void;
        pub fn free(p: *mut c_void);
        pub fn realloc(p: *mut c_void, n: usize) -> *mut c_void;
        pub fn _aligned_malloc(n: usize, align: usize) -> *mut c_void;
        pub fn _aligned_free(p: *mut c_void);
        pub fn exit(code: c_int) -> !;
        pub fn abort() -> !;
        pub fn _wopen(path: *const u16, flags: c_int, ...) -> c_int;
        pub fn _read(fd: c_int, buf: *mut c_void, n: u32) -> c_int;
        pub fn _write(fd: c_int, buf: *const c_void, n: u32) -> c_int;
        pub fn _close(fd: c_int) -> c_int;
        pub fn _lseeki64(fd: c_int, offset: i64, whence: c_int) -> i64;
        pub fn _setmode(fd: c_int, mode: c_int) -> c_int;
        pub fn _wmkdir(path: *const u16) -> c_int;
        pub fn _wunlink(path: *const u16) -> c_int;
        pub fn _localtime64_s(tm: *mut c_void, t: *const i64) -> c_int;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateThread(
            attr: *mut c_void,
            stack: usize,
            f: unsafe extern "system" fn(*mut c_void) -> u32,
            param: *mut c_void,
            flags: u32,
            id: *mut u32,
        ) -> *mut c_void;
        pub fn CloseHandle(h: *mut c_void) -> i32;
        pub fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
        pub fn GetExitCodeThread(h: *mut c_void, code: *mut u32) -> i32;
        pub fn Sleep(ms: u32);
        pub fn GetSystemTimePreciseAsFileTime(ft: *mut u64);
        pub fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        pub fn LoadLibraryW(name: *const u16) -> *mut c_void;
        pub fn GetProcAddress(m: *mut c_void, name: *const c_char) -> *mut c_void;
        pub fn GetEnvironmentVariableW(name: *const u16, buf: *mut u16, size: u32) -> u32;
        pub fn GetCommandLineW() -> *const u16;
        pub fn GetActiveProcessorCount(group: u16) -> u32;
        pub fn LocalFree(p: *mut c_void) -> *mut c_void;
    }

    #[link(name = "ws2_32")]
    extern "system" {
        pub fn WSAStartup(version: u16, data: *mut u8) -> i32;
        pub fn socket(af: i32, ty: i32, proto: i32) -> usize;
        pub fn connect(s: usize, name: *const c_void, len: i32) -> i32;
        pub fn send(s: usize, buf: *const c_void, len: i32, flags: i32) -> i32;
        pub fn recv(s: usize, buf: *mut c_void, len: i32, flags: i32) -> i32;
        pub fn recvfrom(s: usize, buf: *mut c_void, len: i32, flags: i32, from: *mut c_void, fromlen: *mut i32) -> i32;
        pub fn bind(s: usize, name: *const c_void, len: i32) -> i32;
        pub fn setsockopt(s: usize, level: i32, name: i32, val: *const c_void, len: i32) -> i32;
        pub fn closesocket(s: usize) -> i32;
        pub fn WSAPoll(fds: *mut super::WsaPollFd, n: u32, timeout: i32) -> i32;
        pub fn getaddrinfo(node: *const c_char, service: *const c_char, hints: *const super::AddrInfo, res: *mut *mut super::AddrInfo) -> i32;
        pub fn freeaddrinfo(res: *mut super::AddrInfo);
    }

    #[link(name = "winmm")]
    extern "system" {
        pub fn timeBeginPeriod(ms: u32) -> u32;
    }

    #[link(name = "advapi32")]
    extern "system" {
        /// RtlGenRandom.
        pub fn SystemFunction036(buf: *mut c_void, len: u32) -> u8;
    }

    #[link(name = "shell32")]
    extern "system" {
        pub fn ShellExecuteW(wnd: *mut c_void, verb: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
        pub fn CommandLineToArgvW(line: *const u16, n: *mut i32) -> *mut *mut u16;
    }
}

// ------------------------------------------------------------------ text

/// A NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

/// The same for a NUL-terminated UTF-8 C string (the paths the callers build with `cstr`).
unsafe fn wide_c(p: *const c_char) -> Vec<u16> {
    wide(&String::from_utf8_lossy(CStr::from_ptr(p).to_bytes()))
}

unsafe fn from_wide(p: *const u16) -> String {
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(core::slice::from_raw_parts(p, n))
}

// ------------------------------------------------------------------ memory

pub unsafe fn malloc(n: SizeT) -> *mut c_void {
    ffi::malloc(n)
}
pub unsafe fn free(p: *mut c_void) {
    ffi::free(p)
}
pub unsafe fn realloc(p: *mut c_void, n: SizeT) -> *mut c_void {
    ffi::realloc(p, n)
}
/// Memory of an alignment above 16 comes from `_aligned_malloc` and must go back to `aligned_free`.
pub unsafe fn posix_memalign(out: *mut *mut c_void, align: SizeT, n: SizeT) -> c_int {
    let p = ffi::_aligned_malloc(n, align);
    *out = p;
    if p.is_null() {
        12
    } else {
        0
    }
}
pub unsafe fn aligned_free(p: *mut c_void) {
    ffi::_aligned_free(p)
}

pub unsafe fn exit(code: c_int) -> ! {
    ffi::exit(code)
}
pub unsafe fn abort() -> ! {
    ffi::abort()
}

// ------------------------------------------------------------------ files (UCRT file descriptors)

const O_BINARY: c_int = 0x8000;
/// _S_IREAD | _S_IWRITE: the only mode bits Windows has.
const S_RW: c_int = 0x100 | 0x80;

/// `open` with the POSIX flags of `sys.rs`: on Windows its flag constants are already the CRT's
/// (O_BINARY included), so they pass through; the mode is reduced to read-write.
pub unsafe fn open(path: *const c_char, flags: c_int, _mode: c_int) -> c_int {
    let w = wide_c(path);
    ffi::_wopen(w.as_ptr(), flags, S_RW)
}
pub unsafe fn read(fd: c_int, buf: *mut c_void, n: SizeT) -> SsizeT {
    ffi::_read(fd, buf, n.min(1 << 30) as u32) as SsizeT
}
pub unsafe fn write(fd: c_int, buf: *const c_void, n: SizeT) -> SsizeT {
    ffi::_write(fd, buf, n.min(1 << 30) as u32) as SsizeT
}
pub unsafe fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64 {
    ffi::_lseeki64(fd, offset, whence)
}
pub unsafe fn close(fd: c_int) -> c_int {
    ffi::_close(fd)
}
pub unsafe fn mkdir(path: *const c_char, _mode: c_int) -> c_int {
    let w = wide_c(path);
    ffi::_wmkdir(w.as_ptr())
}
pub unsafe fn unlink(path: *const c_char) -> c_int {
    let w = wide_c(path);
    ffi::_wunlink(w.as_ptr())
}
/// MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED: renaming over an existing file works, as on POSIX.
pub unsafe fn rename(from: *const c_char, to: *const c_char) -> c_int {
    let (a, b) = (wide_c(from), wide_c(to));
    if ffi::MoveFileExW(a.as_ptr(), b.as_ptr(), 0x1 | 0x2) != 0 {
        0
    } else {
        -1
    }
}
/// No symbolic links on Windows (they need a privilege): only the self-update made them.
pub unsafe fn symlink(_target: *const c_char, _link: *const c_char) -> c_int {
    -1
}

/// stdout and stderr in binary mode (the program writes "\n" and nothing is translated), and a 1 ms
/// timer, so `Sleep(1)` is about 1 ms and not 15.
pub fn init_console() {
    unsafe {
        ffi::timeBeginPeriod(1);
        ffi::_setmode(1, O_BINARY);
        ffi::_setmode(2, O_BINARY);
    }
}

// ------------------------------------------------------------------ time

const FILETIME_TO_UNIX: u64 = 116_444_736_000_000_000;

pub unsafe fn gettimeofday(tv: *mut TimeVal, _tz: *mut c_void) -> c_int {
    let mut ft: u64 = 0;
    ffi::GetSystemTimePreciseAsFileTime(&mut ft);
    let t = ft.saturating_sub(FILETIME_TO_UNIX); // 100 ns units since 1970
    (*tv).sec = (t / 10_000_000) as i64;
    (*tv).usec = ((t % 10_000_000) / 10) as i64;
    0
}

pub unsafe fn usleep(us: u32) -> c_int {
    ffi::Sleep(us / 1000);
    0
}

/// `localtime_r`: the first nine ints of the CRT's `struct tm` are the POSIX ones.
pub unsafe fn localtime_r(t: *const i64, out: *mut c_void) -> *mut c_void {
    if ffi::_localtime64_s(out, t) == 0 {
        out
    } else {
        core::ptr::null_mut()
    }
}

/// The processor count (the argument, a Linux/Mac sysconf name, is ignored).
pub unsafe fn sysconf(_name: c_int) -> i64 {
    ffi::GetActiveProcessorCount(0xFFFF) as i64
}

// ------------------------------------------------------------------ process

/// The command line as UTF-8 arguments (the CRT's argv is in the ANSI code page).
pub fn args() -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let mut n: i32 = 0;
        let v = ffi::CommandLineToArgvW(ffi::GetCommandLineW(), &mut n);
        if v.is_null() {
            return out;
        }
        for i in 0..n.max(0) as usize {
            out.push(from_wide(*v.add(i)));
        }
        ffi::LocalFree(v as *mut c_void);
    }
    out
}

/// An environment variable as UTF-8 (None when unset or empty).
pub fn getenv_utf8(name: &str) -> Option<String> {
    let n = wide(name);
    let mut buf: Vec<u16> = alloc::vec![0; 512];
    loop {
        let r = unsafe { ffi::GetEnvironmentVariableW(n.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if r == 0 {
            return None;
        }
        if r > buf.len() {
            buf.resize(r, 0);
            continue;
        }
        return Some(String::from_utf16_lossy(&buf[..r]));
    }
}

/// An http(s) address in the default browser.
pub fn shell_open(url: &str) -> bool {
    let (verb, file) = (wide("open"), wide(url));
    unsafe { ffi::ShellExecuteW(core::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), core::ptr::null(), core::ptr::null(), 1) > 32 }
}

pub fn random_bytes(buf: &mut [u8]) -> bool {
    unsafe { ffi::SystemFunction036(buf.as_mut_ptr() as *mut c_void, buf.len() as u32) != 0 }
}

pub struct MainArgs {
    pub f: fn(c_int, *const *const c_char) -> c_int,
    pub argc: c_int,
    pub argv: *const *const c_char,
}

unsafe extern "system" fn main_thread(p: *mut c_void) -> u32 {
    let m = Box::from_raw(p as *mut MainArgs);
    (m.f)(m.argc, m.argv) as u32
}

/// Runs the program on a thread with a 16 MB stack (a Windows exe gets 1 MB for its main thread, Linux
/// 8 MB) and waits for it; its return value is the process's. The stack is reserved, not committed.
pub fn run_main(m: MainArgs) -> c_int {
    let (f, argc, argv) = (m.f, m.argc, m.argv);
    unsafe {
        let raw = Box::into_raw(Box::new(m));
        // STACK_SIZE_PARAM_IS_A_RESERVATION
        let h = ffi::CreateThread(core::ptr::null_mut(), 16 << 20, main_thread, raw as *mut c_void, 0x10000, core::ptr::null_mut());
        if h.is_null() {
            drop(Box::from_raw(raw));
            return f(argc, argv);
        }
        ffi::WaitForSingleObject(h, 0xFFFF_FFFF);
        let mut code: u32 = 1;
        ffi::GetExitCodeThread(h, &mut code);
        ffi::CloseHandle(h);
        code as c_int
    }
}

// ------------------------------------------------------------------ threads

struct Start {
    f: extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
}

unsafe extern "system" fn thread_start(p: *mut c_void) -> u32 {
    let s = Box::from_raw(p as *mut Start);
    (s.f)(s.arg);
    0
}

/// `pthread_attr_t` stand-in: the first 8 bytes hold the stack size (the callers give 128 bytes).
pub unsafe fn pthread_attr_init(attr: *mut c_void) -> c_int {
    *(attr as *mut usize) = 0;
    0
}
pub unsafe fn pthread_attr_setstacksize(attr: *mut c_void, size: SizeT) -> c_int {
    *(attr as *mut usize) = size;
    0
}
pub unsafe fn pthread_attr_destroy(_attr: *mut c_void) -> c_int {
    0
}
/// `t` receives the thread handle; `pthread_detach` closes it.
pub unsafe fn pthread_create(t: *mut usize, attr: *const c_void, f: extern "C" fn(*mut c_void) -> *mut c_void, arg: *mut c_void) -> c_int {
    let stack = if attr.is_null() { 0 } else { *(attr as *const usize) };
    let raw = Box::into_raw(Box::new(Start { f, arg }));
    let h = ffi::CreateThread(core::ptr::null_mut(), stack, thread_start, raw as *mut c_void, if stack != 0 { 0x10000 } else { 0 }, core::ptr::null_mut());
    if h.is_null() {
        drop(Box::from_raw(raw));
        return 11;
    }
    *t = h as usize;
    0
}
pub unsafe fn pthread_detach(t: usize) -> c_int {
    ffi::CloseHandle(t as *mut c_void);
    0
}

// ------------------------------------------------------------------ dynamic libraries

/// The library `dlopen` loaded last: where a lookup with the "default" handle (null) goes. SDL is the
/// only library the program loads; `dlsym(NULL, ...)` is how it finds SDL's later functions.
static LAST_LIB: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

/// LoadLibrary: a bare name ("SDL2.dll") is looked for in the program's own folder first.
pub unsafe fn dlopen(path: *const c_char, _mode: c_int) -> *mut c_void {
    let w = wide_c(path);
    let h = ffi::LoadLibraryW(w.as_ptr());
    if !h.is_null() {
        LAST_LIB.store(h, Ordering::Relaxed);
    }
    h
}
pub unsafe fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    let h = if handle.is_null() { LAST_LIB.load(Ordering::Relaxed) } else { handle };
    if h.is_null() {
        return core::ptr::null_mut();
    }
    ffi::GetProcAddress(h, name)
}

// ------------------------------------------------------------------ sockets (Winsock 2)

/// POLLRDNORM, POLLERR | POLLHUP | POLLNVAL: what WSAPoll reports for "readable" and "look at it".
const WSA_POLLIN: i16 = 0x100;
const WSA_POLLANY: i16 = 0x300 | 0x7;

#[repr(C)]
pub struct WsaPollFd {
    fd: usize,
    events: i16,
    revents: i16,
}

static WSA_STATE: AtomicU32 = AtomicU32::new(0);

/// WSAStartup, once, before the first socket call (any thread; the others wait for it).
fn wsa_init() {
    if WSA_STATE.load(Ordering::Acquire) == 2 {
        return;
    }
    if WSA_STATE.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        let mut data = [0u8; 512]; // WSADATA is 408 bytes on 64-bit Windows
        unsafe { ffi::WSAStartup(0x0202, data.as_mut_ptr()) };
        WSA_STATE.store(2, Ordering::Release);
    } else {
        while WSA_STATE.load(Ordering::Acquire) != 2 {
            unsafe { ffi::Sleep(1) };
        }
    }
}

fn handle(fd: c_int) -> usize {
    fd as u32 as usize
}

pub unsafe fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int {
    wsa_init();
    let s = ffi::socket(domain, ty, proto);
    if s == usize::MAX {
        -1
    } else {
        s as u32 as c_int
    }
}
pub unsafe fn connect(fd: c_int, addr: *const c_void, len: usize) -> c_int {
    ffi::connect(handle(fd), addr, len as i32)
}
pub unsafe fn send(fd: c_int, buf: *const c_void, n: SizeT, flags: c_int) -> SsizeT {
    ffi::send(handle(fd), buf, n.min(1 << 30) as i32, flags) as SsizeT
}
pub unsafe fn recv(fd: c_int, buf: *mut c_void, n: SizeT, flags: c_int) -> SsizeT {
    ffi::recv(handle(fd), buf, n.min(1 << 30) as i32, flags) as SsizeT
}
pub unsafe fn recvfrom(fd: c_int, buf: *mut c_void, n: SizeT, flags: c_int, from: *mut c_void, len: *mut u32) -> SsizeT {
    ffi::recvfrom(handle(fd), buf, n.min(1 << 30) as i32, flags, from, len as *mut i32) as SsizeT
}
pub unsafe fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int {
    ffi::bind(handle(fd), addr, len as i32)
}
pub unsafe fn setsockopt(fd: c_int, level: c_int, name: c_int, val: *const c_void, len: u32) -> c_int {
    ffi::setsockopt(handle(fd), level, name, val, len as i32)
}
pub unsafe fn sock_close(fd: c_int) -> c_int {
    ffi::closesocket(handle(fd))
}
pub unsafe fn poll(fds: *mut PollFd, n: usize, timeout_ms: c_int) -> c_int {
    let mut v: Vec<WsaPollFd> = Vec::with_capacity(n);
    for i in 0..n {
        let p = &*fds.add(i);
        v.push(WsaPollFd { fd: handle(p.fd), events: if p.events & 1 != 0 { WSA_POLLIN } else { 0 }, revents: 0 });
    }
    let r = ffi::WSAPoll(v.as_mut_ptr(), n as u32, timeout_ms);
    if r >= 0 {
        for i in 0..n {
            (*fds.add(i)).revents = if v[i].revents & WSA_POLLANY != 0 { 1 } else { 0 };
        }
    }
    r
}
pub unsafe fn getaddrinfo(node: *const c_char, service: *const c_char, hints: *const AddrInfo, res: *mut *mut AddrInfo) -> c_int {
    wsa_init();
    ffi::getaddrinfo(node, service, hints, res)
}
pub unsafe fn freeaddrinfo(res: *mut AddrInfo) {
    ffi::freeaddrinfo(res)
}
