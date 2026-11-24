//! The voice chat's UDP socket: one connected datagram socket for the whole session (the
//! server binds a connection to the first address it hears from, so the socket must not change),
//! sent on and read without blocking.

use crate::sys::{self, AddrInfo, PollFd, POLLIN};
use alloc::format;
use alloc::string::String;
use core::ffi::{c_char, c_void};

const SOCK_DGRAM: i32 = 2;

pub struct Udp {
    fd: i32,
}

impl Udp {
    /// A socket connected to `host:port` (the first address the resolver gives that takes a
    /// UDP socket; connecting a datagram socket only sets the peer).
    pub fn connect(host: &str, port: u16) -> Result<Udp, String> {
        let h = sys::cstr(host);
        let p = sys::cstr(&format!("{}", port));
        let mut hints: AddrInfo = unsafe { core::mem::zeroed() };
        hints.ai_socktype = SOCK_DGRAM;
        let mut res: *mut AddrInfo = core::ptr::null_mut();
        let rc = unsafe { sys::getaddrinfo(h.as_ptr() as *const c_char, p.as_ptr() as *const c_char, &hints, &mut res) };
        if rc != 0 || res.is_null() {
            return Err(format!("cannot resolve {} (error {})", host, rc));
        }
        let mut ai = res;
        let mut found = None;
        while !ai.is_null() {
            let a = unsafe { &*ai };
            let fd = unsafe { sys::socket(a.ai_family, a.ai_socktype, a.ai_protocol) };
            if fd >= 0 {
                if unsafe { sys::connect(fd, a.ai_addr, a.ai_addrlen) } == 0 {
                    found = Some(fd);
                    break;
                }
                unsafe { sys::sock_close(fd) };
            }
            ai = a.ai_next;
        }
        unsafe { sys::freeaddrinfo(res) };
        match found {
            Some(fd) => Ok(Udp { fd }),
            None => Err(format!("cannot reach the voice server {}:{}", host, port)),
        }
    }

    /// Sends one datagram; false when the system refused it (a full buffer drops audio, as
    /// UDP does).
    pub fn send(&mut self, data: &[u8]) -> bool {
        unsafe { sys::send(self.fd, data.as_ptr() as *const c_void, data.len(), 0) == data.len() as isize }
    }

    /// The next datagram waiting, if any (never blocks).
    pub fn recv(&mut self, buf: &mut [u8]) -> Option<usize> {
        let mut p = PollFd { fd: self.fd, events: POLLIN, revents: 0 };
        if unsafe { sys::poll(&mut p, 1, 0) } <= 0 || p.revents & POLLIN == 0 {
            return None;
        }
        let n = unsafe { sys::recv(self.fd, buf.as_mut_ptr() as *mut c_void, buf.len(), 0) };
        if n > 0 {
            Some(n as usize)
        } else {
            None
        }
    }
}

impl Drop for Udp {
    fn drop(&mut self) {
        unsafe { sys::sock_close(self.fd) };
    }
}
