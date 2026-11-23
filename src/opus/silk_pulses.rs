// Copyright (c) 2006-2011, Skype Limited. All rights reserved.
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
// - Redistributions of source code must retain the above copyright notice,
// this list of conditions and the following disclaimer.
// - Redistributions in binary form must reproduce the above copyright
// notice, this list of conditions and the following disclaimer in the
// documentation and/or other materials provided with the distribution.
// - Neither the name of Internet Society, IETF or IETF Trust, nor the
// names of specific contributors, may be used to endorse or promote
// products derived from this software without specific prior written
// permission.
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
// POSSIBILITY OF SUCH DAMAGE.
// Port of the SILK excitation entropy decoding of libopus: decode_pulses.c, shell_coder.c (decoder),
// code_signs.c (decoder).

#![allow(dead_code)]

use super::rangedec::RangeDecoder;
use super::silk_tables::*;

const SHELL_CODEC_FRAME_LENGTH: usize = 16;
const MAX_NB_SHELL_BLOCKS: usize = 20;
const SILK_MAX_PULSES: i32 = 16;
const N_RATE_LEVELS: usize = 10;

#[inline]
fn decode_split(rc: &mut RangeDecoder, p: i32, shell_table: &[u8; 152]) -> (i16, i16) {
    if p > 0 {
        let off = SHELL_CODE_TABLE_OFFSETS[(p as usize).min(16)] as usize;
        let c1 = rc.icdf(&shell_table[off..], 8) as i16;
        (c1, (p - c1 as i32) as i16)
    } else {
        (0, 0)
    }
}

/// silk_shell_decoder: one shell code frame of 16 pulses (pulses0 has 16 entries).
fn shell_decoder(pulses0: &mut [i16], rc: &mut RangeDecoder, pulses4: i32) {
    let mut p3 = [0i16; 2];
    let mut p2 = [0i16; 4];
    let mut p1 = [0i16; 8];
    (p3[0], p3[1]) = decode_split(rc, pulses4, &SHELL_CODE_TABLE3);
    (p2[0], p2[1]) = decode_split(rc, p3[0] as i32, &SHELL_CODE_TABLE2);
    (p1[0], p1[1]) = decode_split(rc, p2[0] as i32, &SHELL_CODE_TABLE1);
    (pulses0[0], pulses0[1]) = decode_split(rc, p1[0] as i32, &SHELL_CODE_TABLE0);
    (pulses0[2], pulses0[3]) = decode_split(rc, p1[1] as i32, &SHELL_CODE_TABLE0);
    (p1[2], p1[3]) = decode_split(rc, p2[1] as i32, &SHELL_CODE_TABLE1);
    (pulses0[4], pulses0[5]) = decode_split(rc, p1[2] as i32, &SHELL_CODE_TABLE0);
    (pulses0[6], pulses0[7]) = decode_split(rc, p1[3] as i32, &SHELL_CODE_TABLE0);
    (p2[2], p2[3]) = decode_split(rc, p3[1] as i32, &SHELL_CODE_TABLE2);
    (p1[4], p1[5]) = decode_split(rc, p2[2] as i32, &SHELL_CODE_TABLE1);
    (pulses0[8], pulses0[9]) = decode_split(rc, p1[4] as i32, &SHELL_CODE_TABLE0);
    (pulses0[10], pulses0[11]) = decode_split(rc, p1[5] as i32, &SHELL_CODE_TABLE0);
    (p1[6], p1[7]) = decode_split(rc, p2[3] as i32, &SHELL_CODE_TABLE1);
    (pulses0[12], pulses0[13]) = decode_split(rc, p1[6] as i32, &SHELL_CODE_TABLE0);
    (pulses0[14], pulses0[15]) = decode_split(rc, p1[7] as i32, &SHELL_CODE_TABLE0);
}

/// silk_decode_signs.
fn decode_signs(rc: &mut RangeDecoder, pulses: &mut [i16], length: usize, signal_type: i32, quant_offset_type: i32, sum_pulses: &[i32; MAX_NB_SHELL_BLOCKS]) {
    let mut icdf = [0u8; 2];
    let base = (7 * (quant_offset_type + (signal_type << 1))) as usize;
    let blocks = (length + SHELL_CODEC_FRAME_LENGTH / 2) >> 4;
    for i in 0..blocks {
        let p = sum_pulses[i];
        if p > 0 {
            icdf[0] = SIGN_ICDF[base + core::cmp::min(p & 0x1F, 6) as usize];
            for j in 0..SHELL_CODEC_FRAME_LENGTH {
                let q = &mut pulses[i * SHELL_CODEC_FRAME_LENGTH + j];
                if *q > 0 {
                    let s = rc.icdf(&icdf, 8);
                    *q = (*q as i32 * ((s << 1) - 1)) as i16;
                }
            }
        }
    }
}

/// silk_decode_pulses: decodes the quantised excitation of one frame into `pulses` (at least
/// `frame_length` rounded up to a multiple of 16 entries).
pub fn decode_pulses(rc: &mut RangeDecoder, pulses: &mut [i16], signal_type: i32, quant_offset_type: i32, frame_length: usize) {
    let mut sum_pulses = [0i32; MAX_NB_SHELL_BLOCKS];
    let mut nlshifts = [0i32; MAX_NB_SHELL_BLOCKS];

    let rate_level = rc.icdf(&RATE_LEVELS_ICDF[((signal_type >> 1) as usize).min(1)], 8) as usize;

    let mut iter = frame_length >> 4;
    if iter * SHELL_CODEC_FRAME_LENGTH < frame_length {
        iter += 1;
    }
    let iter = iter.min(MAX_NB_SHELL_BLOCKS);

    let cdf = &PULSES_PER_BLOCK_ICDF[rate_level.min(N_RATE_LEVELS - 1)];
    for i in 0..iter {
        nlshifts[i] = 0;
        sum_pulses[i] = rc.icdf(cdf, 8);
        while sum_pulses[i] == SILK_MAX_PULSES + 1 {
            nlshifts[i] += 1;
            let off = (nlshifts[i] == 10) as usize;
            sum_pulses[i] = rc.icdf(&PULSES_PER_BLOCK_ICDF[N_RATE_LEVELS - 1][off..], 8);
        }
    }

    for i in 0..iter {
        let blk = &mut pulses[i * SHELL_CODEC_FRAME_LENGTH..(i + 1) * SHELL_CODEC_FRAME_LENGTH];
        if sum_pulses[i] > 0 {
            shell_decoder(blk, rc, sum_pulses[i]);
        } else {
            for v in blk.iter_mut() {
                *v = 0;
            }
        }
    }

    for i in 0..iter {
        if nlshifts[i] > 0 {
            let nls = nlshifts[i];
            let blk = &mut pulses[i * SHELL_CODEC_FRAME_LENGTH..(i + 1) * SHELL_CODEC_FRAME_LENGTH];
            for v in blk.iter_mut() {
                let mut abs_q = *v as i32;
                for _ in 0..nls {
                    abs_q <<= 1;
                    abs_q += rc.icdf(&LSB_ICDF, 8);
                }
                *v = abs_q as i16;
            }
            sum_pulses[i] |= nls << 5;
        }
    }

    decode_signs(rc, pulses, frame_length, signal_type, quant_offset_type, &sum_pulses);
}
