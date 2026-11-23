// Ported from libopus, BSD-3-Clause licence. Original notice:
// Copyright (c) 2007-2008 CSIRO
// Copyright (c) 2007-2009 Xiph.Org Foundation
// Copyright (c) 2008 Gregory Maxwell
// Written by Jean-Marc Valin and Gregory Maxwell
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

//! The CELT mode of the 48 kHz / 960 sample frame (celt/modes.c, `opus_custom_mode_create` for the
//! static mode): band layout, window and MDCT. The window and the transform twiddles are libopus's
//! static tables (celt/static_modes_float.h), which differ from correctly rounded sin/cos by an ulp
//! here and there; all big tables are in `celt_tables.rs`.

use alloc::vec::Vec;

use super::celt_mdct::Mdct;
use super::celt_tables::{
    BAND_ALLOCATION, CACHE_BITS50, CACHE_CAPS50, CACHE_INDEX50, EBAND5MS, LOGN400, WINDOW120,
};

pub const NB_EBANDS: usize = 21;
pub const EFF_EBANDS: usize = 21;
pub const SHORT_MDCT_SIZE: usize = 120;
pub const OVERLAP: usize = 120;
pub const MAX_LM: usize = 3;
pub const NB_ALLOC_VECTORS: usize = 11;
/// The decoder's history buffer (modes.h DEC_PITCH_BUF_SIZE) and the longest postfilter period.
pub const DECODE_BUFFER_SIZE: usize = 2048;
pub const MAX_PERIOD: usize = 1024;

pub struct CeltMode {
    pub window: Vec<f32>,
    pub mdct: Mdct,
}

impl CeltMode {
    pub fn new() -> CeltMode {
        let window = WINDOW120.to_vec();
        CeltMode { window, mdct: Mdct::new() }
    }

    /// Band edge `i` in units of 200 Hz (coefficients at LM = 0).
    #[inline]
    pub fn ebands(i: usize) -> i32 {
        EBAND5MS[i] as i32
    }

    #[inline]
    pub fn log_n(i: usize) -> i32 {
        LOGN400[i] as i32
    }

    #[inline]
    pub fn alloc_vector(row: usize, band: usize) -> i32 {
        BAND_ALLOCATION[row * NB_EBANDS + band] as i32
    }

    /// The pulse cache entry for (LM + 1, band): `[0]` is the largest pseudo-pulse count, then the
    /// bit costs. Empty if the band size has no cache (index -1).
    pub fn cache_bits(lm_plus1: usize, band: usize) -> &'static [u8] {
        match CACHE_INDEX50.get(lm_plus1 * NB_EBANDS + band) {
            Some(&idx) if idx >= 0 => &CACHE_BITS50[idx as usize..],
            _ => &[],
        }
    }

    /// The per-band cap table row for (2 * LM + C - 1).
    pub fn cache_caps(row: usize) -> &'static [u8] {
        &CACHE_CAPS50[row * NB_EBANDS..(row + 1) * NB_EBANDS]
    }
}
