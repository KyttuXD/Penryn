//! The Opus range decoder (libopus celt/entdec.c, entcode.c), shared by SILK and CELT: a
//! `no_std`-compatible port (core only). Ported from libopus, Copyright (c) 2001-2011 Timothy B.
//! Terriberry and Jean-Marc Valin, 2008-2009 Xiph.Org Foundation, BSD-3-Clause licence.

const SYM_BITS: u32 = 8;
const CODE_BITS: u32 = 32;
const SYM_MAX: u32 = (1 << SYM_BITS) - 1;
const CODE_TOP: u32 = 1 << (CODE_BITS - 1);
const CODE_BOT: u32 = CODE_TOP >> SYM_BITS;
const CODE_EXTRA: u32 = (CODE_BITS - 2) % SYM_BITS + 1;
const WINDOW_SIZE: u32 = 32;
const UINT_BITS: u32 = 8;
/// ec_tell_frac's resolution: 1/8 bit.
pub const BITRES: u32 = 3;

/// `ec_ilog`: the number of bits needed for `v` (0 for 0).
#[inline]
pub fn ilog(v: u32) -> i32 {
    32 - v.leading_zeros() as i32
}

pub struct RangeDecoder<'a> {
    buf: &'a [u8],
    storage: u32,
    end_offs: u32,
    end_window: u32,
    nend_bits: i32,
    /// Total bits consumed so far (ec_tell's base).
    nbits_total: i32,
    offs: u32,
    rng: u32,
    val: u32,
    ext: u32,
    rem: i32,
    /// Set when the stream was found corrupt (ec_get_error).
    pub error: bool,
}

impl<'a> RangeDecoder<'a> {
    /// `ec_dec_init`.
    pub fn new(buf: &'a [u8]) -> RangeDecoder<'a> {
        let mut d = RangeDecoder {
            buf,
            storage: buf.len() as u32,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: (CODE_BITS + 1 - ((CODE_BITS - CODE_EXTRA) / SYM_BITS) * SYM_BITS) as i32,
            offs: 0,
            rng: 1 << CODE_EXTRA,
            val: 0,
            ext: 0,
            rem: 0,
            error: false,
        };
        d.rem = d.read_byte();
        d.val = d.rng - 1 - ((d.rem as u32) >> (SYM_BITS - CODE_EXTRA));
        d.normalize();
        d
    }

    /// The bytes the decoder was given (CELT's hybrid frames read the rest of a SILK frame's).
    pub fn storage(&self) -> u32 {
        self.storage
    }

    /// Shortens the buffer's end (ec_dec: storage -= n), for the redundancy frame CELT reads
    /// after the main frame in a hybrid or CELT packet.
    pub fn shrink(&mut self, n: u32) {
        self.storage = self.storage.saturating_sub(n);
    }

    fn read_byte(&mut self) -> i32 {
        if self.offs < self.storage {
            let b = self.buf[self.offs as usize] as i32;
            self.offs += 1;
            b
        } else {
            0
        }
    }

    fn read_byte_from_end(&mut self) -> u32 {
        if self.end_offs < self.storage {
            self.end_offs += 1;
            self.buf[(self.storage - self.end_offs) as usize] as u32
        } else {
            0
        }
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.nbits_total += SYM_BITS as i32;
            self.rng <<= SYM_BITS;
            let mut sym = self.rem;
            self.rem = self.read_byte();
            sym = ((sym << SYM_BITS) | self.rem) >> (SYM_BITS - CODE_EXTRA);
            self.val = ((self.val << SYM_BITS).wrapping_add(SYM_MAX & !(sym as u32))) & (CODE_TOP - 1);
        }
    }

    /// `ec_decode`: the value in 0..ft that the next symbol falls in (then call `update`).
    pub fn decode(&mut self, ft: u32) -> u32 {
        self.ext = self.rng / ft;
        let s = self.val / self.ext;
        ft - core::cmp::min(s + 1, ft)
    }

    /// `ec_decode_bin`: as `decode` for ft = 1 << bits.
    pub fn decode_bin(&mut self, bits: u32) -> u32 {
        self.ext = self.rng >> bits;
        let s = self.val / self.ext;
        (1u32 << bits) - core::cmp::min(s + 1, 1u32 << bits)
    }

    /// `ec_dec_update`.
    pub fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let s = self.ext.wrapping_mul(ft - fh);
        self.val = self.val.wrapping_sub(s);
        self.rng = if fl > 0 { self.ext.wrapping_mul(fh - fl) } else { self.rng.wrapping_sub(s) };
        self.normalize();
    }

    /// `ec_dec_bit_logp`: a bit whose probability of 1 is 1/2^logp.
    pub fn bit_logp(&mut self, logp: u32) -> i32 {
        let r = self.rng;
        let d = self.val;
        let s = r >> logp;
        let ret = d < s;
        if !ret {
            self.val = d - s;
        }
        self.rng = if ret { s } else { r - s };
        self.normalize();
        ret as i32
    }

    /// `ec_dec_icdf`: a symbol from an inverse cdf of bytes (total 1 << ftb).
    pub fn icdf(&mut self, icdf: &[u8], ftb: u32) -> i32 {
        let mut s = self.rng;
        let d = self.val;
        let r = s >> ftb;
        let mut ret: i32 = -1;
        let mut t;
        loop {
            t = s;
            ret += 1;
            s = r.wrapping_mul(icdf[ret as usize] as u32);
            if d >= s {
                break;
            }
        }
        self.val = d - s;
        self.rng = t - s;
        self.normalize();
        ret
    }

    /// `ec_dec_icdf16`: as `icdf` for 16-bit tables.
    pub fn icdf16(&mut self, icdf: &[u16], ftb: u32) -> i32 {
        let mut s = self.rng;
        let d = self.val;
        let r = s >> ftb;
        let mut ret: i32 = -1;
        let mut t;
        loop {
            t = s;
            ret += 1;
            s = r.wrapping_mul(icdf[ret as usize] as u32);
            if d >= s {
                break;
            }
        }
        self.val = d - s;
        self.rng = t - s;
        self.normalize();
        ret
    }

    /// `ec_dec_uint`: a uniform integer in 0..ft (ft > 1).
    pub fn uint(&mut self, ft: u32) -> u32 {
        let mut ft = ft - 1;
        let mut ftb = ilog(ft) as u32;
        if ftb > UINT_BITS {
            ftb -= UINT_BITS;
            let top = (ft >> ftb) + 1;
            let s = self.decode(top);
            self.update(s, s + 1, top);
            let t = (s << ftb) | self.bits(ftb);
            if t <= ft {
                return t;
            }
            self.error = true;
            ft
        } else {
            ft += 1;
            let s = self.decode(ft);
            self.update(s, s + 1, ft);
            s
        }
    }

    /// `ec_dec_bits`: raw bits, read from the end of the buffer.
    pub fn bits(&mut self, n: u32) -> u32 {
        let mut window = self.end_window;
        let mut available = self.nend_bits;
        if (available as u32) < n {
            loop {
                window |= self.read_byte_from_end() << available;
                available += SYM_BITS as i32;
                if available > (WINDOW_SIZE - SYM_BITS) as i32 {
                    break;
                }
            }
        }
        let ret = window & ((1u32 << n).wrapping_sub(1));
        window = if n >= 32 { 0 } else { window >> n };
        available -= n as i32;
        self.end_window = window;
        self.nend_bits = available;
        self.nbits_total += n as i32;
        ret
    }

    /// `ec_tell`: whole bits used so far.
    pub fn tell(&self) -> i32 {
        self.nbits_total - ilog(self.rng)
    }

    /// `ec_tell_frac`: bits used, in 1/8 bits.
    pub fn tell_frac(&self) -> u32 {
        const CORRECTION: [u32; 8] = [35733, 38967, 42495, 46340, 50535, 55109, 60097, 65535];
        let nbits = (self.nbits_total as u32) << BITRES;
        let mut l = ilog(self.rng);
        let r = self.rng >> (l - 16);
        let mut b = (r >> 12) - 8;
        if r > CORRECTION[b as usize] {
            b += 1;
        }
        l = (l << 3) + b as i32;
        nbits - l as u32
    }

    /// The range, for decoders that need ec_dec's state (CELT's anti-collapse and the like).
    pub fn range(&self) -> u32 {
        self.rng
    }

    /// Pretends all of the first `total_bits` bits were consumed, so that `tell` returns
    /// `total_bits` from now on (CELT's silence flag does `dec->nbits_total += tell - ec_tell(dec)`).
    pub fn mark_all_read(&mut self, total_bits: i32) {
        let t = self.tell();
        self.nbits_total = self.nbits_total.wrapping_add(total_bits.wrapping_sub(t));
    }
}
