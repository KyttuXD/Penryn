// Ported from libopus, BSD-3-Clause licence. Original notice:
// Copyright (c) 2007 CSIRO
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

//! The Laplace-like distribution used for the CELT coarse energy (celt/laplace.c, decoder).

use super::rangedec::RangeDecoder;

/// The minimum probability of an energy delta (out of 32768).
const LAPLACE_MINP: u32 = 1;
/// The minimum number of guaranteed representable energy deltas (in one direction).
const LAPLACE_NMIN: u32 = 16;

/// When called, decay is positive and at most 11456.
fn ec_laplace_get_freq1(fs0: u32, decay: i32) -> u32 {
    let ft = 32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN) - fs0;
    ((ft as i64 * (16384 - decay) as i64) >> 15) as u32
}

/// `ec_laplace_decode`.
pub fn ec_laplace_decode(dec: &mut RangeDecoder, fs: u32, decay: i32) -> i32 {
    let mut fs = fs;
    let mut val: i32 = 0;
    let fm = dec.decode_bin(15);
    let mut fl: u32 = 0;
    if fm >= fs {
        val += 1;
        fl = fs;
        fs = ec_laplace_get_freq1(fs, decay) + LAPLACE_MINP;
        // Search the decaying part of the PDF.
        while fs > LAPLACE_MINP && fm >= fl + 2 * fs {
            fs *= 2;
            fl += fs;
            fs = (((fs - 2 * LAPLACE_MINP) as i64 * decay as i64) >> 15) as u32;
            fs += LAPLACE_MINP;
            val += 1;
        }
        // Everything beyond that has probability LAPLACE_MINP.
        if fs <= LAPLACE_MINP {
            let di = (fm - fl) >> 1;
            val += di as i32;
            fl += 2 * di * LAPLACE_MINP;
        }
        if fm < fl + fs {
            val = -val;
        } else {
            fl += fs;
        }
    }
    let fh = core::cmp::min(fl + fs, 32768);
    dec.update(fl, fh, 32768);
    val
}
