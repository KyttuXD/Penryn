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
// Port of the SILK packet loss concealment (PLC.c, without the optional deep PLC) and the comfort noise
// generation (CNG.c) of libopus. Integer arithmetic exactly as libopus.

#![allow(dead_code)]

use super::silk_dec::{DecCtrl, DecState, LTP_ORDER, MAX_FRAME_LENGTH, MAX_NB_SUBFR, TYPE_NO_VOICE_ACTIVITY, TYPE_VOICED};
use super::silk_dsp::*;
use super::silk_math::*;

const MAX_LPC: usize = 16;
const V_PITCH_GAIN_START_MIN_Q14: i32 = 11469; // 0.7 in Q14
const V_PITCH_GAIN_START_MAX_Q14: i32 = 15565; // 0.95 in Q14
const MAX_PITCH_LAG_MS: i32 = 18;
const RAND_BUF_SIZE: usize = 128;
const RAND_BUF_MASK: i32 = (RAND_BUF_SIZE - 1) as i32;
const LOG2_INV_LPC_GAIN_HIGH_THRES: u32 = 3; // 2^3 = 8 dB LPC gain
const LOG2_INV_LPC_GAIN_LOW_THRES: u32 = 8; // 2^8 = 24 dB LPC gain
const PITCH_DRIFT_FAC_Q16: i32 = 655; // 0.01 in Q16
const BWE_COEF_Q16: i32 = 64881; // SILK_FIX_CONST(0.99, 16)
const NB_ATT: usize = 2;
const HARM_ATT_Q15: [i32; NB_ATT] = [32440, 31130]; // 0.99, 0.95
const PLC_RAND_ATTENUATE_V_Q15: [i32; NB_ATT] = [31130, 26214]; // 0.95, 0.8
const PLC_RAND_ATTENUATE_UV_Q15: [i32; NB_ATT] = [32440, 29491]; // 0.99, 0.9

const CNG_BUF_MASK_MAX: i32 = 255;
const CNG_GAIN_SMTH_Q16: i32 = 4634; // 0.25^(1/4)
const CNG_GAIN_SMTH_THRESHOLD_Q16: i32 = 46396; // -3 dB
const CNG_NLSF_SMTH_Q16: i32 = 16348; // 0.25

/// silk_PLC_struct.
pub struct PlcState {
    pub pitch_l_q8: i32,
    pub ltp_coef_q14: [i16; LTP_ORDER],
    pub prev_lpc_q12: [i16; MAX_LPC],
    pub last_frame_lost: bool,
    pub rand_seed: i32,
    pub rand_scale_q14: i16,
    pub conc_energy: i32,
    pub conc_energy_shift: i32,
    pub prev_ltp_scale_q14: i16,
    pub prev_gain_q16: [i32; 2],
    pub fs_khz: i32,
    pub nb_subfr: usize,
    pub subfr_length: usize,
}

impl PlcState {
    pub fn new() -> PlcState {
        PlcState {
            pitch_l_q8: 0,
            ltp_coef_q14: [0; LTP_ORDER],
            prev_lpc_q12: [0; MAX_LPC],
            last_frame_lost: false,
            rand_seed: 0,
            rand_scale_q14: 0,
            conc_energy: 0,
            conc_energy_shift: 0,
            prev_ltp_scale_q14: 0,
            prev_gain_q16: [0; 2],
            fs_khz: 0,
            nb_subfr: 0,
            subfr_length: 0,
        }
    }
}

/// silk_CNG_struct.
pub struct CngState {
    pub exc_buf_q14: [i32; MAX_FRAME_LENGTH],
    pub smth_nlsf_q15: [i16; MAX_LPC],
    pub synth_state: [i32; MAX_LPC],
    pub smth_gain_q16: i32,
    pub rand_seed: i32,
    pub fs_khz: i32,
}

impl CngState {
    pub fn new() -> CngState {
        CngState {
            exc_buf_q14: [0; MAX_FRAME_LENGTH],
            smth_nlsf_q15: [0; MAX_LPC],
            synth_state: [0; MAX_LPC],
            smth_gain_q16: 0,
            rand_seed: 0,
            fs_khz: 0,
        }
    }
}

/// silk_PLC_Reset.
pub fn plc_reset(st: &mut DecState) {
    st.s_plc.pitch_l_q8 = lshift(st.frame_length as i32, 8 - 1);
    st.s_plc.prev_gain_q16[0] = 1 << 16;
    st.s_plc.prev_gain_q16[1] = 1 << 16;
    st.s_plc.subfr_length = 20;
    st.s_plc.nb_subfr = 2;
}

/// silk_PLC: updates the concealment state after a good frame, or generates a concealed frame.
pub fn plc(st: &mut DecState, ctrl: &mut DecCtrl, frame: &mut [i16], lost: bool) {
    if st.fs_khz != st.s_plc.fs_khz {
        plc_reset(st);
        st.s_plc.fs_khz = st.fs_khz;
    }
    if lost {
        plc_conceal(st, ctrl, frame);
        st.loss_cnt += 1;
    } else {
        plc_update(st, ctrl);
    }
}

/// silk_PLC_update: saves the parameters used in case of packet loss.
fn plc_update(st: &mut DecState, ctrl: &DecCtrl) {
    let nb_subfr = st.nb_subfr;
    let subfr_length = st.subfr_length as i32;
    st.prev_signal_type = st.indices.signal_type as i32;
    let mut ltp_gain_q14: i32 = 0;
    let psplc = &mut st.s_plc;
    if st.indices.signal_type as i32 == TYPE_VOICED {
        // Find the parameters for the last subframe which contains a pitch pulse
        let mut j = 0usize;
        while (j as i32) * subfr_length < ctrl.pitch_l[nb_subfr - 1] {
            if j == nb_subfr {
                break;
            }
            let mut temp_ltp_gain_q14: i32 = 0;
            for i in 0..LTP_ORDER {
                temp_ltp_gain_q14 += ctrl.ltp_coef_q14[(nb_subfr - 1 - j) * LTP_ORDER + i] as i32;
            }
            if temp_ltp_gain_q14 > ltp_gain_q14 {
                ltp_gain_q14 = temp_ltp_gain_q14;
                let o = (nb_subfr - 1 - j) * LTP_ORDER;
                psplc.ltp_coef_q14.copy_from_slice(&ctrl.ltp_coef_q14[o..o + LTP_ORDER]);
                psplc.pitch_l_q8 = lshift(ctrl.pitch_l[nb_subfr - 1 - j], 8);
            }
            j += 1;
        }
        psplc.ltp_coef_q14 = [0; LTP_ORDER];
        psplc.ltp_coef_q14[LTP_ORDER / 2] = ltp_gain_q14 as i16;

        // Limit LT coefs
        if ltp_gain_q14 < V_PITCH_GAIN_START_MIN_Q14 {
            let tmp = lshift(V_PITCH_GAIN_START_MIN_Q14, 10);
            let scale_q10 = tmp / core::cmp::max(ltp_gain_q14, 1);
            for i in 0..LTP_ORDER {
                psplc.ltp_coef_q14[i] = (smulbb(psplc.ltp_coef_q14[i] as i32, scale_q10) >> 10) as i16;
            }
        } else if ltp_gain_q14 > V_PITCH_GAIN_START_MAX_Q14 {
            let tmp = lshift(V_PITCH_GAIN_START_MAX_Q14, 14);
            let scale_q14 = tmp / core::cmp::max(ltp_gain_q14, 1);
            for i in 0..LTP_ORDER {
                psplc.ltp_coef_q14[i] = (smulbb(psplc.ltp_coef_q14[i] as i32, scale_q14) >> 14) as i16;
            }
        }
    } else {
        psplc.pitch_l_q8 = lshift(smulbb(st.fs_khz, 18), 8);
        psplc.ltp_coef_q14 = [0; LTP_ORDER];
    }

    // Save LPC coefficients
    psplc.prev_lpc_q12[..st.lpc_order].copy_from_slice(&ctrl.pred_coef_q12[1][..st.lpc_order]);
    psplc.prev_ltp_scale_q14 = ctrl.ltp_scale_q14 as i16;

    // Save last two gains
    psplc.prev_gain_q16.copy_from_slice(&ctrl.gains_q16[nb_subfr - 2..nb_subfr]);

    psplc.subfr_length = st.subfr_length;
    psplc.nb_subfr = nb_subfr;
}

/// silk_PLC_energy: energies of the last two (scaled) excitation subframes.
fn plc_energy(exc_q14: &[i32; MAX_FRAME_LENGTH], prev_gain_q10: &[i32; 2], subfr_length: usize, nb_subfr: usize) -> (i32, i32, i32, i32) {
    let mut exc_buf = [0i16; 2 * 80];
    for k in 0..2 {
        for i in 0..subfr_length {
            exc_buf[k * subfr_length + i] =
                sat16(smulww(exc_q14[i + (k + nb_subfr - 2) * subfr_length], prev_gain_q10[k]) >> 8) as i16;
        }
    }
    let (e1, s1) = sum_sqr_shift(&exc_buf[..subfr_length]);
    let (e2, s2) = sum_sqr_shift(&exc_buf[subfr_length..2 * subfr_length]);
    (e1, s1, e2, s2)
}

/// silk_PLC_conceal: generates a concealed frame (LTP + LPC synthesis of attenuated excitation).
fn plc_conceal(st: &mut DecState, ctrl: &mut DecCtrl, frame: &mut [i16]) {
    let ltp_mem = st.ltp_mem_length;
    let order = st.lpc_order;
    let frame_length = st.frame_length;
    let subfr_length = st.subfr_length;
    let mut s_ltp = [0i16; MAX_FRAME_LENGTH];
    let mut s_ltp_q14 = [0i32; 2 * MAX_FRAME_LENGTH];
    let mut a_q12 = [0i16; MAX_LPC];

    let prev_gain_q10 = [st.s_plc.prev_gain_q16[0] >> 6, st.s_plc.prev_gain_q16[1] >> 6];

    if st.first_frame_after_reset {
        st.s_plc.prev_lpc_q12 = [0; MAX_LPC];
    }

    let (energy1, shift1, energy2, shift2) = plc_energy(&st.exc_q14, &prev_gain_q10, subfr_length, st.nb_subfr);
    let rand_off: usize = if (energy1 >> shift2) < (energy2 >> shift1) {
        // First sub-frame has lowest energy
        core::cmp::max(0, (st.s_plc.nb_subfr as i32 - 1) * st.s_plc.subfr_length as i32 - RAND_BUF_SIZE as i32) as usize
    } else {
        // Second sub-frame has lowest energy
        core::cmp::max(0, st.s_plc.nb_subfr as i32 * st.s_plc.subfr_length as i32 - RAND_BUF_SIZE as i32) as usize
    };

    // Set up Gain to random noise component
    let mut b_q14 = st.s_plc.ltp_coef_q14;
    let mut rand_scale_q14: i16 = st.s_plc.rand_scale_q14;

    // Set up attenuation gains
    let att = core::cmp::min(NB_ATT as i32 - 1, st.loss_cnt) as usize;
    let harm_gain_q15 = HARM_ATT_Q15[att];
    let mut rand_gain_q15 = if st.prev_signal_type == TYPE_VOICED {
        PLC_RAND_ATTENUATE_V_Q15[att]
    } else {
        PLC_RAND_ATTENUATE_UV_Q15[att]
    };

    // LPC concealment. Apply BWE to previous LPC
    bwexpander(&mut st.s_plc.prev_lpc_q12, order, BWE_COEF_Q16);

    // Preload LPC coefficients
    a_q12[..order].copy_from_slice(&st.s_plc.prev_lpc_q12[..order]);

    // First Lost frame
    if st.loss_cnt == 0 {
        rand_scale_q14 = 1 << 14;
        // Reduce random noise Gain for voiced frames
        if st.prev_signal_type == TYPE_VOICED {
            for i in 0..LTP_ORDER {
                rand_scale_q14 = (rand_scale_q14 as i32 - b_q14[i] as i32) as i16;
            }
            rand_scale_q14 = core::cmp::max(3277, rand_scale_q14); // 0.2
            rand_scale_q14 = (smulbb(rand_scale_q14 as i32, st.s_plc.prev_ltp_scale_q14 as i32) >> 14) as i16;
        } else {
            // Reduce random noise for unvoiced frames with high LPC gain
            let inv_gain_q30 = lpc_inverse_pred_gain(&st.s_plc.prev_lpc_q12, order);
            let mut down_scale_q30 = core::cmp::min((1i32 << 30) >> LOG2_INV_LPC_GAIN_HIGH_THRES, inv_gain_q30);
            down_scale_q30 = core::cmp::max((1i32 << 30) >> LOG2_INV_LPC_GAIN_LOW_THRES, down_scale_q30);
            down_scale_q30 = lshift(down_scale_q30, LOG2_INV_LPC_GAIN_HIGH_THRES);
            rand_gain_q15 = smulwb(down_scale_q30, rand_gain_q15) >> 14;
        }
    }

    let mut rand_seed = st.s_plc.rand_seed;
    let min_lag = 2 * st.fs_khz;
    let max_lag = 18 * st.fs_khz;
    let mut lag = rshift_round(st.s_plc.pitch_l_q8, 8).clamp(min_lag, max_lag);
    let mut s_ltp_buf_idx = ltp_mem;

    // Rewhiten LTP state
    let idx = ltp_mem - lag as usize - order - LTP_ORDER / 2;
    lpc_analysis_filter(&mut s_ltp[idx..], &st.out_buf[idx..], &a_q12, ltp_mem - idx, order);

    // Scale LTP state
    let mut inv_gain_q30 = inverse32_var_q(st.s_plc.prev_gain_q16[1], 46);
    inv_gain_q30 = core::cmp::min(inv_gain_q30, i32::MAX >> 1);
    for i in idx + order..ltp_mem {
        s_ltp_q14[i] = smulwb(inv_gain_q30, s_ltp[i] as i32);
    }

    // LTP synthesis filtering
    for _k in 0..st.nb_subfr {
        let mut base = s_ltp_buf_idx - lag as usize + LTP_ORDER / 2;
        for _i in 0..subfr_length {
            // Avoids introducing a bias because silk_SMLAWB() always rounds to -inf
            let mut ltp_pred_q12: i32 = 2;
            ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[base], b_q14[0] as i32);
            ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[base - 1], b_q14[1] as i32);
            ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[base - 2], b_q14[2] as i32);
            ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[base - 3], b_q14[3] as i32);
            ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[base - 4], b_q14[4] as i32);
            base += 1;

            // Generate LPC excitation
            rand_seed = rand(rand_seed);
            let ridx = ((rand_seed >> 25) & RAND_BUF_MASK) as usize;
            s_ltp_q14[s_ltp_buf_idx] = lshift(smlawb(ltp_pred_q12, st.exc_q14[rand_off + ridx], rand_scale_q14 as i32), 2);
            s_ltp_buf_idx += 1;
        }

        // Gradually reduce LTP gain
        for j in 0..LTP_ORDER {
            b_q14[j] = (smulbb(harm_gain_q15, b_q14[j] as i32) >> 15) as i16;
        }
        // Gradually reduce excitation gain
        rand_scale_q14 = (smulbb(rand_scale_q14 as i32, rand_gain_q15) >> 15) as i16;

        // Slowly increase pitch lag
        st.s_plc.pitch_l_q8 = smlawb(st.s_plc.pitch_l_q8, st.s_plc.pitch_l_q8, PITCH_DRIFT_FAC_Q16);
        st.s_plc.pitch_l_q8 = core::cmp::min(st.s_plc.pitch_l_q8, lshift(smulbb(MAX_PITCH_LAG_MS, st.fs_khz), 8));
        lag = rshift_round(st.s_plc.pitch_l_q8, 8).clamp(min_lag, max_lag);
    }
    st.s_plc.ltp_coef_q14 = b_q14;

    // LPC synthesis filtering: sLPC_Q14_ptr = &sLTP_Q14[ltp_mem - MAX_LPC_ORDER]
    let p0 = ltp_mem - MAX_LPC;
    s_ltp_q14[p0..p0 + MAX_LPC].copy_from_slice(&st.s_lpc_q14_buf);

    for i in 0..frame_length {
        let mut lpc_pred_q10: i32 = (order >> 1) as i32;
        for j in 0..order {
            lpc_pred_q10 = smlawb(lpc_pred_q10, s_ltp_q14[p0 + MAX_LPC + i - 1 - j], a_q12[j] as i32);
        }
        // Add prediction to LPC excitation
        let o = p0 + MAX_LPC + i;
        s_ltp_q14[o] = add_sat32(s_ltp_q14[o], lshift_sat32(lpc_pred_q10, 4));
        // Scale with Gain
        frame[i] = sat16(sat16(rshift_round(smulww(s_ltp_q14[o], prev_gain_q10[1]), 8))) as i16;
    }

    // Save LPC state
    st.s_lpc_q14_buf.copy_from_slice(&s_ltp_q14[p0 + frame_length..p0 + frame_length + MAX_LPC]);

    // Update states
    st.s_plc.rand_seed = rand_seed;
    st.s_plc.rand_scale_q14 = rand_scale_q14;
    for i in 0..MAX_NB_SUBFR {
        ctrl.pitch_l[i] = lag;
    }
}

/// silk_PLC_glue_frames: glues concealed frames with new good received frames.
pub fn plc_glue_frames(st: &mut DecState, frame: &mut [i16], length: usize) {
    let skip_fade = st.wb_glue_off && st.s_plc.fs_khz == 16;
    let psplc = &mut st.s_plc;
    if st.loss_cnt != 0 {
        // Calculate energy in concealed residual
        let (e, s) = sum_sqr_shift(&frame[..length]);
        psplc.conc_energy = e;
        psplc.conc_energy_shift = s;
        psplc.last_frame_lost = true;
    } else {
        if psplc.last_frame_lost {
            // Calculate residual in decoded signal if last frame was lost
            let (mut energy, energy_shift) = sum_sqr_shift(&frame[..length]);

            // Normalize energies
            if energy_shift > psplc.conc_energy_shift {
                psplc.conc_energy >>= energy_shift - psplc.conc_energy_shift;
            } else if energy_shift < psplc.conc_energy_shift {
                energy >>= psplc.conc_energy_shift - energy_shift;
            }

            // Fade in the energy difference
            if energy > psplc.conc_energy {
                let lz = clz32(psplc.conc_energy) - 1;
                psplc.conc_energy = lshift(psplc.conc_energy, lz.max(0) as u32);
                energy >>= core::cmp::max(24 - lz, 0);

                let frac_q24 = psplc.conc_energy / core::cmp::max(energy, 1);

                let mut gain_q16 = lshift(sqrt_approx(frac_q24), 4);
                let mut slope_q16 = ((1i32 << 16) - gain_q16) / length as i32;
                // Make slope 4x steeper to avoid missing onsets after DTX
                slope_q16 = lshift(slope_q16, 2);

                // libopus built with the deep PLC skips this fade-in at 16 kHz (`wb_glue_off`)
                if !skip_fade {
                    for i in 0..length {
                        frame[i] = smulwb(gain_q16, frame[i] as i32) as i16;
                        gain_q16 = gain_q16.wrapping_add(slope_q16);
                        if gain_q16 > (1i32 << 16) {
                            break;
                        }
                    }
                }
            }
        }
        psplc.last_frame_lost = false;
    }
}

/// silk_CNG_Reset.
pub fn cng_reset(st: &mut DecState) {
    let order = st.lpc_order as i32;
    let nlsf_step_q15 = 32767 / (order + 1);
    let mut nlsf_acc_q15: i32 = 0;
    for i in 0..st.lpc_order {
        nlsf_acc_q15 += nlsf_step_q15;
        st.s_cng.smth_nlsf_q15[i] = nlsf_acc_q15 as i16;
    }
    st.s_cng.smth_gain_q16 = 0;
    st.s_cng.rand_seed = 3176576;
}

/// silk_CNG_exc: excitation for the CNG LPC synthesis.
fn cng_exc(exc_q14: &mut [i32], exc_buf_q14: &[i32; MAX_FRAME_LENGTH], length: usize, rand_seed: &mut i32) {
    let mut exc_mask = CNG_BUF_MASK_MAX;
    while exc_mask > length as i32 {
        exc_mask >>= 1;
    }
    let mut seed = *rand_seed;
    for i in 0..length {
        seed = rand(seed);
        let idx = ((seed >> 24) & exc_mask) as usize;
        exc_q14[i] = exc_buf_q14[idx];
    }
    *rand_seed = seed;
}

/// silk_CNG: updates the CNG estimate, and applies the CNG when a packet was lost.
pub fn cng(st: &mut DecState, ctrl: &DecCtrl, frame: &mut [i16], length: usize) {
    let order = st.lpc_order;
    if st.fs_khz != st.s_cng.fs_khz {
        cng_reset(st);
        st.s_cng.fs_khz = st.fs_khz;
    }
    if st.loss_cnt == 0 && st.prev_signal_type == TYPE_NO_VOICE_ACTIVITY {
        // Update CNG parameters: smoothing of LSF's
        for i in 0..order {
            let d = st.prev_nlsf_q15[i] as i32 - st.s_cng.smth_nlsf_q15[i] as i32;
            st.s_cng.smth_nlsf_q15[i] = (st.s_cng.smth_nlsf_q15[i] as i32 + smulwb(d, CNG_NLSF_SMTH_Q16)) as i16;
        }
        // Find the subframe with the highest gain
        let mut max_gain_q16: i32 = 0;
        let mut subfr = 0usize;
        for i in 0..st.nb_subfr {
            if ctrl.gains_q16[i] > max_gain_q16 {
                max_gain_q16 = ctrl.gains_q16[i];
                subfr = i;
            }
        }
        // Update CNG excitation buffer with excitation from this subframe
        let sl = st.subfr_length;
        st.s_cng.exc_buf_q14.copy_within(0..(st.nb_subfr - 1) * sl, sl);
        st.s_cng.exc_buf_q14[..sl].copy_from_slice(&st.exc_q14[subfr * sl..(subfr + 1) * sl]);

        // Smooth gains
        for i in 0..st.nb_subfr {
            st.s_cng.smth_gain_q16 = st.s_cng.smth_gain_q16.wrapping_add(smulwb(ctrl.gains_q16[i].wrapping_sub(st.s_cng.smth_gain_q16), CNG_GAIN_SMTH_Q16));
            // If the smoothed gain is 3 dB greater than this subframe's gain, use this subframe's gain to adapt faster.
            if smulww(st.s_cng.smth_gain_q16, CNG_GAIN_SMTH_THRESHOLD_Q16) > ctrl.gains_q16[i] {
                st.s_cng.smth_gain_q16 = ctrl.gains_q16[i];
            }
        }
    }

    // Add CNG when packet is lost or during DTX
    if st.loss_cnt != 0 {
        let mut cng_sig_q14 = [0i32; MAX_FRAME_LENGTH + MAX_LPC];
        let mut a_q12 = [0i16; MAX_LPC];

        // Generate CNG excitation
        let mut gain_q16 = smulww(st.s_plc.rand_scale_q14 as i32, st.s_plc.prev_gain_q16[1]);
        if gain_q16 >= (1 << 21) || st.s_cng.smth_gain_q16 > (1 << 23) {
            gain_q16 = smultt(gain_q16, gain_q16);
            gain_q16 = smultt(st.s_cng.smth_gain_q16, st.s_cng.smth_gain_q16).wrapping_sub(lshift(gain_q16, 5));
            gain_q16 = lshift(sqrt_approx(gain_q16), 16);
        } else {
            gain_q16 = smulww(gain_q16, gain_q16);
            gain_q16 = smulww(st.s_cng.smth_gain_q16, st.s_cng.smth_gain_q16).wrapping_sub(lshift(gain_q16, 5));
            gain_q16 = lshift(sqrt_approx(gain_q16), 8);
        }
        let gain_q10 = gain_q16 >> 6;

        cng_exc(&mut cng_sig_q14[MAX_LPC..], &st.s_cng.exc_buf_q14, length, &mut st.s_cng.rand_seed);

        // Convert CNG NLSF to filter representation
        nlsf2a(&mut a_q12, &st.s_cng.smth_nlsf_q15, order);

        // Generate CNG signal, by synthesis filtering
        cng_sig_q14[..MAX_LPC].copy_from_slice(&st.s_cng.synth_state);
        for i in 0..length {
            // Avoids introducing a bias because silk_SMLAWB() always rounds to -inf
            let mut lpc_pred_q10: i32 = (order >> 1) as i32;
            for j in 0..order {
                lpc_pred_q10 = smlawb(lpc_pred_q10, cng_sig_q14[MAX_LPC + i - 1 - j], a_q12[j] as i32);
            }
            // Update states
            cng_sig_q14[MAX_LPC + i] = add_sat32(cng_sig_q14[MAX_LPC + i], lshift_sat32(lpc_pred_q10, 4));
            // Scale with Gain and add to input signal
            frame[i] = add_sat16(frame[i] as i32, sat16(rshift_round(smulww(cng_sig_q14[MAX_LPC + i], gain_q10), 8))) as i16;
        }
        st.s_cng.synth_state.copy_from_slice(&cng_sig_q14[length..length + MAX_LPC]);
    } else {
        for v in st.s_cng.synth_state[..order].iter_mut() {
            *v = 0;
        }
    }
}
