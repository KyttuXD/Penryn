//! AES-128-GCM as the Simple Voice Chat UDP protocol uses it (spec section 4.1): 12-byte IV,
//! 16-byte tag appended to the ciphertext, no additional authenticated data.
//!
//! Original code written from NIST SP 800-38D; the AES block function is the one in
//! `crate::crypto` (nothing is duplicated). GHASH is the plain bitwise multiply in GF(2^128),
//! branch-free on the data, which is plenty for ~100 byte datagrams at 50 per second.

use crate::crypto::Aes128;
use alloc::vec::Vec;

pub const TAG_LEN: usize = 16;
pub const IV_LEN: usize = 12;

/// x * y in GF(2^128) with the GCM bit order (the leftmost bit is the coefficient of x^0).
fn gf_mul(x: u128, y: u128) -> u128 {
    const R: u128 = 0xE1 << 120;
    let mut z = 0u128;
    let mut v = y;
    for i in 0..128 {
        let bit = (x >> (127 - i)) & 1;
        z ^= v & 0u128.wrapping_sub(bit);
        let lsb = v & 1;
        v >>= 1;
        v ^= R & 0u128.wrapping_sub(lsb);
    }
    z
}

struct Ghash {
    h: u128,
    y: u128,
}

impl Ghash {
    /// Absorbs `data`, zero padded to a multiple of 16 bytes.
    fn update(&mut self, data: &[u8]) {
        for chunk in data.chunks(16) {
            let mut b = [0u8; 16];
            b[..chunk.len()].copy_from_slice(chunk);
            self.y = gf_mul(self.y ^ u128::from_be_bytes(b), self.h);
        }
    }
}

/// CTR keystream starting at counter value 2 (counter 1 is reserved for the tag).
fn ctr_xor(aes: &Aes128, iv: &[u8; 12], data: &mut [u8]) {
    let mut counter: u32 = 2;
    for chunk in data.chunks_mut(16) {
        let mut block = [0u8; 16];
        block[..12].copy_from_slice(iv);
        block[12..].copy_from_slice(&counter.to_be_bytes());
        aes.encrypt_block(&mut block);
        for (d, k) in chunk.iter_mut().zip(block.iter()) {
            *d ^= *k;
        }
        counter = counter.wrapping_add(1);
    }
}

/// The tag over `ciphertext` (empty AAD).
fn compute_tag(aes: &Aes128, iv: &[u8; 12], ciphertext: &[u8]) -> [u8; 16] {
    let mut h = [0u8; 16];
    aes.encrypt_block(&mut h);
    let mut g = Ghash { h: u128::from_be_bytes(h), y: 0 };
    g.update(ciphertext);
    // length block: 64-bit AAD length in bits (0), 64-bit ciphertext length in bits
    let bits = (ciphertext.len() as u128) * 8;
    g.y = gf_mul(g.y ^ bits, g.h);
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(iv);
    j0[15] = 1;
    aes.encrypt_block(&mut j0);
    (g.y ^ u128::from_be_bytes(j0)).to_be_bytes()
}

/// Encrypts `plaintext`; the result is `ciphertext || tag` (len + 16 bytes).
pub fn seal(key: &[u8; 16], iv: &[u8; 12], plaintext: &[u8]) -> Vec<u8> {
    let aes = Aes128::new(key);
    let mut out = Vec::with_capacity(plaintext.len() + TAG_LEN);
    out.extend_from_slice(plaintext);
    ctr_xor(&aes, iv, &mut out);
    let tag = compute_tag(&aes, iv, &out);
    out.extend_from_slice(&tag);
    out
}

/// Decrypts `ciphertext || tag`. `None` when the data is shorter than a tag or the tag does not
/// verify (compared in constant time).
pub fn open(key: &[u8; 16], iv: &[u8; 12], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < TAG_LEN {
        return None;
    }
    let (ct, tag) = data.split_at(data.len() - TAG_LEN);
    let aes = Aes128::new(key);
    let expect = compute_tag(&aes, iv, ct);
    let mut diff = 0u8;
    for (a, b) in expect.iter().zip(tag.iter()) {
        diff |= a ^ b;
    }
    if diff != 0 {
        return None;
    }
    let mut out = ct.to_vec();
    ctr_xor(&aes, iv, &mut out);
    Some(out)
}
