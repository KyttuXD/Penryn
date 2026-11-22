//! Plain HTTP/1.0, to talk to penryn-relay (tools/relay.py) on a newer machine.
//!
//! Mac OS X 10.6 cannot make the TLS 1.2 connections Microsoft, Xbox Live and Mojang require,
//! and this program carries no TLS. The relay does those HTTPS calls; this side only ever
//! talks to the relay, on the local network.

use crate::net::Tcp;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub struct Response {
    pub status: u32,
    pub body: Vec<u8>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub fn get(host: &str, port: u16, path: &str, timeout_ms: i32) -> Result<Response, String> {
    request(host, port, "GET", path, &[], timeout_ms)
}

pub fn post(host: &str, port: u16, path: &str, body: &[u8], timeout_ms: i32) -> Result<Response, String> {
    request(host, port, "POST", path, body, timeout_ms)
}

fn request(host: &str, port: u16, method: &str, path: &str, body: &[u8], timeout_ms: i32) -> Result<Response, String> {
    let mut t = Tcp::connect(host, port)?;
    let head = format!(
        "{} {} HTTP/1.0\r\nHost: {}:{}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        method,
        path,
        host,
        port,
        body.len()
    );
    t.send_all(head.as_bytes())?;
    t.send_all(body)?;
    let mut data = Vec::new();
    let deadline = crate::sys::now_ms() + timeout_ms as u64;
    let mut buf = [0u8; 8192];
    loop {
        let left = deadline.saturating_sub(crate::sys::now_ms()) as i32;
        if left <= 0 {
            return Err(String::from("the relay did not answer in time"));
        }
        match t.recv(&mut buf, left) {
            Ok(0) => continue,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(_) => break, // closed: the response is complete
        }
    }
    let split = data.windows(4).position(|w| w == b"\r\n\r\n").ok_or("bad answer from the relay")?;
    let head = String::from_utf8_lossy(&data[..split]).into_owned();
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    Ok(Response { status, body: data[split + 4..].to_vec() })
}

/// GETs `path` into the file `out` without holding it in memory (the sound pack). Written to
/// `out.part` first and renamed, so a broken download never looks complete. `progress` gets
/// (bytes so far, total).
pub fn get_to_file(host: &str, port: u16, path: &str, out: &str, progress: &mut dyn FnMut(u64, u64)) -> Result<u32, String> {
    let mut t = Tcp::connect(host, port)?;
    let head = format!("GET {} HTTP/1.0\r\nHost: {}:{}\r\nConnection: close\r\n\r\n", path, host, port);
    t.send_all(head.as_bytes())?;
    let mut buf = alloc::vec![0u8; 1 << 16];
    let mut pending: Vec<u8> = Vec::new();
    // The head first.
    let (status, total) = loop {
        match t.recv(&mut buf, 60000) {
            Ok(0) => return Err(String::from("the relay did not answer in time")),
            Ok(n) => pending.extend_from_slice(&buf[..n]),
            Err(_) => return Err(String::from("the relay closed the connection")),
        }
        if let Some(split) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
            let h = String::from_utf8_lossy(&pending[..split]).into_owned();
            let status: u32 = h.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let total: u64 = h
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.trim().eq_ignore_ascii_case("content-length") { v.trim().parse().ok() } else { None }
                })
                .unwrap_or(0);
            pending.drain(..split + 4);
            break (status, total);
        }
    };
    if status != 200 {
        return Ok(status);
    }
    let part = format!("{}.part", out);
    let fd = crate::sys::create_private(&part).ok_or_else(|| format!("cannot write {}", part))?;
    let mut got = pending.len() as u64;
    crate::sys::write_fd(fd, &pending);
    let mut last = crate::sys::now_ms();
    loop {
        match t.recv(&mut buf, 60000) {
            Ok(0) => {
                crate::sys::close_fd(fd);
                return Err(String::from("the download stalled"));
            }
            Ok(n) => {
                crate::sys::write_fd(fd, &buf[..n]);
                got += n as u64;
                if crate::sys::now_ms() > last + 500 {
                    last = crate::sys::now_ms();
                    progress(got, total);
                }
            }
            Err(_) => break,
        }
    }
    crate::sys::close_fd(fd);
    if total != 0 && got != total {
        return Err(format!("the download broke off ({} of {} bytes)", got, total));
    }
    progress(got, total);
    if !crate::sys::rename_file(&part, out) {
        return Err(format!("cannot rename {} to {}", part, out));
    }
    Ok(200)
}

/// Splits `host[:port]`.
pub fn host_port(s: &str, default: u16) -> (String, u16) {
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (String::from(h), p.parse().unwrap_or(default)),
        _ => (String::from(s), default),
    }
}
