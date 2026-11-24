//! Blocking TCP over the C library's sockets, with read timeouts through `poll`.

use crate::sys::{self, AddrInfo, PollFd, POLLIN, SOCK_STREAM};
use alloc::format;
use alloc::string::String;
use core::ffi::{c_char, c_void};

pub struct Tcp {
    fd: i32,
}

impl Tcp {
    /// Connects to `host:port`, trying each address the resolver gives.
    pub fn connect(host: &str, port: u16) -> Result<Tcp, String> {
        let h = sys::cstr(host);
        let p = sys::cstr(&format!("{}", port));
        let mut hints: AddrInfo = unsafe { core::mem::zeroed() };
        hints.ai_socktype = SOCK_STREAM;
        let mut res: *mut AddrInfo = core::ptr::null_mut();
        let rc = unsafe {
            sys::getaddrinfo(h.as_ptr() as *const c_char, p.as_ptr() as *const c_char, &hints, &mut res)
        };
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
            Some(fd) => Ok(Tcp { fd }),
            None => Err(format!("cannot connect to {}:{}", host, port)),
        }
    }

    pub fn send_all(&mut self, mut b: &[u8]) -> Result<(), String> {
        while !b.is_empty() {
            let n = unsafe { sys::send(self.fd, b.as_ptr() as *const c_void, b.len(), 0) };
            if n <= 0 {
                return Err(String::from("connection lost while sending"));
            }
            b = &b[n as usize..];
        }
        Ok(())
    }

    /// Waits up to `timeout_ms` for data. `Ok(0)` is a timeout, `Err` a closed connection.
    pub fn recv(&mut self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, String> {
        let mut p = PollFd { fd: self.fd, events: POLLIN, revents: 0 };
        let r = unsafe { sys::poll(&mut p, 1, timeout_ms) };
        if r < 0 {
            return Err(String::from("poll failed"));
        }
        if r == 0 {
            return Ok(0);
        }
        let n = unsafe { sys::recv(self.fd, buf.as_mut_ptr() as *mut c_void, buf.len(), 0) };
        if n <= 0 {
            return Err(String::from("the server closed the connection"));
        }
        Ok(n as usize)
    }
}

impl Drop for Tcp {
    fn drop(&mut self) {
        unsafe { sys::sock_close(self.fd) };
    }
}
