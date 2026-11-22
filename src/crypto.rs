//! What an online-mode join needs, written out so the program has no dependencies:
//! AES-128 in CFB8 mode (the connection cipher), RSA PKCS#1 v1.5 encryption with the
//! server's key, SHA-1 and Minecraft's signed-hex "server hash".

use alloc::string::String;
use alloc::vec::Vec;

// ------------------------------------------------------------------ AES-128

const SBOX: [u8; 256] = {
    // Built at compile time from the AES definition (multiplicative inverse + affine map).
    let mut s = [0u8; 256];
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        // p *= 3
        p = p ^ (p << 1) ^ (if p & 0x80 != 0 { 0x1B } else { 0 });
        // q /= 3
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        s[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    s[0] = 0x63;
    s
};

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1B } else { 0 }
}

pub struct Aes128 {
    rk: [[u8; 16]; 11],
}

impl Aes128 {
    pub fn new(key: &[u8; 16]) -> Aes128 {
        let mut w = [[0u8; 4]; 44];
        for i in 0..4 {
            w[i].copy_from_slice(&key[4 * i..4 * i + 4]);
        }
        let mut rcon: u8 = 1;
        for i in 4..44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
                rcon = xtime(rcon);
            }
            for j in 0..4 {
                w[i][j] = w[i - 4][j] ^ t[j];
            }
        }
        let mut rk = [[0u8; 16]; 11];
        for r in 0..11 {
            for c in 0..4 {
                rk[r][4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        Aes128 { rk }
    }

    pub fn encrypt_block(&self, b: &mut [u8; 16]) {
        for i in 0..16 {
            b[i] ^= self.rk[0][i];
        }
        for round in 1..11 {
            for x in b.iter_mut() {
                *x = SBOX[*x as usize];
            }
            // ShiftRows (column-major state: byte (row r, col c) at 4c + r)
            let t = *b;
            for c in 0..4 {
                for r in 0..4 {
                    b[4 * c + r] = t[4 * ((c + r) % 4) + r];
                }
            }
            if round != 10 {
                for c in 0..4 {
                    let a = [b[4 * c], b[4 * c + 1], b[4 * c + 2], b[4 * c + 3]];
                    let all = a[0] ^ a[1] ^ a[2] ^ a[3];
                    for r in 0..4 {
                        b[4 * c + r] = a[r] ^ all ^ xtime(a[r] ^ a[(r + 1) % 4]);
                    }
                }
            }
            for i in 0..16 {
                b[i] ^= self.rk[round][i];
            }
        }
    }
}

/// AES-128/CFB8, as Minecraft uses it: key and IV are both the shared secret.
pub struct Cfb8 {
    aes: Aes128,
    iv: [u8; 16],
}

impl Cfb8 {
    pub fn new(secret: &[u8; 16]) -> Cfb8 {
        Cfb8 { aes: Aes128::new(secret), iv: *secret }
    }
    fn step(&mut self, input: u8, decrypting: bool) -> u8 {
        let mut k = self.iv;
        self.aes.encrypt_block(&mut k);
        let out = input ^ k[0];
        let c = if decrypting { input } else { out };
        self.iv.copy_within(1.., 0);
        self.iv[15] = c;
        out
    }
}

pub struct Cfb8Pair {
    enc: Cfb8,
    dec: Cfb8,
}

impl Cfb8Pair {
    pub fn new(secret: &[u8; 16]) -> Cfb8Pair {
        Cfb8Pair { enc: Cfb8::new(secret), dec: Cfb8::new(secret) }
    }
    pub fn encrypt(&mut self, b: &mut [u8]) {
        for x in b {
            *x = self.enc.step(*x, false);
        }
    }
    pub fn decrypt(&mut self, b: &mut [u8]) {
        for x in b {
            *x = self.dec.step(*x, true);
        }
    }
}

// ------------------------------------------------------------------ SHA-1

pub struct Sha1 {
    h: [u32; 5],
    buf: Vec<u8>,
    len: u64,
}

impl Sha1 {
    pub fn new() -> Sha1 {
        Sha1 { h: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0], buf: Vec::new(), len: 0 }
    }
    pub fn update(&mut self, d: &[u8]) {
        self.len += d.len() as u64;
        self.buf.extend_from_slice(d);
        let full = self.buf.len() / 64 * 64;
        let blocks: Vec<u8> = self.buf.drain(..full).collect();
        for b in blocks.chunks(64) {
            self.block(b);
        }
    }
    fn block(&mut self, b: &[u8]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut bb, mut c, mut d, mut e] = self.h;
        for i in 0..80 {
            let (f, k) = match i / 20 {
                0 => ((bb & c) | (!bb & d), 0x5A827999),
                1 => (bb ^ c ^ d, 0x6ED9EBA1),
                2 => ((bb & c) | (bb & d) | (c & d), 0x8F1BBCDC),
                _ => (bb ^ c ^ d, 0xCA62C1D6u32),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(w[i]);
            e = d;
            d = c;
            c = bb.rotate_left(30);
            bb = a;
            a = t;
        }
        for (h, v) in self.h.iter_mut().zip([a, bb, c, d, e]) {
            *h = h.wrapping_add(v);
        }
    }
    pub fn finish(mut self) -> [u8; 20] {
        let bits = self.len.wrapping_mul(8);
        let mut pad = alloc::vec![0x80u8];
        while (self.buf.len() + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bits.to_be_bytes());
        let l = self.len;
        self.update(&pad);
        self.len = l;
        let mut out = [0u8; 20];
        for i in 0..5 {
            out[4 * i..4 * i + 4].copy_from_slice(&self.h[i].to_be_bytes());
        }
        out
    }
}

/// Minecraft's server hash: SHA-1 read as a signed big-endian number, in hex, no leading zeros.
pub fn server_hash(server_id: &[u8], secret: &[u8], public_key: &[u8]) -> String {
    let mut s = Sha1::new();
    s.update(server_id);
    s.update(secret);
    s.update(public_key);
    let mut d = s.finish();
    let negative = d[0] & 0x80 != 0;
    if negative {
        // two's complement
        let mut carry = true;
        for x in d.iter_mut().rev() {
            *x = !*x;
            if carry {
                let (v, c) = x.overflowing_add(1);
                *x = v;
                carry = c;
            }
        }
    }
    let mut hex = String::new();
    for b in d {
        hex.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        hex.push(char::from_digit((b & 15) as u32, 16).unwrap());
    }
    let t = hex.trim_start_matches('0');
    let t = if t.is_empty() { "0" } else { t };
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(t);
    out
}

// ------------------------------------------------------------------ RSA

/// Little-endian 32-bit limbs.
#[derive(Clone)]
struct Big(Vec<u32>);

impl Big {
    fn from_be(b: &[u8]) -> Big {
        let mut v = Vec::new();
        let mut i = b.len();
        while i > 0 {
            let lo = i.saturating_sub(4);
            let mut x = 0u32;
            for &c in &b[lo..i] {
                x = x << 8 | c as u32;
            }
            v.push(x);
            i = lo;
        }
        let mut r = Big(v);
        r.trim();
        r
    }
    fn to_be(&self, len: usize) -> Vec<u8> {
        let mut out = alloc::vec![0u8; len];
        for (i, limb) in self.0.iter().enumerate() {
            for k in 0..4 {
                let pos = i * 4 + k;
                if pos < len {
                    out[len - 1 - pos] = (limb >> (8 * k)) as u8;
                }
            }
        }
        out
    }
    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
    fn bits(&self) -> usize {
        match self.0.last() {
            None => 0,
            Some(&t) => (self.0.len() - 1) * 32 + (32 - t.leading_zeros() as usize),
        }
    }
    fn bit(&self, i: usize) -> bool {
        self.0.get(i / 32).map_or(false, |l| l >> (i % 32) & 1 == 1)
    }
    fn mul(&self, o: &Big) -> Big {
        let mut r = alloc::vec![0u32; self.0.len() + o.0.len()];
        for (i, &a) in self.0.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in o.0.iter().enumerate() {
                let t = r[i + j] as u64 + a as u64 * b as u64 + carry;
                r[i + j] = t as u32;
                carry = t >> 32;
            }
            r[i + o.0.len()] = carry as u32;
        }
        let mut b = Big(r);
        b.trim();
        b
    }
    fn ge(&self, o: &Big) -> bool {
        if self.0.len() != o.0.len() {
            return self.0.len() > o.0.len();
        }
        for i in (0..self.0.len()).rev() {
            if self.0[i] != o.0[i] {
                return self.0[i] > o.0[i];
            }
        }
        true
    }
    fn sub(&mut self, o: &Big) {
        let mut borrow = 0i64;
        for i in 0..self.0.len() {
            let t = self.0[i] as i64 - o.0.get(i).copied().unwrap_or(0) as i64 - borrow;
            if t < 0 {
                self.0[i] = (t + (1i64 << 32)) as u32;
                borrow = 1;
            } else {
                self.0[i] = t as u32;
                borrow = 0;
            }
        }
        self.trim();
    }
    fn shl1_or(&mut self, bit: bool) {
        let mut carry = bit as u32;
        for l in self.0.iter_mut() {
            let n = *l >> 31;
            *l = *l << 1 | carry;
            carry = n;
        }
        if carry != 0 {
            self.0.push(carry);
        }
    }
    fn rem(&self, m: &Big) -> Big {
        let mut r = Big(Vec::new());
        for i in (0..self.bits()).rev() {
            r.shl1_or(self.bit(i));
            if r.ge(m) {
                r.sub(m);
            }
        }
        r
    }
    fn modpow(&self, e: &Big, m: &Big) -> Big {
        let mut result = Big(alloc::vec![1]);
        let base = self.rem(m);
        for i in (0..e.bits()).rev() {
            result = result.mul(&result).rem(m);
            if e.bit(i) {
                result = result.mul(&base).rem(m);
            }
        }
        result
    }
}

/// One DER element at the front of `d`: (tag, contents, rest).
fn der(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *d.first()?;
    let l0 = *d.get(1)? as usize;
    let (len, hdr) = if l0 < 0x80 {
        (l0, 2)
    } else {
        let n = l0 & 0x7F;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = len << 8 | *d.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let body = d.get(hdr..hdr + len)?;
    Some((tag, body, &d[hdr + len..]))
}

pub struct RsaKey {
    n: Big,
    e: Big,
    size: usize,
}

impl RsaKey {
    /// Reads an X.509 SubjectPublicKeyInfo, as the server sends it.
    pub fn from_spki(spki: &[u8]) -> Option<RsaKey> {
        let (_, seq, _) = der(spki)?; // SEQUENCE
        let (_, _alg, rest) = der(seq)?; // AlgorithmIdentifier
        let (tag, bits, _) = der(rest)?; // BIT STRING
        if tag != 0x03 || bits.first() != Some(&0) {
            return None;
        }
        let (_, inner, _) = der(&bits[1..])?; // RSAPublicKey SEQUENCE
        let (_, n, rest) = der(inner)?;
        let (_, e, _) = der(rest)?;
        let n = Big::from_be(n);
        let size = (n.bits() + 7) / 8;
        Some(RsaKey { n, e: Big::from_be(e), size })
    }

    /// PKCS#1 v1.5 encryption (block type 2) of `msg`, with `random` supplying the padding.
    pub fn encrypt(&self, msg: &[u8], random: &mut dyn FnMut() -> u8) -> Option<Vec<u8>> {
        if msg.len() + 11 > self.size {
            return None;
        }
        let mut em = alloc::vec![0u8; self.size];
        em[1] = 2;
        let pad_end = self.size - msg.len() - 1;
        for x in &mut em[2..pad_end] {
            let mut r = random();
            while r == 0 {
                r = random();
            }
            *x = r;
        }
        em[pad_end] = 0;
        em[pad_end + 1..].copy_from_slice(msg);
        let c = Big::from_be(&em).modpow(&self.e, &self.n);
        Some(c.to_be(self.size))
    }
}
