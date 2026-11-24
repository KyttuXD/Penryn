//! The C library, called directly. Everything here exists both in glibc and in Mac OS X 10.6's
//! libSystem; nothing newer is used (no `clock_gettime`, no thread-locals), so that the
//! Mac build loads on 10.6. Every symbol used here must also be listed in the libSystem
//! stub in `scripts/build-mac-10.6.sh`.

use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::ffi::{c_char, c_int, c_void};

// Windows: the same names on the UCRT, kernel32 and Winsock (sys_win.rs). The POSIX code below is not
// compiled there, and nothing in it changes for Linux and macOS.
#[cfg(windows)]
#[path = "sys_win.rs"]
mod win;
#[cfg(windows)]
pub use win::*;

pub type SizeT = usize;
pub type SsizeT = isize;

#[repr(C)]
pub struct PollFd {
    pub fd: c_int,
    pub events: i16,
    pub revents: i16,
}
pub const POLLIN: i16 = 0x1;

/// `struct addrinfo`. glibc and Darwin order the last fields differently.
#[cfg(target_os = "macos")]
#[repr(C)]
pub struct AddrInfo {
    pub ai_flags: c_int,
    pub ai_family: c_int,
    pub ai_socktype: c_int,
    pub ai_protocol: c_int,
    pub ai_addrlen: u32,
    pub ai_canonname: *mut c_char,
    pub ai_addr: *mut c_void,
    pub ai_next: *mut AddrInfo,
}
#[cfg(windows)]
#[repr(C)]
pub struct AddrInfo {
    pub ai_flags: c_int,
    pub ai_family: c_int,
    pub ai_socktype: c_int,
    pub ai_protocol: c_int,
    /// size_t on Windows.
    pub ai_addrlen: usize,
    pub ai_canonname: *mut c_char,
    pub ai_addr: *mut c_void,
    pub ai_next: *mut AddrInfo,
}
#[cfg(not(any(target_os = "macos", windows)))]
#[repr(C)]
pub struct AddrInfo {
    pub ai_flags: c_int,
    pub ai_family: c_int,
    pub ai_socktype: c_int,
    pub ai_protocol: c_int,
    pub ai_addrlen: u32,
    pub ai_addr: *mut c_void,
    pub ai_canonname: *mut c_char,
    pub ai_next: *mut AddrInfo,
}

/// `struct timeval`: on Darwin `tv_usec` is 32 bits followed by padding.
#[repr(C)]
pub(crate) struct TimeVal {
    sec: i64,
    #[cfg(target_os = "macos")]
    usec: i32,
    #[cfg(target_os = "macos")]
    _pad: i32,
    #[cfg(not(target_os = "macos"))]
    usec: i64,
}

pub const SOCK_STREAM: c_int = 1;
pub const RTLD_NOW: c_int = 2;

#[cfg(not(windows))]
#[cfg_attr(all(unix, not(target_os = "macos")), link(name = "c"))]
#[cfg_attr(target_os = "linux", link(name = "dl"))]
#[cfg_attr(target_os = "macos", link(name = "System"))]
extern "C" {
    fn malloc(n: SizeT) -> *mut c_void;
    fn free(p: *mut c_void);
    fn realloc(p: *mut c_void, n: SizeT) -> *mut c_void;
    fn posix_memalign(out: *mut *mut c_void, align: SizeT, n: SizeT) -> c_int;
    fn write(fd: c_int, buf: *const c_void, n: SizeT) -> SsizeT;
    fn read(fd: c_int, buf: *mut c_void, n: SizeT) -> SsizeT;
    fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
    fn open(path: *const c_char, flags: c_int, mode: c_int) -> c_int;
    fn mkdir(path: *const c_char, mode: c_int) -> c_int;
    fn rename(from: *const c_char, to: *const c_char) -> c_int;
    pub fn close(fd: c_int) -> c_int;
    pub fn exit(code: c_int) -> !;
    pub fn abort() -> !;
    fn getenv(name: *const c_char) -> *const c_char;
    fn usleep(us: u32) -> c_int;
    fn gettimeofday(tv: *mut TimeVal, tz: *mut c_void) -> c_int;
    pub fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int;
    pub fn connect(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    pub fn send(fd: c_int, buf: *const c_void, n: SizeT, flags: c_int) -> SsizeT;
    pub fn recv(fd: c_int, buf: *mut c_void, n: SizeT, flags: c_int) -> SsizeT;
    pub fn getaddrinfo(
        node: *const c_char,
        service: *const c_char,
        hints: *const AddrInfo,
        res: *mut *mut AddrInfo,
    ) -> c_int;
    pub fn freeaddrinfo(res: *mut AddrInfo);
    pub fn poll(fds: *mut PollFd, n: usize, timeout_ms: c_int) -> c_int;
    pub fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    pub fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    /// The skin download thread (skins.rs). pthread_t is 8 bytes on both systems.
    pub fn pthread_create(t: *mut usize, attr: *const c_void, f: extern "C" fn(*mut c_void) -> *mut c_void, arg: *mut c_void) -> c_int;
    pub fn pthread_detach(t: usize) -> c_int;
    /// Background work with its own stack size (task.rs); the menus' LAN discovery (UDP) and
    /// the local date of the title screen's splash.
    pub fn pthread_attr_init(attr: *mut c_void) -> c_int;
    pub fn pthread_attr_setstacksize(attr: *mut c_void, size: SizeT) -> c_int;
    pub fn pthread_attr_destroy(attr: *mut c_void) -> c_int;
    pub fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    pub fn setsockopt(fd: c_int, level: c_int, name: c_int, val: *const c_void, len: u32) -> c_int;
    pub fn recvfrom(fd: c_int, buf: *mut c_void, n: SizeT, flags: c_int, from: *mut c_void, len: *mut u32) -> SsizeT;
    fn localtime_r(t: *const i64, out: *mut c_void) -> *mut c_void;
    fn system(cmd: *const c_char) -> c_int;
    /// The processor count, for the meshing threads (mesher.rs).
    fn sysconf(name: c_int) -> i64;
}

#[cfg(target_os = "linux")]
extern "C" {
    fn readlink(path: *const c_char, buf: *mut u8, n: SizeT) -> SsizeT;
}

#[cfg(not(windows))]
extern "C" {
    fn symlink(target: *const c_char, link: *const c_char) -> c_int;
    fn unlink(path: *const c_char) -> c_int;
}

/// Closes a socket (a plain `close` here; on Windows sockets are not file descriptors).
#[cfg(not(windows))]
pub unsafe fn sock_close(fd: c_int) -> c_int {
    close(fd)
}

/// Runs a shell command and waits for it (its exit status, -1 when it could not start).
/// Windows: only the self-update ran commands, and it does not exist there.
#[cfg(windows)]
pub fn shell(_cmd: &str) -> i32 {
    -1
}

/// Runs a shell command and waits for it (its exit status, -1 when it could not start).
#[cfg(not(windows))]
pub fn shell(cmd: &str) -> i32 {
    let c = cstr(cmd);
    let r = unsafe { system(c.as_ptr() as *const c_char) };
    if r == -1 {
        -1
    } else {
        (r >> 8) & 0xff
    }
}

/// Makes `link` a symbolic link to `target` (replacing a file or link there).
pub fn make_symlink(target: &str, link: &str) -> bool {
    let (t, l) = (cstr(target), cstr(link));
    unsafe {
        unlink(l.as_ptr() as *const c_char);
        symlink(t.as_ptr() as *const c_char, l.as_ptr() as *const c_char) == 0
    }
}

/// Deletes a file or link.
pub fn remove_file(path: &str) {
    let p = cstr(path);
    unsafe { unlink(p.as_ptr() as *const c_char) };
}

/// Where the running program is (Linux only: /proc/self/exe).
pub fn exe_path() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        read_link("/proc/self/exe")
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Where a symbolic link points (None: not a link, or too long).
#[cfg(target_os = "linux")]
fn read_link(path: &str) -> Option<String> {
    let p = cstr(path);
    let mut buf = [0u8; 4096];
    let n = unsafe { readlink(p.as_ptr() as *const c_char, buf.as_mut_ptr(), buf.len()) };
    if n <= 0 || n as usize >= buf.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// Linux: keeps ~/.local/bin/penryn a link to the program that is running, every run, so `penryn`
/// works from a terminal wherever it was built or put. Nothing is done when it already is, or when
/// the program runs from that path itself. Elsewhere (Mac) it does nothing.
pub fn install_local_bin() {
    #[cfg(target_os = "linux")]
    {
        let (Some(home), Some(exe)) = (env("HOME"), read_link("/proc/self/exe")) else { return };
        let dir = alloc::format!("{}/.local/bin", home);
        let link = alloc::format!("{}/penryn", dir);
        if exe == link || read_link(&link).as_deref() == Some(exe.as_str()) {
            return;
        }
        make_dir(&alloc::format!("{}/.local", home));
        make_dir(&dir);
        let (l, e) = (cstr(&link), cstr(&exe));
        unsafe {
            unlink(l.as_ptr() as *const c_char);
            symlink(e.as_ptr() as *const c_char, l.as_ptr() as *const c_char);
        }
    }
}

/// Processors online (sysconf _SC_NPROCESSORS_ONLN: 58 on Mac OS X, 84 on Linux), at least 1.
pub fn processors() -> u32 {
    let name = if cfg!(target_os = "macos") { 58 } else { 84 };
    unsafe { sysconf(name) }.clamp(1, 64) as u32
}

// The prebuilt `alloc` was compiled with unwinding; with panic = "abort" nothing ever unwinds,
// but the linker still wants these two names.
// (Windows MSVC: the CRT's vruntime has __CxxFrameHandler3; these are not wanted there.)
#[cfg(not(windows))]
#[no_mangle]
pub extern "C" fn rust_eh_personality() {}

#[cfg(not(windows))]
#[no_mangle]
pub extern "C" fn _Unwind_Resume() -> ! {
    unsafe { abort() }
}

// ------------------------------------------------------------------ memory

pub struct Malloc;

unsafe impl GlobalAlloc for Malloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if l.align() <= 16 {
            malloc(l.size().max(1)) as *mut u8
        } else {
            let mut p: *mut c_void = core::ptr::null_mut();
            if posix_memalign(&mut p, l.align(), l.size().max(1)) != 0 {
                return core::ptr::null_mut();
            }
            p as *mut u8
        }
    }
    unsafe fn dealloc(&self, p: *mut u8, _l: Layout) {
        #[cfg(windows)]
        if _l.align() > 16 {
            return aligned_free(p as *mut c_void);
        }
        free(p as *mut c_void)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if l.align() <= 16 {
            realloc(p as *mut c_void, new.max(1)) as *mut u8
        } else {
            let q = self.alloc(Layout::from_size_align_unchecked(new, l.align()));
            if !q.is_null() {
                core::ptr::copy_nonoverlapping(p, q, l.size().min(new));
                self.dealloc(p, l);
            }
            q
        }
    }
}

// ------------------------------------------------------------------ text out

fn write_all(fd: c_int, b: &[u8]) {
    write_all_ok(fd, b);
}

/// Writes all of `b`; false when the disk refused some of it (full, quota, I/O error).
fn write_all_ok(fd: c_int, mut b: &[u8]) -> bool {
    while !b.is_empty() {
        let n = unsafe { write(fd, b.as_ptr() as *const c_void, b.len()) };
        if n <= 0 {
            return false;
        }
        b = &b[n as usize..];
    }
    true
}

pub fn print(s: &str) {
    write_all(1, s.as_bytes())
}

pub fn eprint(s: &str) {
    write_all(2, s.as_bytes())
}

#[macro_export]
macro_rules! say {
    ($($t:tt)*) => {{ $crate::sys::print(&alloc::format!($($t)*)); $crate::sys::print("\n"); }};
}
#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => {{ $crate::sys::eprint(&alloc::format!($($t)*)); $crate::sys::eprint("\n"); }};
}

// ------------------------------------------------------------------ misc

/// A NUL-terminated copy of `s`.
pub fn cstr(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

pub unsafe fn from_cstr(p: *const c_char) -> String {
    let mut v = Vec::new();
    let mut i = 0;
    while *p.add(i) != 0 {
        v.push(*p.add(i) as u8);
        i += 1;
    }
    String::from_utf8_lossy(&v).into_owned()
}

#[cfg(windows)]
pub fn env(name: &str) -> Option<String> {
    getenv_utf8(name)
}

#[cfg(not(windows))]
pub fn env(name: &str) -> Option<String> {
    let n = cstr(name);
    let p = unsafe { getenv(n.as_ptr() as *const c_char) };
    if p.is_null() {
        None
    } else {
        Some(unsafe { from_cstr(p) })
    }
}

/// Seconds since 1970, with microseconds.
pub fn now() -> f64 {
    let mut tv = TimeVal {
        sec: 0,
        #[cfg(target_os = "macos")]
        usec: 0,
        #[cfg(target_os = "macos")]
        _pad: 0,
        #[cfg(not(target_os = "macos"))]
        usec: 0,
    };
    unsafe { gettimeofday(&mut tv, core::ptr::null_mut()) };
    tv.sec as f64 + tv.usec as f64 / 1e6
}

/// The local date: (month 1-12, day 1-31).
pub fn local_date() -> (u32, u32) {
    // struct tm starts with nine ints (sec, min, hour, mday, mon, year, ...) on both systems.
    let t = now() as i64;
    let mut tm = [0i32; 16];
    let ok = unsafe { !localtime_r(&t, tm.as_mut_ptr() as *mut c_void).is_null() };
    if ok {
        (tm[4] as u32 + 1, tm[3] as u32)
    } else {
        (1, 2)
    }
}

/// The local date and time as vanilla names a screenshot: 2026-11-22_11.20.33.
pub fn local_stamp() -> String {
    let t = now() as i64;
    let mut tm = [0i32; 16];
    let ok = unsafe { !localtime_r(&t, tm.as_mut_ptr() as *mut c_void).is_null() };
    if ok {
        alloc::format!("{:04}-{:02}-{:02}_{:02}.{:02}.{:02}", tm[5] + 1900, tm[4] + 1, tm[3], tm[2], tm[1], tm[0])
    } else {
        String::from("screenshot")
    }
}

/// Makes a folder (an existing one is fine).
pub fn make_dir(path: &str) {
    let p = cstr(path);
    unsafe { mkdir(p.as_ptr() as *const c_char, 0o755) };
}

pub fn now_ms() -> u64 {
    (now() * 1000.0) as u64
}

pub fn sleep_ms(ms: u32) {
    unsafe { usleep(ms.saturating_mul(1000)) };
}

#[cfg(target_os = "macos")]
const O_RDONLY: c_int = 0;
#[cfg(target_os = "macos")]
const O_WRONLY_CREAT_TRUNC: c_int = 0x1 | 0x200 | 0x400;
#[cfg(windows)]
const O_RDONLY: c_int = 0x8000; // _O_RDONLY | _O_BINARY
#[cfg(windows)]
const O_WRONLY_CREAT_TRUNC: c_int = 0x1 | 0x100 | 0x200 | 0x8000; // _O_WRONLY | _O_CREAT | _O_TRUNC | _O_BINARY
#[cfg(not(any(target_os = "macos", windows)))]
const O_RDONLY: c_int = 0;
#[cfg(not(any(target_os = "macos", windows)))]
const O_WRONLY_CREAT_TRUNC: c_int = 0o1 | 0o100 | 0o1000;

pub fn read_file(path: &str) -> Option<Vec<u8>> {
    let p = cstr(path);
    let fd = unsafe { open(p.as_ptr() as *const c_char, O_RDONLY, 0) };
    if fd < 0 {
        return None;
    }
    let mut out = Vec::new();
    let mut buf = [0u8; 16384];
    loop {
        let n = unsafe { read(fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n < 0 {
            unsafe { close(fd) };
            return None;
        }
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    unsafe { close(fd) };
    Some(out)
}

/// A file kept open for reading pieces of it (the sound pack).
pub struct File(c_int);

impl File {
    pub fn open(path: &str) -> Option<File> {
        let p = cstr(path);
        let fd = unsafe { open(p.as_ptr() as *const c_char, O_RDONLY, 0) };
        if fd < 0 {
            None
        } else {
            Some(File(fd))
        }
    }

    /// `n` bytes at `offset`.
    pub fn read_at(&self, offset: u64, n: usize) -> Option<Vec<u8>> {
        if unsafe { lseek(self.0, offset as i64, 0) } < 0 {
            return None;
        }
        let mut out = alloc::vec![0u8; n];
        let mut got = 0;
        while got < n {
            let r = unsafe { read(self.0, out[got..].as_mut_ptr() as *mut c_void, n - got) };
            if r <= 0 {
                return None;
            }
            got += r as usize;
        }
        Some(out)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        unsafe { close(self.0) };
    }
}

/// Opens `path` for writing (created or emptied, readable by its owner only).
pub fn create_private(path: &str) -> Option<c_int> {
    let p = cstr(path);
    let fd = unsafe { open(p.as_ptr() as *const c_char, O_WRONLY_CREAT_TRUNC, 0o600) };
    if fd < 0 {
        None
    } else {
        Some(fd)
    }
}

pub fn write_fd(fd: c_int, data: &[u8]) {
    write_all(fd, data)
}

pub fn close_fd(fd: c_int) {
    unsafe { close(fd) };
}

pub fn rename_file(from: &str, to: &str) -> bool {
    let (a, b) = (cstr(from), cstr(to));
    unsafe { rename(a.as_ptr() as *const c_char, b.as_ptr() as *const c_char) == 0 }
}

/// The first `n` bytes of a file (fewer if it is shorter).
pub fn read_head(path: &str, n: usize) -> Option<Vec<u8>> {
    let p = cstr(path);
    let fd = unsafe { open(p.as_ptr() as *const c_char, O_RDONLY, 0) };
    if fd < 0 {
        return None;
    }
    let mut out = alloc::vec![0u8; n];
    let r = unsafe { read(fd, out.as_mut_ptr() as *mut c_void, n) };
    unsafe { close(fd) };
    out.truncate(r.max(0) as usize);
    Some(out)
}

/// Util.getPlatform().openUri: an http(s) URL in the system's browser (`open` on macOS,
/// `xdg-open` elsewhere), quoted for the shell.
pub fn open_url(url: &str) -> bool {
    if !(url.starts_with("http://") || url.starts_with("https://")) || url.bytes().any(|b| b < 0x20) {
        return false;
    }
    #[cfg(windows)]
    {
        // ShellExecuteW "open": no shell, so nothing to quote.
        shell_open(url)
    }
    #[cfg(not(windows))]
    {
        let quoted = url.replace('\'', "'\\''");
        #[cfg(target_os = "macos")]
        let cmd = alloc::format!("/usr/bin/open '{}' >/dev/null 2>&1 &", quoted);
        #[cfg(not(target_os = "macos"))]
        let cmd = alloc::format!("xdg-open '{}' >/dev/null 2>&1 &", quoted);
        let c = cstr(&cmd);
        unsafe { system(c.as_ptr() as *const c_char) == 0 }
    }
}

/// `n` random bytes from /dev/urandom (Windows: RtlGenRandom, the system's generator).
#[cfg(windows)]
pub fn read_urandom(n: usize) -> Option<Vec<u8>> {
    let mut out = alloc::vec![0u8; n];
    if random_bytes(&mut out) {
        Some(out)
    } else {
        None
    }
}

/// `n` random bytes from /dev/urandom.
#[cfg(not(windows))]
pub fn read_urandom(n: usize) -> Option<Vec<u8>> {
    let p = cstr("/dev/urandom");
    let fd = unsafe { open(p.as_ptr() as *const c_char, O_RDONLY, 0) };
    if fd < 0 {
        return None;
    }
    let mut out = alloc::vec![0u8; n];
    let mut got = 0;
    while got < n {
        let r = unsafe { read(fd, out[got..].as_mut_ptr() as *mut c_void, n - got) };
        if r <= 0 {
            unsafe { close(fd) };
            return None;
        }
        got += r as usize;
    }
    unsafe { close(fd) };
    Some(out)
}

/// Writes `data` to `path`, readable by its owner only (0600).
pub fn write_file_private(path: &str, data: &[u8]) -> bool {
    write_file_mode(path, data, 0o600)
}

/// Writes `data` to `path` with the usual file mode (0644), as a screenshot is saved.
pub fn write_file_readable(path: &str, data: &[u8]) -> bool {
    write_file_mode(path, data, 0o644)
}

/// Writes `data` to `path` with the given file mode (the bits a new file gets, before the umask).
pub fn write_file_mode(path: &str, data: &[u8], mode: i32) -> bool {
    let p = cstr(path);
    let fd = unsafe { open(p.as_ptr() as *const c_char, O_WRONLY_CREAT_TRUNC, mode) };
    if fd < 0 {
        return false;
    }
    let ok = write_all_ok(fd, data);
    // close can report the delayed write error of a full disk too.
    let closed = unsafe { close(fd) } == 0;
    ok && closed
}
