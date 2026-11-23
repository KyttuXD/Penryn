// Ported from libopus, BSD-3-Clause licence. Original notice:
// Copyright (c) 2007-2008 CSIRO
// Copyright (c) 2007-2009 Xiph.Org Foundation
// Written by Jean-Marc Valin
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
//
// - Redistributions of source code must retain the above copyright
// notice, this list of conditions and the following disclaimer.
//
// - Redistributions in binary form must reproduce the above copyright
// notice, this list of conditions and the following disclaimer in the
// documentation and/or other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
// ``AS IS'' AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
// LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
// A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER
// OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
// EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
// PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
// PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
// LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
// NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
// SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//

//! The Opus range encoder (libopus celt/entenc.c), the counterpart of `rangedec.rs`: a
//! `no_std` + alloc port. Ported from libopus, Copyright (c) 2001-2011 Timothy B. Terriberry and
//! Jean-Marc Valin, 2008-2009 Xiph.Org Foundation, BSD-3-Clause licence (full text above).
//!
//! The encoder owns a buffer of exactly `size` bytes: the range coder fills it from the front, the
//! raw bits (`bits`) from the back, and `done` zeroes the gap in between.

use alloc::vec;
use alloc::vec::Vec;

use super::rangedec::ilog;

const SYM_BITS: u32 = 8;
const CODE_BITS: u32 = 32;
const SYM_MAX: u32 = (1 << SYM_BITS) - 1;
const CODE_TOP: u32 = 1 << (CODE_BITS - 1);
const CODE_BOT: u32 = CODE_TOP >> SYM_BITS;
const CODE_SHIFT: u32 = CODE_BITS - SYM_BITS - 1;
const WINDOW_SIZE: u32 = 32;
const UINT_BITS: u32 = 8;

#[derive(Clone)]
pub struct RangeEncoder {
    buf: Vec<u8>,
    storage: u32,
    end_offs: u32,
    end_window: u64,
    nend_bits: u32,
    nbits_total: i32,
    offs: u32,
    rng: u32,
    /// -1: no byte buffered yet.
    rem: i32,
    ext: u32,
    val: u32,
    /// Set when more was written than the buffer holds (the frame is then corrupt).
    pub error: bool,
}

impl RangeEncoder {
    /// `ec_enc_init` with a zeroed buffer of `size` bytes.
    pub fn new(size: usize) -> RangeEncoder {
        RangeEncoder {
            buf: vec![0u8; size],
            storage: size as u32,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: (CODE_BITS + 1) as i32,
            offs: 0,
            rng: CODE_TOP,
            rem: -1,
            ext: 0,
            val: 0,
            error: false,
        }
    }

    /// The size of the buffer in bytes.
    pub fn storage(&self) -> u32 {
        self.storage
    }

    /// The current range (OPUS_GET_FINAL_RANGE after `done`).
    pub fn range(&self) -> u32 {
        self.rng
    }

    fn write_byte(&mut self, value: u32) {
        if self.offs + self.end_offs >= self.storage {
            self.error = true;
            return;
        }
        self.buf[self.offs as usize] = value as u8;
        self.offs += 1;
    }

    fn write_byte_at_end(&mut self, value: u32) {
        if self.offs + self.end_offs >= self.storage {
            self.error = true;
            return;
        }
        self.end_offs += 1;
        self.buf[(self.storage - self.end_offs) as usize] = value as u8;
    }

    /// `ec_enc_carry_out`.
    fn carry_out(&mut self, c: i32) {
        if c as u32 != SYM_MAX {
            let carry = c >> SYM_BITS;
            if self.rem >= 0 {
                self.write_byte((self.rem + carry) as u32);
            }
            if self.ext > 0 {
                let sym = (SYM_MAX + carry as u32) & SYM_MAX;
                while self.ext > 0 {
                    self.write_byte(sym);
                    self.ext -= 1;
                }
            }
            self.rem = c & SYM_MAX as i32;
        } else {
            self.ext += 1;
        }
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.carry_out((self.val >> CODE_SHIFT) as i32);
            self.val = (self.val << SYM_BITS) & (CODE_TOP - 1);
            self.rng <<= SYM_BITS;
            self.nbits_total += SYM_BITS as i32;
        }
    }

    /// `ec_encode`: codes the symbol with cumulative frequencies fl..fh out of ft.
    pub fn encode(&mut self, fl: u32, fh: u32, ft: u32) {
        if ft == 0 {
            self.error = true;
            return;
        }
        let r = self.rng / ft;
        if fl > 0 {
            self.val = self.val.wrapping_add(self.rng.wrapping_sub(r.wrapping_mul(ft.wrapping_sub(fl))));
            self.rng = r.wrapping_mul(fh.wrapping_sub(fl));
        } else {
            self.rng = self.rng.wrapping_sub(r.wrapping_mul(ft.wrapping_sub(fh)));
        }
        self.normalize();
    }

    /// `ec_encode_bin`: as `encode` for ft = 1 << bits.
    pub fn encode_bin(&mut self, fl: u32, fh: u32, bits: u32) {
        let r = self.rng >> bits;
        if fl > 0 {
            self.val = self.val.wrapping_add(self.rng.wrapping_sub(r.wrapping_mul((1u32 << bits).wrapping_sub(fl))));
            self.rng = r.wrapping_mul(fh.wrapping_sub(fl));
        } else {
            self.rng = self.rng.wrapping_sub(r.wrapping_mul((1u32 << bits).wrapping_sub(fh)));
        }
        self.normalize();
    }

    /// `ec_enc_bit_logp`: a bit whose probability of being 1 is 1/2^logp.
    pub fn bit_logp(&mut self, val: bool, logp: u32) {
        let mut r = self.rng;
        let l = self.val;
        let s = r >> logp;
        r -= s;
        if val {
            self.val = l.wrapping_add(r);
        }
        self.rng = if val { s } else { r };
        self.normalize();
    }

    /// `ec_enc_icdf`: symbol `s` of an inverse cdf of bytes (total 1 << ftb).
    pub fn icdf(&mut self, s: usize, icdf: &[u8], ftb: u32) {
        let r = self.rng >> ftb;
        if s > 0 {
            self.val = self.val.wrapping_add(self.rng.wrapping_sub(r.wrapping_mul(icdf[s - 1] as u32)));
            self.rng = r.wrapping_mul(icdf[s - 1] as u32 - icdf[s] as u32);
        } else {
            self.rng = self.rng.wrapping_sub(r.wrapping_mul(icdf[s] as u32));
        }
        self.normalize();
    }

    /// `ec_enc_uint`: a uniform integer fl in 0..ft (ft > 1).
    pub fn uint(&mut self, fl: u32, ft: u32) {
        if ft <= 1 {
            self.error = true;
            return;
        }
        let ft = ft - 1;
        let mut ftb = ilog(ft) as u32;
        if ftb > UINT_BITS {
            ftb -= UINT_BITS;
            let top = (ft >> ftb) + 1;
            let f = fl >> ftb;
            self.encode(f, f + 1, top);
            self.bits(fl & ((1u32 << ftb) - 1), ftb);
        } else {
            self.encode(fl, fl + 1, ft + 1);
        }
    }

    /// `ec_enc_bits`: `n` (1..=25) raw bits, written from the end of the buffer.
    pub fn bits(&mut self, fl: u32, n: u32) {
        if n == 0 || n > 25 {
            return;
        }
        let mut window = self.end_window;
        let mut used = self.nend_bits;
        if used + n > WINDOW_SIZE {
            loop {
                self.write_byte_at_end((window & SYM_MAX as u64) as u32);
                window >>= SYM_BITS;
                used -= SYM_BITS;
                if used < SYM_BITS {
                    break;
                }
            }
        }
        window |= ((fl & ((1u32 << n) - 1)) as u64) << used;
        used += n;
        self.end_window = window;
        self.nend_bits = used;
        self.nbits_total += n as i32;
    }

    /// `ec_tell`: whole bits used so far (rounded up).
    pub fn tell(&self) -> i32 {
        self.nbits_total - ilog(self.rng)
    }

    /// `ec_tell_frac`: bits used, in 1/8 bits.
    pub fn tell_frac(&self) -> u32 {
        const CORRECTION: [u32; 8] = [35733, 38967, 42495, 46340, 50535, 55109, 60097, 65535];
        let nbits = (self.nbits_total as u32) << 3;
        let mut l = ilog(self.rng);
        let r = self.rng >> (l - 16);
        let mut b = (r >> 12) - 8;
        if r > CORRECTION[b as usize] {
            b += 1;
        }
        l = (l << 3) + b as i32;
        nbits.wrapping_sub(l as u32)
    }

    /// `ec_enc_done`: flushes the coder and returns the finished buffer (exactly `storage` bytes).
    pub fn done(mut self) -> Vec<u8> {
        let mut l = CODE_BITS as i32 - ilog(self.rng);
        let mut msk = (CODE_TOP - 1) >> l;
        let mut end = self.val.wrapping_add(msk) & !msk;
        if (end | msk) >= self.val.wrapping_add(self.rng) {
            l += 1;
            msk >>= 1;
            end = self.val.wrapping_add(msk) & !msk;
        }
        while l > 0 {
            self.carry_out((end >> CODE_SHIFT) as i32);
            end = (end << SYM_BITS) & (CODE_TOP - 1);
            l -= SYM_BITS as i32;
        }
        if self.rem >= 0 || self.ext > 0 {
            self.carry_out(0);
        }
        let mut window = self.end_window;
        let mut used = self.nend_bits;
        while used >= SYM_BITS {
            self.write_byte_at_end((window & SYM_MAX as u64) as u32);
            window >>= SYM_BITS;
            used -= SYM_BITS;
        }
        if !self.error {
            let a = self.offs as usize;
            let b = (self.storage - self.end_offs) as usize;
            for v in self.buf[a..b].iter_mut() {
                *v = 0;
            }
            if used > 0 {
                if self.end_offs >= self.storage {
                    self.error = true;
                } else {
                    let l = -l;
                    if self.offs + self.end_offs >= self.storage && (l as i64) < used as i64 {
                        window &= (1u64 << l.max(0)) - 1;
                        self.error = true;
                    }
                    let idx = (self.storage - self.end_offs - 1) as usize;
                    self.buf[idx] |= window as u8;
                }
            }
        }
        self.buf
    }
}
