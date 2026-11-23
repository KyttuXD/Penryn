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
// Port of the SILK LPC / NLSF helpers of libopus: NLSF_unpack.c, NLSF_decode.c, NLSF_stabilize.c, NLSF2A.c,
// bwexpander.c, bwexpander_32.c, LPC_fit.c, LPC_inv_pred_gain.c, LPC_analysis_filter.c, gain_quant.c
// (dequantisation only), decode_pitch.c and sort.c (insertion sort). Integer arithmetic exactly as libopus.

#![allow(dead_code)]

use super::silk_math::*;
use super::silk_tables::*;

pub const MAX_LPC_ORDER: usize = 16;
pub const SILK_MAX_ORDER_LPC: usize = 24;
pub const NLSF_QUANT_MAX_AMPLITUDE: i32 = 4;
pub const N_LEVELS_QGAIN: i32 = 64;
pub const MAX_DELTA_GAIN_QUANT: i32 = 36;
pub const MIN_DELTA_GAIN_QUANT: i32 = -4;
pub const MAX_LPC_STABILIZE_ITERATIONS: i32 = 16;

const GAIN_OFFSET: i32 = (2 * 128) / 6 + 16 * 128;
const INV_SCALE_Q16: i32 = (65536 * (((88 - 2) * 128) / 6)) / (N_LEVELS_QGAIN - 1);

/// silk_NLSF_unpack: entropy table indices and predictor for the given first-stage index.
pub fn nlsf_unpack(ec_ix: &mut [i16; MAX_LPC_ORDER], pred_q8: &mut [u8; MAX_LPC_ORDER], cb: &NlsfCb, cb1_index: usize) {
    let order = cb.order as usize;
    let base = cb1_index * order / 2;
    let mut i = 0;
    while i < order {
        let entry = cb.ec_sel.get(base + i / 2).copied().unwrap_or(0) as i32;
        ec_ix[i] = (((entry >> 1) & 7) * (2 * NLSF_QUANT_MAX_AMPLITUDE + 1)) as i16;
        pred_q8[i] = cb.pred_q8[i + (entry as usize & 1) * (order - 1)];
        ec_ix[i + 1] = (((entry >> 5) & 7) * (2 * NLSF_QUANT_MAX_AMPLITUDE + 1)) as i16;
        pred_q8[i + 1] = cb.pred_q8[i + ((entry as usize >> 4) & 1) * (order - 1) + 1];
        i += 2;
    }
}

/// Predictive dequantiser for the NLSF residuals (silk_NLSF_residual_dequant).
fn nlsf_residual_dequant(x_q10: &mut [i16; MAX_LPC_ORDER], indices: &[i8], pred_coef_q8: &[u8; MAX_LPC_ORDER], quant_step_size_q16: i32, order: usize) {
    let mut out_q10: i32 = 0;
    for i in (0..order).rev() {
        let pred_q10 = smulbb(out_q10, pred_coef_q8[i] as i32) >> 8;
        out_q10 = lshift(indices[i] as i32, 10);
        if out_q10 > 0 {
            out_q10 = out_q10.wrapping_sub(102);
        } else if out_q10 < 0 {
            out_q10 = out_q10.wrapping_add(102);
        }
        out_q10 = smlawb(pred_q10, out_q10, quant_step_size_q16);
        x_q10[i] = out_q10 as i16;
    }
}

/// silk_insertion_sort_increasing_all_values_int16.
fn insertion_sort_increasing(a: &mut [i16]) {
    for i in 1..a.len() {
        let value = a[i];
        let mut j = i as isize - 1;
        while j >= 0 && value < a[j as usize] {
            a[(j + 1) as usize] = a[j as usize];
            j -= 1;
        }
        a[(j + 1) as usize] = value;
    }
}

/// silk_NLSF_stabilize.
pub fn nlsf_stabilize(nlsf: &mut [i16], delta_min: &[i16], l: usize) {
    const MAX_LOOPS: usize = 20;
    let mut loops = 0;
    while loops < MAX_LOOPS {
        // Find the smallest distance
        let mut min_diff = nlsf[0] as i32 - delta_min[0] as i32;
        let mut ii = 0usize;
        for i in 1..l {
            let diff = nlsf[i] as i32 - (nlsf[i - 1] as i32 + delta_min[i] as i32);
            if diff < min_diff {
                min_diff = diff;
                ii = i;
            }
        }
        let diff = (1 << 15) - (nlsf[l - 1] as i32 + delta_min[l] as i32);
        if diff < min_diff {
            min_diff = diff;
            ii = l;
        }
        if min_diff >= 0 {
            return;
        }
        if ii == 0 {
            nlsf[0] = delta_min[0];
        } else if ii == l {
            nlsf[l - 1] = ((1 << 15) - delta_min[l] as i32) as i16;
        } else {
            let mut min_center: i32 = 0;
            for k in 0..ii {
                min_center += delta_min[k] as i32;
            }
            min_center += (delta_min[ii] as i32) >> 1;
            let mut max_center: i32 = 1 << 15;
            for k in (ii + 1..=l).rev() {
                max_center -= delta_min[k] as i32;
            }
            max_center -= (delta_min[ii] as i32) >> 1;
            let c = rshift_round(nlsf[ii - 1] as i32 + nlsf[ii] as i32, 1);
            let center = (if min_center > max_center {
                // silk_LIMIT_32 with swapped limits
                if c > min_center {
                    min_center
                } else if c < max_center {
                    max_center
                } else {
                    c
                }
            } else if c > max_center {
                max_center
            } else if c < min_center {
                min_center
            } else {
                c
            }) as i16;
            nlsf[ii - 1] = (center as i32 - (delta_min[ii] as i32 >> 1)) as i16;
            nlsf[ii] = (nlsf[ii - 1] as i32 + delta_min[ii] as i32) as i16;
        }
        loops += 1;
    }
    // Safe and simple fall back method
    insertion_sort_increasing(&mut nlsf[..l]);
    nlsf[0] = core::cmp::max(nlsf[0] as i32, delta_min[0] as i32) as i16;
    for i in 1..l {
        nlsf[i] = core::cmp::max(nlsf[i] as i32, add_sat16(nlsf[i - 1] as i32, delta_min[i] as i32)) as i16;
    }
    nlsf[l - 1] = core::cmp::min(nlsf[l - 1] as i32, (1 << 15) - delta_min[l] as i32) as i16;
    for i in (0..l - 1).rev() {
        nlsf[i] = core::cmp::min(nlsf[i] as i32, nlsf[i + 1] as i32 - delta_min[i + 1] as i32) as i16;
    }
}

/// silk_NLSF_decode: `indices` is NLSFIndices[0..=order].
pub fn nlsf_decode(nlsf_q15: &mut [i16; MAX_LPC_ORDER], indices: &[i8], cb: &NlsfCb) {
    let order = cb.order as usize;
    let mut pred_q8 = [0u8; MAX_LPC_ORDER];
    let mut ec_ix = [0i16; MAX_LPC_ORDER];
    let mut res_q10 = [0i16; MAX_LPC_ORDER];
    let i0 = (indices[0].max(0) as usize).min(cb.n_vectors as usize - 1);
    nlsf_unpack(&mut ec_ix, &mut pred_q8, cb, i0);
    nlsf_residual_dequant(&mut res_q10, &indices[1..], &pred_q8, cb.quant_step_size_q16, order);
    let elem = &cb.cb1_nlsf_q8[i0 * order..];
    let wght = &cb.cb1_wght_q9[i0 * order..];
    for i in 0..order {
        let w = wght[i] as i32;
        let q = if w != 0 { lshift(res_q10[i] as i32, 14) / w } else { 0 };
        let tmp = q.wrapping_add(lshift(elem[i] as i32, 7));
        nlsf_q15[i] = tmp.clamp(0, 32767) as i16;
    }
    nlsf_stabilize(nlsf_q15, cb.delta_min_q15, order);
}

/// silk_bwexpander: chirp (bandwidth expand) an AR filter of 16-bit coefficients.
pub fn bwexpander(ar: &mut [i16], d: usize, chirp_q16: i32) {
    let mut chirp = chirp_q16;
    let chirp_minus_one = chirp.wrapping_sub(65536);
    for i in 0..d - 1 {
        ar[i] = rshift_round(chirp.wrapping_mul(ar[i] as i32), 16) as i16;
        chirp = chirp.wrapping_add(rshift_round(chirp.wrapping_mul(chirp_minus_one), 16));
    }
    ar[d - 1] = rshift_round(chirp.wrapping_mul(ar[d - 1] as i32), 16) as i16;
}

/// silk_bwexpander_32.
pub fn bwexpander_32(ar: &mut [i32], d: usize, chirp_q16: i32) {
    let mut chirp = chirp_q16;
    let chirp_minus_one = chirp.wrapping_sub(65536);
    for i in 0..d - 1 {
        ar[i] = smulww(chirp, ar[i]);
        chirp = chirp.wrapping_add(rshift_round(chirp.wrapping_mul(chirp_minus_one), 16));
    }
    ar[d - 1] = smulww(chirp, ar[d - 1]);
}

/// silk_LPC_fit: converts int32 coefficients to int16 and makes sure there is no wrap-around.
pub fn lpc_fit(a_qout: &mut [i16], a_qin: &mut [i32], qout: i32, qin: i32, d: usize) {
    let mut idx = 0usize;
    let mut i = 0;
    while i < 10 {
        let mut maxabs: i32 = 0;
        for k in 0..d {
            let absval = a_qin[k].wrapping_abs();
            if absval > maxabs {
                maxabs = absval;
                idx = k;
            }
        }
        maxabs = rshift_round(maxabs, (qin - qout) as u32);
        if maxabs > 32767 {
            maxabs = core::cmp::min(maxabs, 163838);
            let den = maxabs.wrapping_mul(idx as i32 + 1) >> 2;
            let chirp_q16 = 65470 - if den != 0 { lshift(maxabs - 32767, 14) / den } else { 0 };
            bwexpander_32(a_qin, d, chirp_q16);
        } else {
            break;
        }
        i += 1;
    }
    if i == 10 {
        for k in 0..d {
            a_qout[k] = sat16(rshift_round(a_qin[k], (qin - qout) as u32)) as i16;
            a_qin[k] = lshift(a_qout[k] as i32, (qin - qout) as u32);
        }
    } else {
        for k in 0..d {
            a_qout[k] = rshift_round(a_qin[k], (qin - qout) as u32) as i16;
        }
    }
}

const LPC_QA: u32 = 24;
const A_LIMIT: i32 = 16773022; // SILK_FIX_CONST(0.99975, 24)

#[inline]
fn mul32_frac_q(a: i32, b: i32, q: u32) -> i32 {
    rshift_round64(smull(a, b), q) as i32
}

/// LPC_inverse_pred_gain_QA_c: the inverse prediction gain in Q30, 0 when unstable.
fn lpc_inverse_pred_gain_qa(a_qa: &mut [i32; SILK_MAX_ORDER_LPC], order: usize) -> i32 {
    let mut inv_gain_q30: i32 = 1 << 30;
    let mut k = order - 1;
    while k > 0 {
        if a_qa[k] > A_LIMIT || a_qa[k] < -A_LIMIT {
            return 0;
        }
        let rc_q31 = lshift(a_qa[k], 31 - LPC_QA).wrapping_neg();
        let rc_mult1_q30 = (1i32 << 30).wrapping_sub(smmul(rc_q31, rc_q31));
        inv_gain_q30 = lshift(smmul(inv_gain_q30, rc_mult1_q30), 2);
        if inv_gain_q30 < 107374 {
            // SILK_FIX_CONST(1.0f / MAX_PREDICTION_POWER_GAIN, 30)
            return 0;
        }
        let mult2q = (32 - clz32(rc_mult1_q30.wrapping_abs())) as u32;
        let rc_mult2 = inverse32_var_q(rc_mult1_q30, mult2q as i32 + 30);
        for n in 0..(k + 1) >> 1 {
            let tmp1 = a_qa[n];
            let tmp2 = a_qa[k - n - 1];
            let tmp64 = rshift_round64(smull(sub_sat32(tmp1, mul32_frac_q(tmp2, rc_q31, 31)), rc_mult2), mult2q);
            if tmp64 > i32::MAX as i64 || tmp64 < i32::MIN as i64 {
                return 0;
            }
            a_qa[n] = tmp64 as i32;
            let tmp64 = rshift_round64(smull(sub_sat32(tmp2, mul32_frac_q(tmp1, rc_q31, 31)), rc_mult2), mult2q);
            if tmp64 > i32::MAX as i64 || tmp64 < i32::MIN as i64 {
                return 0;
            }
            a_qa[k - n - 1] = tmp64 as i32;
        }
        k -= 1;
    }
    if a_qa[0] > A_LIMIT || a_qa[0] < -A_LIMIT {
        return 0;
    }
    let rc_q31 = lshift(a_qa[0], 31 - LPC_QA).wrapping_neg();
    let rc_mult1_q30 = (1i32 << 30).wrapping_sub(smmul(rc_q31, rc_q31));
    inv_gain_q30 = lshift(smmul(inv_gain_q30, rc_mult1_q30), 2);
    if inv_gain_q30 < 107374 {
        return 0;
    }
    inv_gain_q30
}

/// silk_LPC_inverse_pred_gain_c: for input in the Q12 domain.
pub fn lpc_inverse_pred_gain(a_q12: &[i16], order: usize) -> i32 {
    let mut atmp = [0i32; SILK_MAX_ORDER_LPC];
    let mut dc_resp: i32 = 0;
    for k in 0..order {
        dc_resp += a_q12[k] as i32;
        atmp[k] = lshift(a_q12[k] as i32, LPC_QA - 12);
    }
    if dc_resp >= 4096 {
        return 0;
    }
    lpc_inverse_pred_gain_qa(&mut atmp, order)
}

/// helper for NLSF2A: polynomial from the interleaved 2*cos(LSF) values (stride 2 starting at `c[0]`).
fn nlsf2a_find_poly(out: &mut [i32; 9], c: &[i32], dd: usize) {
    out[0] = lshift(1, 16);
    out[1] = c[0].wrapping_neg();
    for k in 1..dd {
        let ftmp = c[2 * k];
        out[k + 1] = lshift(out[k - 1], 1).wrapping_sub(rshift_round64(smull(ftmp, out[k]), 16) as i32);
        let mut n = k;
        while n > 1 {
            out[n] = out[n].wrapping_add(out[n - 2].wrapping_sub(rshift_round64(smull(ftmp, out[n - 1]), 16) as i32));
            n -= 1;
        }
        out[1] = out[1].wrapping_sub(ftmp);
    }
}

/// silk_NLSF2A: whitening filter coefficients (Q12) from normalised line spectral frequencies (Q15).
pub fn nlsf2a(a_q12: &mut [i16], nlsf: &[i16], d: usize) {
    const ORDERING16: [usize; 16] = [0, 15, 8, 7, 4, 11, 12, 3, 2, 13, 10, 5, 6, 9, 14, 1];
    const ORDERING10: [usize; 10] = [0, 9, 6, 3, 4, 5, 8, 1, 2, 7];
    let ordering: &[usize] = if d == 16 { &ORDERING16 } else { &ORDERING10 };
    let mut cos_lsf_qa = [0i32; SILK_MAX_ORDER_LPC];
    let mut p = [0i32; 9];
    let mut q = [0i32; 9];
    let mut a32_qa1 = [0i32; SILK_MAX_ORDER_LPC];
    for k in 0..d {
        let f_int = ((nlsf[k] as i32) >> (15 - 7)).clamp(0, 127);
        let f_frac = nlsf[k] as i32 - lshift(f_int, 15 - 7);
        let cos_val = LSFCOSTAB_FIX_Q12[f_int as usize] as i32;
        let delta = LSFCOSTAB_FIX_Q12[f_int as usize + 1] as i32 - cos_val;
        cos_lsf_qa[ordering[k]] = rshift_round(lshift(cos_val, 8).wrapping_add(delta.wrapping_mul(f_frac)), 20 - 16);
    }
    let dd = d >> 1;
    nlsf2a_find_poly(&mut p, &cos_lsf_qa[0..], dd);
    nlsf2a_find_poly(&mut q, &cos_lsf_qa[1..], dd);
    for k in 0..dd {
        let ptmp = p[k + 1].wrapping_add(p[k]);
        let qtmp = q[k + 1].wrapping_sub(q[k]);
        a32_qa1[k] = qtmp.wrapping_neg().wrapping_sub(ptmp);
        a32_qa1[d - k - 1] = qtmp.wrapping_sub(ptmp);
    }
    lpc_fit(a_q12, &mut a32_qa1, 12, 16 + 1, d);
    let mut i = 0;
    while lpc_inverse_pred_gain(a_q12, d) == 0 && i < MAX_LPC_STABILIZE_ITERATIONS {
        bwexpander_32(&mut a32_qa1, d, 65536 - lshift(2, i as u32));
        for k in 0..d {
            a_q12[k] = rshift_round(a32_qa1[k], 16 + 1 - 12) as i16;
        }
        i += 1;
    }
}

/// silk_LPC_analysis_filter: `out` and `in_` have `len` samples; the first `d` outputs are zero.
pub fn lpc_analysis_filter(out: &mut [i16], in_: &[i16], b: &[i16], len: usize, d: usize) {
    for ix in d..len {
        let p = ix - 1;
        let mut out32_q12 = smulbb(in_[p] as i32, b[0] as i32);
        out32_q12 = smlabb(out32_q12, in_[p - 1] as i32, b[1] as i32);
        out32_q12 = smlabb(out32_q12, in_[p - 2] as i32, b[2] as i32);
        out32_q12 = smlabb(out32_q12, in_[p - 3] as i32, b[3] as i32);
        out32_q12 = smlabb(out32_q12, in_[p - 4] as i32, b[4] as i32);
        out32_q12 = smlabb(out32_q12, in_[p - 5] as i32, b[5] as i32);
        let mut j = 6;
        while j < d {
            out32_q12 = smlabb(out32_q12, in_[p - j] as i32, b[j] as i32);
            out32_q12 = smlabb(out32_q12, in_[p - j - 1] as i32, b[j + 1] as i32);
            j += 2;
        }
        out32_q12 = lshift(in_[ix] as i32, 12).wrapping_sub(out32_q12);
        out[ix] = sat16(rshift_round(out32_q12, 12)) as i16;
    }
    for o in out[..d].iter_mut() {
        *o = 0;
    }
}

/// silk_gains_dequant.
pub fn gains_dequant(gain_q16: &mut [i32; 4], ind: &[i8; 4], prev_ind: &mut i8, conditional: bool, nb_subfr: usize) {
    let mut prev = *prev_ind as i32;
    for k in 0..nb_subfr {
        if k == 0 && !conditional {
            prev = core::cmp::max(ind[k] as i32, prev - 16);
        } else {
            let ind_tmp = ind[k] as i32 + MIN_DELTA_GAIN_QUANT;
            let double_step_size_threshold = 2 * MAX_DELTA_GAIN_QUANT - N_LEVELS_QGAIN + prev;
            if ind_tmp > double_step_size_threshold {
                prev += lshift(ind_tmp, 1) - double_step_size_threshold;
            } else {
                prev += ind_tmp;
            }
        }
        prev = prev.clamp(0, N_LEVELS_QGAIN - 1);
        gain_q16[k] = log2lin(core::cmp::min(smulwb(INV_SCALE_Q16, prev) + GAIN_OFFSET, 3967));
    }
    *prev_ind = prev as i8;
}

/// silk_decode_pitch: the pitch lags of the subframes from the lag index and the contour index.
pub fn decode_pitch(lag_index: i16, contour_index: i8, pitch_lags: &mut [i32; 4], fs_khz: i32, nb_subfr: usize) {
    let min_lag = 2 * fs_khz;
    let max_lag = 18 * fs_khz;
    let lag = min_lag + lag_index as i32;
    let c = contour_index.max(0) as usize;
    for k in 0..nb_subfr {
        let off: i32 = if fs_khz == 8 {
            if nb_subfr == 4 {
                CB_LAGS_STAGE2[k][c.min(10)] as i32
            } else {
                CB_LAGS_STAGE2_10_MS[k][c.min(2)] as i32
            }
        } else if nb_subfr == 4 {
            CB_LAGS_STAGE3[k][c.min(33)] as i32
        } else {
            CB_LAGS_STAGE3_10_MS[k][c.min(11)] as i32
        };
        pitch_lags[k] = (lag + off).clamp(min_lag, max_lag);
    }
}
