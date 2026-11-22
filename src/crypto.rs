//! What an online-mode join needs, written out so the program has no dependencies:
//! AES-128 in CFB8 mode (the connection cipher), RSA PKCS#1 v1.5 encryption with the
//! server's key, SHA-1 and Minecraft's signed-hex "server hash"; SHA-256 and RSA PKCS#1 v1.5
//! signatures (SHA256withRSA) for secure chat.

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

// ------------------------------------------------------------------ SHA-256

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
    0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
    0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 (FIPS 180-4): what secure chat signs (SHA256withRSA).
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    fill: usize,
    len: u64,
}

impl Sha256 {
    pub fn new() -> Sha256 {
        Sha256 {
            h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: [0; 64],
            fill: 0,
            len: 0,
        }
    }
    pub fn update(&mut self, mut d: &[u8]) {
        self.len += d.len() as u64;
        while !d.is_empty() {
            let n = (64 - self.fill).min(d.len());
            self.buf[self.fill..self.fill + n].copy_from_slice(&d[..n]);
            self.fill += n;
            d = &d[n..];
            if self.fill == 64 {
                let b = self.buf;
                self.block(&b);
                self.fill = 0;
            }
        }
    }
    fn block(&mut self, b: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut bb, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & bb) ^ (a & c) ^ (bb & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = bb;
            bb = a;
            a = t1.wrapping_add(t2);
        }
        for (x, v) in self.h.iter_mut().zip([a, bb, c, d, e, f, g, h]) {
            *x = x.wrapping_add(v);
        }
    }
    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.len.wrapping_mul(8);
        let l = self.len;
        self.update(&[0x80]);
        while self.fill != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        self.len = l;
        let mut out = [0u8; 32];
        for i in 0..8 {
            out[4 * i..4 * i + 4].copy_from_slice(&self.h[i].to_be_bytes());
        }
        out
    }
}

pub fn sha256(d: &[u8]) -> [u8; 32] {
    let mut s = Sha256::new();
    s.update(d);
    s.finish()
}

// ------------------------------------------------------------------ RSA signatures

impl Big {
    fn add(&self, o: &Big) -> Big {
        let n = self.0.len().max(o.0.len());
        let mut r = Vec::with_capacity(n + 1);
        let mut carry = 0u64;
        for i in 0..n {
            let t = self.0.get(i).copied().unwrap_or(0) as u64 + o.0.get(i).copied().unwrap_or(0) as u64 + carry;
            r.push(t as u32);
            carry = t >> 32;
        }
        if carry != 0 {
            r.push(carry as u32);
        }
        let mut b = Big(r);
        b.trim();
        b
    }
}

/// Montgomery arithmetic modulo an odd `m` (CIOS, 32-bit limbs): RSA's modular powers in a
/// few milliseconds instead of the bit-by-bit remainder's seconds.
struct Mont {
    m: Vec<u32>,
    /// -m^-1 mod 2^32.
    inv: u32,
    /// R^2 mod m, R = 2^(32 n).
    rr: Vec<u32>,
}

impl Mont {
    fn new(m: &Big) -> Option<Mont> {
        let m0 = *m.0.first()?;
        if m0 & 1 == 0 {
            return None;
        }
        // Newton's iteration for m0^-1 mod 2^32.
        let mut x: u32 = 1;
        for _ in 0..5 {
            x = x.wrapping_mul(2u32.wrapping_sub(m0.wrapping_mul(x)));
        }
        let n = m.0.len();
        let mut r2 = alloc::vec![0u32; 2 * n + 1];
        r2[2 * n] = 1;
        let rr = Big(r2).rem(m);
        let mut rr = rr.0;
        rr.resize(n, 0);
        Some(Mont { m: m.0.clone(), inv: x.wrapping_neg(), rr })
    }
    /// a b R^-1 mod m, for a, b < m (n limbs each).
    fn mul(&self, a: &[u32], b: &[u32], t: &mut Vec<u32>) -> Vec<u32> {
        let n = self.m.len();
        t.clear();
        t.resize(n + 2, 0);
        for i in 0..n {
            let bi = b[i] as u64;
            let mut c = 0u64;
            for j in 0..n {
                let s = t[j] as u64 + a[j] as u64 * bi + c;
                t[j] = s as u32;
                c = s >> 32;
            }
            let s = t[n] as u64 + c;
            t[n] = s as u32;
            t[n + 1] = (s >> 32) as u32;
            let q = t[0].wrapping_mul(self.inv) as u64;
            let s = t[0] as u64 + q * self.m[0] as u64;
            let mut c = s >> 32;
            for j in 1..n {
                let s = t[j] as u64 + q * self.m[j] as u64 + c;
                t[j - 1] = s as u32;
                c = s >> 32;
            }
            let s = t[n] as u64 + c;
            t[n - 1] = s as u32;
            t[n] = t[n + 1] + (s >> 32) as u32;
        }
        let mut r: Vec<u32> = t[..n].to_vec();
        let ge = t[n] != 0 || {
            let mut ge = true;
            for i in (0..n).rev() {
                if r[i] != self.m[i] {
                    ge = r[i] > self.m[i];
                    break;
                }
            }
            ge
        };
        if ge {
            let mut borrow = 0i64;
            for i in 0..n {
                let d = r[i] as i64 - self.m[i] as i64 - borrow;
                r[i] = d as u32;
                borrow = (d < 0) as i64;
            }
        }
        r
    }
    fn pow(&self, base: &Big, e: &Big) -> Big {
        let n = self.m.len();
        let mut b = base.rem(&Big(self.m.clone())).0;
        b.resize(n, 0);
        let mut t = Vec::with_capacity(n + 2);
        let x = self.mul(&b, &self.rr, &mut t);
        let mut one = alloc::vec![0u32; n];
        one[0] = 1;
        let mut acc = self.mul(&one, &self.rr, &mut t);
        for i in (0..e.bits()).rev() {
            acc = self.mul(&acc, &acc, &mut t);
            if e.bit(i) {
                acc = self.mul(&acc, &x, &mut t);
            }
        }
        let mut r = Big(self.mul(&acc, &one, &mut t));
        r.trim();
        r
    }
}

/// EMSA-PKCS1-v1_5 for SHA-256: 00 01 FF.. 00, the DigestInfo, the hash.
fn pkcs1_sha256(msg: &[u8], k: usize) -> Option<Vec<u8>> {
    const INFO: [u8; 19] = [0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20];
    if k < 19 + 32 + 11 {
        return None;
    }
    let mut em = alloc::vec![0xFFu8; k];
    em[0] = 0;
    em[1] = 1;
    let t = k - 32 - 19;
    em[t - 1] = 0;
    em[t..t + 19].copy_from_slice(&INFO);
    em[t + 19..].copy_from_slice(&sha256(msg));
    Some(em)
}

impl RsaKey {
    /// The modulus, big-endian.
    pub fn modulus(&self) -> Vec<u8> {
        self.n.to_be(self.size)
    }

    /// RSASSA-PKCS1-v1_5 verification with SHA-256 (SHA256withRSA).
    pub fn verify_sha256(&self, msg: &[u8], sig: &[u8]) -> bool {
        if sig.len() != self.size {
            return false;
        }
        let s = Big::from_be(sig);
        if s.ge(&self.n) {
            return false;
        }
        let Some(m) = Mont::new(&self.n) else { return false };
        let em = m.pow(&s, &self.e).to_be(self.size);
        pkcs1_sha256(msg, self.size).map_or(false, |want| want == em)
    }
}

/// An RSA private key (PKCS#1 RSAPrivateKey, or wrapped in PKCS#8): the profile key pair's
/// private half that signs chat. The CRT values make a 2048-bit signature take milliseconds.
pub struct RsaPrivate {
    pub public: RsaKey,
    d: Big,
    crt: Option<(Big, Big, Big, Big, Big)>,
}

impl RsaPrivate {
    pub fn from_der(der_bytes: &[u8]) -> Option<RsaPrivate> {
        let (tag, seq, _) = der(der_bytes)?;
        if tag != 0x30 {
            return None;
        }
        let (t, _version, rest) = der(seq)?;
        if t != 0x02 {
            return None;
        }
        // PKCS#8: AlgorithmIdentifier, then the RSAPrivateKey in an OCTET STRING.
        if rest.first() == Some(&0x30) {
            let (_, alg, rest) = der(rest)?;
            let (_, oid, _) = der(alg)?;
            if oid != [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01] {
                return None;
            }
            let (t, inner, _) = der(rest)?;
            if t != 0x04 {
                return None;
            }
            return RsaPrivate::from_der(inner);
        }
        let mut ints = Vec::new();
        let mut rest = rest;
        while !rest.is_empty() && ints.len() < 8 {
            let (t, v, r) = der(rest)?;
            if t != 0x02 {
                return None;
            }
            ints.push(Big::from_be(v));
            rest = r;
        }
        if ints.len() < 3 {
            return None;
        }
        let n = ints[0].clone();
        let size = (n.bits() + 7) / 8;
        let crt = if ints.len() == 8 {
            Some((ints[3].clone(), ints[4].clone(), ints[5].clone(), ints[6].clone(), ints[7].clone()))
        } else {
            None
        };
        Some(RsaPrivate { public: RsaKey { n, e: ints[1].clone(), size }, d: ints[2].clone(), crt })
    }

    /// RSASSA-PKCS1-v1_5 with SHA-256 (Java's SHA256withRSA): deterministic.
    pub fn sign_sha256(&self, msg: &[u8]) -> Option<Vec<u8>> {
        let k = self.public.size;
        let m = Big::from_be(&pkcs1_sha256(msg, k)?);
        let s = match &self.crt {
            Some((p, q, dp, dq, qinv)) => {
                let m1 = Mont::new(p)?.pow(&m, dp);
                let m2 = Mont::new(q)?.pow(&m, dq);
                // h = qinv (m1 - m2) mod p; s = m2 + h q
                let m2p = m2.rem(p);
                let mut diff = if m1.ge(&m2p) { m1.clone() } else { m1.add(p) };
                diff.sub(&m2p);
                let h = qinv.mul(&diff).rem(p);
                m2.add(&h.mul(q))
            }
            None => Mont::new(&self.public.n)?.pow(&m, &self.d),
        };
        Some(s.to_be(k))
    }
}
