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
// Port of the SILK output resampler of libopus (decoder side): resampler.c, resampler_private_IIR_FIR.c,
// resampler_private_up2_HQ.c. Converts the internal 8/12/16 kHz signal to the API rate (48 kHz here; 12, 16
// and 24 kHz also work) with the same integer arithmetic as libopus.

#![allow(dead_code)]

use super::silk_math::*;
use super::silk_tables::RESAMPLER_FRAC_FIR_12;

const RESAMPLER_MAX_BATCH_SIZE_MS: i32 = 10;
const RESAMPLER_ORDER_FIR_12: usize = 8;

const UP2_HQ_0: [i16; 3] = [1746, 14986, (39083 - 65536) as i16];
const UP2_HQ_1: [i16; 3] = [6854, 25769, (55542 - 65536) as i16];

/// Delay compensation per (input rate, output rate); rows 8, 12, 16 kHz, columns 8, 12, 16, 24, 48 kHz.
const DELAY_MATRIX_DEC: [[i32; 5]; 3] = [[4, 0, 2, 0, 0], [0, 9, 4, 7, 4], [0, 3, 12, 7, 7]];

#[derive(Clone, Copy, PartialEq)]
enum Func {
    Copy,
    Up2Hq,
    IirFir,
}

#[derive(Clone)]
pub struct Resampler {
    s_iir: [i32; 6],
    s_fir: [i16; RESAMPLER_ORDER_FIR_12],
    delay_buf: [i16; 96],
    func: Func,
    batch_size: i32,
    inv_ratio_q16: i32,
    fs_in_khz: i32,
    fs_out_khz: i32,
    input_delay: i32,
}

impl Resampler {
    /// silk_resampler_init for the decoder (forEnc = 0). Returns None for unsupported rate pairs.
    pub fn new(fs_hz_in: i32, fs_hz_out: i32) -> Option<Resampler> {
        let row = match fs_hz_in {
            8000 => 0,
            12000 => 1,
            16000 => 2,
            _ => return None,
        };
        let col = match fs_hz_out {
            8000 => 0,
            12000 => 1,
            16000 => 2,
            24000 => 3,
            48000 => 4,
            _ => return None,
        };
        let mut up2x = 0;
        let func;
        if fs_hz_out > fs_hz_in {
            if fs_hz_out == fs_hz_in * 2 {
                func = Func::Up2Hq;
            } else {
                func = Func::IirFir;
                up2x = 1;
            }
        } else if fs_hz_out < fs_hz_in {
            // Downsampling is not needed for the 48 kHz API rate.
            return None;
        } else {
            func = Func::Copy;
        }
        let mut s = Resampler {
            s_iir: [0; 6],
            s_fir: [0; RESAMPLER_ORDER_FIR_12],
            delay_buf: [0; 96],
            func,
            batch_size: (fs_hz_in / 1000) * RESAMPLER_MAX_BATCH_SIZE_MS,
            inv_ratio_q16: 0,
            fs_in_khz: fs_hz_in / 1000,
            fs_out_khz: fs_hz_out / 1000,
            input_delay: DELAY_MATRIX_DEC[row][col],
        };
        s.inv_ratio_q16 = lshift(lshift(fs_hz_in, 14 + up2x) / fs_hz_out, 2);
        while smulww(s.inv_ratio_q16, fs_hz_out) < lshift(fs_hz_in, up2x as u32) {
            s.inv_ratio_q16 += 1;
        }
        Some(s)
    }

    /// silk_resampler: `out` must hold `in_.len() * fs_out / fs_in` samples; `in_.len()` >= 1 ms.
    pub fn process(&mut self, out: &mut [i16], in_: &[i16]) {
        let in_len = in_.len();
        let fs_in = self.fs_in_khz as usize;
        let fs_out = self.fs_out_khz as usize;
        let delay = self.input_delay as usize;
        if in_len < fs_in || out.len() < in_len * fs_out / fs_in {
            return;
        }
        let n_samples = fs_in - delay;
        self.delay_buf[delay..fs_in].copy_from_slice(&in_[..n_samples]);
        let delay_buf = self.delay_buf;
        match self.func {
            Func::Up2Hq => {
                up2_hq(&mut self.s_iir, &mut out[..2 * fs_in], &delay_buf[..fs_in]);
                up2_hq(&mut self.s_iir, &mut out[fs_out..], &in_[n_samples..in_len - delay]);
            }
            Func::IirFir => {
                self.iir_fir(&mut out[..fs_out], &delay_buf[..fs_in]);
                self.iir_fir(&mut out[fs_out..], &in_[n_samples..in_len - delay]);
            }
            Func::Copy => {
                out[..fs_in].copy_from_slice(&delay_buf[..fs_in]);
                let rest = in_len - fs_in;
                out[fs_out..fs_out + rest].copy_from_slice(&in_[n_samples..n_samples + rest]);
            }
        }
        self.delay_buf[..delay].copy_from_slice(&in_[in_len - delay..]);
    }

    /// silk_resampler_private_IIR_FIR: allpass 2x upsampling followed by polyphase FIR interpolation.
    fn iir_fir(&mut self, out: &mut [i16], in_: &[i16]) {
        let mut buf = [0i16; 2 * 160 + RESAMPLER_ORDER_FIR_12];
        buf[..RESAMPLER_ORDER_FIR_12].copy_from_slice(&self.s_fir);
        let index_increment_q16 = self.inv_ratio_q16;
        let mut inp = in_;
        let mut outp = 0usize;
        let mut n_samples_in;
        loop {
            n_samples_in = core::cmp::min(inp.len(), self.batch_size as usize);
            up2_hq(
                &mut self.s_iir,
                &mut buf[RESAMPLER_ORDER_FIR_12..RESAMPLER_ORDER_FIR_12 + 2 * n_samples_in],
                &inp[..n_samples_in],
            );
            let max_index_q16 = lshift(n_samples_in as i32, 16 + 1);
            let mut index_q16: i32 = 0;
            while index_q16 < max_index_q16 {
                let table_index = smulwb(index_q16 & 0xFFFF, 12) as usize;
                let bp = (index_q16 >> 16) as usize;
                let t = &RESAMPLER_FRAC_FIR_12[table_index];
                let u = &RESAMPLER_FRAC_FIR_12[11 - table_index];
                let mut res_q15 = smulbb(buf[bp] as i32, t[0] as i32);
                res_q15 = smlabb(res_q15, buf[bp + 1] as i32, t[1] as i32);
                res_q15 = smlabb(res_q15, buf[bp + 2] as i32, t[2] as i32);
                res_q15 = smlabb(res_q15, buf[bp + 3] as i32, t[3] as i32);
                res_q15 = smlabb(res_q15, buf[bp + 4] as i32, u[3] as i32);
                res_q15 = smlabb(res_q15, buf[bp + 5] as i32, u[2] as i32);
                res_q15 = smlabb(res_q15, buf[bp + 6] as i32, u[1] as i32);
                res_q15 = smlabb(res_q15, buf[bp + 7] as i32, u[0] as i32);
                if outp < out.len() {
                    out[outp] = sat16(rshift_round(res_q15, 15)) as i16;
                }
                outp += 1;
                index_q16 = index_q16.wrapping_add(index_increment_q16);
            }
            inp = &inp[n_samples_in..];
            if !inp.is_empty() {
                let (a, b) = buf.split_at_mut(n_samples_in << 1);
                a[..RESAMPLER_ORDER_FIR_12].copy_from_slice(&b[..RESAMPLER_ORDER_FIR_12]);
            } else {
                break;
            }
        }
        let k = n_samples_in << 1;
        self.s_fir.copy_from_slice(&buf[k..k + RESAMPLER_ORDER_FIR_12]);
    }
}

/// silk_resampler_private_up2_HQ: 2x upsampling with two chains of three allpass sections.
fn up2_hq(s: &mut [i32; 6], out: &mut [i16], in_: &[i16]) {
    for k in 0..in_.len() {
        let in32 = lshift(in_[k] as i32, 10);

        let mut y = in32.wrapping_sub(s[0]);
        let mut x = smulwb(y, UP2_HQ_0[0] as i32);
        let mut out32_1 = s[0].wrapping_add(x);
        s[0] = in32.wrapping_add(x);

        y = out32_1.wrapping_sub(s[1]);
        x = smulwb(y, UP2_HQ_0[1] as i32);
        let mut out32_2 = s[1].wrapping_add(x);
        s[1] = out32_1.wrapping_add(x);

        y = out32_2.wrapping_sub(s[2]);
        x = smlawb(y, y, UP2_HQ_0[2] as i32);
        out32_1 = s[2].wrapping_add(x);
        s[2] = out32_2.wrapping_add(x);

        out[2 * k] = sat16(rshift_round(out32_1, 10)) as i16;

        y = in32.wrapping_sub(s[3]);
        x = smulwb(y, UP2_HQ_1[0] as i32);
        out32_1 = s[3].wrapping_add(x);
        s[3] = in32.wrapping_add(x);

        y = out32_1.wrapping_sub(s[4]);
        x = smulwb(y, UP2_HQ_1[1] as i32);
        out32_2 = s[4].wrapping_add(x);
        s[4] = out32_1.wrapping_add(x);

        y = out32_2.wrapping_sub(s[5]);
        x = smlawb(y, y, UP2_HQ_1[2] as i32);
        out32_1 = s[5].wrapping_add(x);
        s[5] = out32_2.wrapping_add(x);

        out[2 * k + 1] = sat16(rshift_round(out32_1, 10)) as i16;
    }
}
