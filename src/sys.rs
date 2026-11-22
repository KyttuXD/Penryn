//! The C library, called directly. Everything here exists both in glibc and in Mac OS X 10.6's
//! libSystem; nothing newer is used (no `clock_gettime`, no thread-locals), so that the
//! Mac build loads on 10.6. Every symbol used here must also be listed in the libSystem
//! stub in `scripts/build-mac-10.6.sh`.

use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::ffi::{c_char, c_int, c_void};

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
#[cfg(not(target_os = "macos"))]
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
struct TimeVal {
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
}

// The prebuilt `alloc` was compiled with unwinding; with panic = "abort" nothing ever unwinds,
// but the linker still wants these two names.
#[no_mangle]
pub extern "C" fn rust_eh_personality() {}

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

fn write_all(fd: c_int, mut b: &[u8]) {
    while !b.is_empty() {
        let n = unsafe { write(fd, b.as_ptr() as *const c_void, b.len()) };
        if n <= 0 {
            return;
        }
        b = &b[n as usize..];
    }
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
#[cfg(not(target_os = "macos"))]
const O_RDONLY: c_int = 0;
#[cfg(not(target_os = "macos"))]
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

/// `n` random bytes from /dev/urandom.
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
    let p = cstr(path);
    let fd = unsafe { open(p.as_ptr() as *const c_char, O_WRONLY_CREAT_TRUNC, 0o600) };
    if fd < 0 {
        return false;
    }
    write_all(fd, data);
    unsafe { close(fd) };
    true
}
