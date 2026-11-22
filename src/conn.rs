//! A Minecraft connection: length-prefixed frames, with zlib compression once the server
//! turns it on. (Encryption, for online-mode servers, plugs in here: see crypto.rs.)

use crate::net::Tcp;
use crate::wire::{peek_varint, varint_len, In, Out};
use crate::zlib;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub struct Packet {
    pub id: i32,
    pub body: Vec<u8>,
}

impl Packet {
    pub fn reader(&self) -> In<'_> {
        In::new(&self.body)
    }
}

pub struct Conn {
    tcp: Tcp,
    rx: Vec<u8>,
    /// Bytes at the front of `rx` already handed out.
    used: usize,
    /// The compression threshold, once the server has set one.
    pub threshold: Option<i32>,
    pub cipher: Option<crate::crypto::Cfb8Pair>,
    /// The receive buffer, kept (zeroing 64 KB on every call cost more than the call).
    buf: Vec<u8>,
}

/// The longest frame the protocol allows (2^21 - 1 bytes).
const MAX_FRAME: usize = 2_097_151;

impl Conn {
    pub fn open(host: &str, port: u16) -> Result<Conn, String> {
        Ok(Conn { tcp: Tcp::connect(host, port)?, rx: Vec::new(), used: 0, threshold: None, cipher: None, buf: alloc::vec![0u8; 65536] })
    }

    pub fn send(&mut self, id: i32, body: &Out) -> Result<(), String> {
        let mut data = Out::new();
        data.varint(id).bytes(&body.b);
        let mut frame = Out::new();
        match self.threshold {
            None => {
                frame.varint(data.b.len() as i32).bytes(&data.b);
            }
            Some(t) if (data.b.len() as i32) < t || t < 0 => {
                frame.varint(data.b.len() as i32 + 1).varint(0).bytes(&data.b);
            }
            Some(_) => {
                let z = zlib::store(&data.b);
                let n = data.b.len() as i32;
                frame.varint((varint_len(n) + z.len()) as i32).varint(n).bytes(&z);
            }
        }
        if let Some(c) = self.cipher.as_mut() {
            c.encrypt(&mut frame.b);
        }
        self.tcp.send_all(&frame.b)
    }

    /// The next packet, or `Ok(None)` if none arrives within `timeout_ms`.
    pub fn recv(&mut self, timeout_ms: i32) -> Result<Option<Packet>, String> {
        let deadline = crate::sys::now_ms() + timeout_ms.max(0) as u64;
        let mut polled = false;
        loop {
            if let Some(p) = self.take_frame()? {
                return Ok(Some(p));
            }
            let left = deadline.saturating_sub(crate::sys::now_ms()) as i32;
            if left <= 0 && polled {
                return Ok(None);
            }
            polled = true;
            let n = self.tcp.recv(&mut self.buf, left.max(0))?;
            if n == 0 {
                if left <= 0 {
                    return Ok(None);
                }
                continue;
            }
            if let Some(c) = self.cipher.as_mut() {
                c.decrypt(&mut self.buf[..n]);
            }
            if self.used > 0 && self.used == self.rx.len() {
                self.rx.clear();
                self.used = 0;
            } else if self.used > 1 << 20 {
                self.rx.drain(..self.used);
                self.used = 0;
            }
            self.rx.extend_from_slice(&self.buf[..n]);
        }
    }

    fn take_frame(&mut self) -> Result<Option<Packet>, String> {
        let avail = &self.rx[self.used..];
        let (len, lsize) = match peek_varint(avail) {
            None => return Ok(None),
            Some(Err(())) => return Err(String::from("bad frame length")),
            Some(Ok(v)) => v,
        };
        if len < 0 || len as usize > MAX_FRAME {
            return Err(format!("frame of {} bytes", len));
        }
        if avail.len() < lsize + len as usize {
            return Ok(None);
        }
        let frame = &avail[lsize..lsize + len as usize];
        self.used += lsize + len as usize;
        let data: Vec<u8> = if self.threshold.is_some() {
            let mut r = In::new(frame);
            let n = r.varint().ok_or("bad compressed frame")?;
            if n == 0 {
                r.rest().to_vec()
            } else {
                if n < 0 || n as usize > 8 << 20 {
                    return Err(format!("compressed packet of {} bytes", n));
                }
                zlib::inflate(r.rest(), n as usize).ok_or("corrupt compressed packet")?
            }
        } else {
            frame.to_vec()
        };
        let mut r = In::new(&data);
        let id = r.varint().ok_or("packet without an id")?;
        let body = r.rest().to_vec();
        Ok(Some(Packet { id, body }))
    }
}
