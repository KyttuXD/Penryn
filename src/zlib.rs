//! zlib (RFC 1950/1951): inflate for the server's compressed packets, and a "stored" deflate
//! (no compression, valid zlib) for the rare packet we send above the compression threshold.
//! The inflater is VoxelRust's (`src/util/image/png.rs`, MIT, same author).

use alloc::vec::Vec;

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    cnt: u32,
}

impl<'a> Bits<'a> {
    fn bits(&mut self, n: u32) -> Option<u32> {
        while self.cnt < n {
            let byte = *self.data.get(self.pos)?;
            self.pos += 1;
            self.buf |= (byte as u32) << self.cnt;
            self.cnt += 8;
        }
        let v = self.buf & ((1u32 << n) - 1);
        self.buf >>= n;
        self.cnt -= n;
        Some(v)
    }
    fn align(&mut self) {
        self.buf = 0;
        self.cnt = 0;
    }
}

struct Huffman {
    count: [u16; 16],
    symbol: [u16; 288],
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Huffman> {
        let mut h = Huffman { count: [0; 16], symbol: [0; 288] };
        if lengths.len() > h.symbol.len() {
            return None;
        }
        for &l in lengths {
            h.count[(l & 15) as usize] += 1;
        }
        let mut left = 1i32;
        for len in 1..16 {
            left = (left << 1) - h.count[len] as i32;
            if left < 0 {
                return None;
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + h.count[len];
        }
        for (sym, &l) in lengths.iter().enumerate() {
            let l = (l & 15) as usize;
            if l != 0 {
                h.symbol[offs[l] as usize] = sym as u16;
                offs[l] += 1;
            }
        }
        Some(h)
    }

    fn decode(&self, br: &mut Bits) -> Option<usize> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= br.bits(1)? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return self.symbol.get((index + code - first) as usize).map(|&s| s as usize);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] =
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CLEN_ORDER: [u8; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

fn fixed_codes() -> Option<(Huffman, Huffman)> {
    let mut l = [0u8; 288];
    for (i, v) in l.iter_mut().enumerate() {
        *v = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Some((Huffman::new(&l)?, Huffman::new(&[5u8; 30])?))
}

fn dynamic_codes(br: &mut Bits) -> Option<(Huffman, Huffman)> {
    let nlen = br.bits(5)? as usize + 257;
    let ndist = br.bits(5)? as usize + 1;
    let ncode = br.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return None;
    }
    let mut cl = [0u8; 19];
    for &slot in CLEN_ORDER.iter().take(ncode) {
        cl[slot as usize] = br.bits(3)? as u8;
    }
    let clen = Huffman::new(&cl)?;
    let mut lengths = [0u8; 286 + 30];
    let total = nlen + ndist;
    let mut i = 0;
    while i < total {
        let sym = clen.decode(br)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
            continue;
        }
        let (value, repeat) = match sym {
            16 => {
                if i == 0 {
                    return None;
                }
                (lengths[i - 1], 3 + br.bits(2)? as usize)
            }
            17 => (0, 3 + br.bits(3)? as usize),
            _ => (0, 11 + br.bits(7)? as usize),
        };
        if i + repeat > total {
            return None;
        }
        for l in &mut lengths[i..i + repeat] {
            *l = value;
        }
        i += repeat;
    }
    if lengths[256] == 0 {
        return None;
    }
    Some((Huffman::new(&lengths[..nlen])?, Huffman::new(&lengths[nlen..total])?))
}

fn inflate_block(br: &mut Bits, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman, limit: usize) -> Option<()> {
    loop {
        let sym = lit.decode(br)?;
        if sym < 256 {
            if out.len() >= limit {
                return None;
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Some(());
        }
        let li = sym - 257;
        let len = *LEN_BASE.get(li)? as usize + br.bits(LEN_EXTRA[li] as u32)? as usize;
        let di = dist.decode(br)?;
        let d = *DIST_BASE.get(di)? as usize + br.bits(DIST_EXTRA[di] as u32)? as usize;
        if d > out.len() || len > limit - out.len() {
            return None;
        }
        let from = out.len() - d;
        for k in 0..len {
            let b = out[from + k];
            out.push(b);
        }
    }
}

/// Decompresses a zlib stream whose output must be exactly `expect` bytes
/// (Minecraft sends the uncompressed length first).
pub fn inflate(zlib: &[u8], expect: usize) -> Option<Vec<u8>> {
    if zlib.len() < 6 {
        return None;
    }
    let (cmf, flg) = (zlib[0] as u32, zlib[1] as u32);
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || (cmf << 8 | flg) % 31 != 0 || flg & 0x20 != 0 {
        return None;
    }
    let mut br = Bits { data: zlib, pos: 2, buf: 0, cnt: 0 };
    let mut out: Vec<u8> = Vec::with_capacity(expect);
    loop {
        let last = br.bits(1)?;
        match br.bits(2)? {
            0 => {
                br.align();
                let head = zlib.get(br.pos..br.pos.checked_add(4)?)?;
                let len = head[0] as usize | (head[1] as usize) << 8;
                let nlen = head[2] as usize | (head[3] as usize) << 8;
                if len ^ nlen != 0xFFFF {
                    return None;
                }
                br.pos += 4;
                let body = zlib.get(br.pos..br.pos.checked_add(len)?)?;
                if len > expect - out.len() {
                    return None;
                }
                out.extend_from_slice(body);
                br.pos += len;
            }
            1 => {
                let (lit, dist) = fixed_codes()?;
                inflate_block(&mut br, &mut out, &lit, &dist, expect)?;
            }
            2 => {
                let (lit, dist) = dynamic_codes(&mut br)?;
                inflate_block(&mut br, &mut out, &lit, &dist, expect)?;
            }
            _ => return None,
        }
        if last == 1 {
            break;
        }
    }
    if out.len() != expect {
        return None;
    }
    // The Adler-32 trailer is checked when present.
    br.align();
    if let Some(t) = zlib.get(br.pos..br.pos + 4) {
        if u32::from_be_bytes([t[0], t[1], t[2], t[3]]) != adler32(&out) {
            return None;
        }
    }
    Some(out)
}

/// A valid zlib stream of `data` in stored (uncompressed) blocks.
pub fn store(data: &[u8]) -> Vec<u8> {
    let mut z = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 11);
    z.push(0x78);
    z.push(0x01);
    let mut chunks = data.chunks(65535).peekable();
    if chunks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        let n = c.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(c);
    }
    z.extend_from_slice(&adler32(data).to_be_bytes());
    z
}
