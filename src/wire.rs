//! Minecraft's wire types: VarInt, VarLong, strings, UUIDs, big-endian numbers.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Default)]
pub struct Out {
    pub b: Vec<u8>,
}

impl Out {
    pub fn new() -> Out {
        Out { b: Vec::new() }
    }
    pub fn varint(&mut self, v: i32) -> &mut Self {
        let mut u = v as u32;
        loop {
            if u & !0x7F == 0 {
                self.b.push(u as u8);
                return self;
            }
            self.b.push((u & 0x7F | 0x80) as u8);
            u >>= 7;
        }
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.b.push(v);
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(v as u8)
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.b.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.b.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.b.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.b.extend_from_slice(&v.to_bits().to_be_bytes());
        self
    }
    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.b.extend_from_slice(&v.to_bits().to_be_bytes());
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.b.extend_from_slice(v);
        self
    }
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.varint(s.len() as i32);
        self.b.extend_from_slice(s.as_bytes());
        self
    }
}

pub fn varint_len(v: i32) -> usize {
    let mut u = v as u32;
    let mut n = 1;
    while u & !0x7F != 0 {
        u >>= 7;
        n += 1;
    }
    n
}

/// Reads from a byte slice. Every read gives `None` past the end.
pub struct In<'a> {
    pub b: &'a [u8],
    pub pos: usize,
}

impl<'a> In<'a> {
    pub fn new(b: &'a [u8]) -> In<'a> {
        In { b, pos: 0 }
    }
    pub fn left(&self) -> usize {
        self.b.len() - self.pos
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.b[self.pos..]
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.left() < n {
            return None;
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }
    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }
    pub fn u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_be_bytes([s[0], s[1]]))
    }
    pub fn i32(&mut self) -> Option<i32> {
        let s = self.take(4)?;
        Some(i32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub fn i64(&mut self) -> Option<i64> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Some(i64::from_be_bytes(a))
    }
    pub fn f32(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.i32()? as u32))
    }
    pub fn f64(&mut self) -> Option<f64> {
        Some(f64::from_bits(self.i64()? as u64))
    }
    pub fn varint(&mut self) -> Option<i32> {
        let mut v: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            v |= ((b & 0x7F) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v as i32);
            }
        }
        None
    }
    pub fn varlong(&mut self) -> Option<i64> {
        let mut v: u64 = 0;
        for i in 0..10 {
            let b = self.u8()?;
            v |= ((b & 0x7F) as u64) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v as i64);
            }
        }
        None
    }
    pub fn string(&mut self) -> Option<String> {
        let n = self.varint()?;
        if n < 0 {
            return None;
        }
        let s = self.take(n as usize)?;
        Some(String::from_utf8_lossy(s).into_owned())
    }
    pub fn uuid(&mut self) -> Option<[u8; 16]> {
        let s = self.take(16)?;
        let mut a = [0u8; 16];
        a.copy_from_slice(s);
        Some(a)
    }
}

/// Reads a VarInt from the front of `b`: the value and how many bytes it took.
/// `None` if `b` ends before the VarInt does; `Some(Err)` if it is too long.
pub fn peek_varint(b: &[u8]) -> Option<Result<(i32, usize), ()>> {
    let mut v: u32 = 0;
    for i in 0..5 {
        let x = *b.get(i)?;
        v |= ((x & 0x7F) as u32) << (7 * i);
        if x & 0x80 == 0 {
            return Some(Ok((v as i32, i + 1)));
        }
    }
    Some(Err(()))
}

pub fn uuid_text(u: &[u8; 16]) -> String {
    let mut s = String::new();
    for (i, b) in u.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            s.push('-');
        }
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 15) as u32, 16).unwrap());
    }
    s
}
