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
// Port of the SILK decoder of libopus (mono): dec_API.c (silk_Decode flow for one packet), decode_frame.c,
// decode_indices.c, decode_parameters.c, decode_core.c, decoder_set_fs.c, init_decoder.c. The loss
// concealment and the comfort noise live in silk_plc.rs, the excitation decoding in silk_pulses.rs, the
// LPC/NLSF helpers in silk_dsp.rs, the output resampler in silk_resampler.rs and the tables (generated from
// the C sources by gen_tables.py) in silk_tables.rs. The integer arithmetic is that of libopus, so the
// decoded 16-bit samples at the internal rate are bit-exact.

#![allow(dead_code)]

use super::rangedec::RangeDecoder;
use super::silk_dsp::*;
use super::silk_math::*;
use super::silk_plc::{cng, cng_reset, plc, plc_glue_frames, plc_reset, CngState, PlcState};
use super::silk_pulses::decode_pulses;
use super::silk_resampler::Resampler;
use super::silk_tables::*;

pub const MAX_NB_SUBFR: usize = 4;
pub const LTP_ORDER: usize = 5;
pub const MAX_FRAME_LENGTH: usize = 320;
pub const MAX_SUB_FRAME_LENGTH: usize = 80;
pub const TYPE_NO_VOICE_ACTIVITY: i32 = 0;
pub const TYPE_UNVOICED: i32 = 1;
pub const TYPE_VOICED: i32 = 2;
pub const CODE_INDEPENDENTLY: i32 = 0;
pub const CODE_INDEPENDENTLY_NO_LTP_SCALING: i32 = 1;
pub const CODE_CONDITIONALLY: i32 = 2;
pub const FLAG_DECODE_NORMAL: i32 = 0;
pub const FLAG_PACKET_LOST: i32 = 1;
pub const FLAG_DECODE_LBRR: i32 = 2;
const BWE_AFTER_LOSS_Q16: i32 = 63570;
const QUANT_LEVEL_ADJUST_Q10: i32 = 80;

/// The quantisation indices of one frame (SideInfoIndices).
#[derive(Clone, Copy)]
pub struct Indices {
    pub gains_indices: [i8; MAX_NB_SUBFR],
    pub ltp_index: [i8; MAX_NB_SUBFR],
    pub nlsf_indices: [i8; 17],
    pub lag_index: i16,
    pub contour_index: i8,
    pub signal_type: i8,
    pub quant_offset_type: i8,
    pub nlsf_interp_coef_q2: i8,
    pub per_index: i8,
    pub ltp_scale_index: i8,
    pub seed: i8,
}

impl Indices {
    const ZERO: Indices = Indices {
        gains_indices: [0; MAX_NB_SUBFR],
        ltp_index: [0; MAX_NB_SUBFR],
        nlsf_indices: [0; 17],
        lag_index: 0,
        contour_index: 0,
        signal_type: 0,
        quant_offset_type: 0,
        nlsf_interp_coef_q2: 0,
        per_index: 0,
        ltp_scale_index: 0,
        seed: 0,
    };
}

/// silk_decoder_control: the decoded parameters of one frame.
pub struct DecCtrl {
    pub pitch_l: [i32; MAX_NB_SUBFR],
    pub gains_q16: [i32; MAX_NB_SUBFR],
    pub pred_coef_q12: [[i16; 16]; 2],
    pub ltp_coef_q14: [i16; LTP_ORDER * MAX_NB_SUBFR],
    pub ltp_scale_q14: i32,
}

impl DecCtrl {
    fn new() -> DecCtrl {
        DecCtrl {
            pitch_l: [0; MAX_NB_SUBFR],
            gains_q16: [0; MAX_NB_SUBFR],
            pred_coef_q12: [[0; 16]; 2],
            ltp_coef_q14: [0; LTP_ORDER * MAX_NB_SUBFR],
            ltp_scale_q14: 0,
        }
    }
}

/// silk_decoder_state: one channel's decoder state.
pub struct DecState {
    pub prev_gain_q16: i32,
    pub exc_q14: [i32; MAX_FRAME_LENGTH],
    pub s_lpc_q14_buf: [i32; 16],
    pub out_buf: [i16; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
    pub lag_prev: i32,
    pub last_gain_index: i8,
    pub fs_khz: i32,
    pub fs_api_hz: i32,
    pub nb_subfr: usize,
    pub frame_length: usize,
    pub subfr_length: usize,
    pub ltp_mem_length: usize,
    pub lpc_order: usize,
    pub prev_nlsf_q15: [i16; 16],
    pub first_frame_after_reset: bool,
    pub pitch_lag_low_bits_icdf: &'static [u8],
    pub pitch_contour_icdf: &'static [u8],
    pub n_frames_decoded: usize,
    pub n_frames_per_packet: usize,
    pub ec_prev_signal_type: i32,
    pub ec_prev_lag_index: i16,
    pub vad_flags: [i32; 3],
    pub lbrr_flag: i32,
    pub lbrr_flags: [i32; 3],
    pub resampler: Option<Resampler>,
    pub nlsf_cb: &'static NlsfCb,
    pub indices: Indices,
    pub s_cng: CngState,
    pub loss_cnt: i32,
    pub prev_signal_type: i32,
    pub s_plc: PlcState,
    /// Compatibility with libopus builds that have OSCE enabled: the decoded 16 kHz / 20 ms frames are clamped
    /// to >= -32767 (the OSCE output stage does so). Configuration, not state: survives `reset`.
    pub osce_clamp: bool,
    /// Compatibility with libopus builds that have the deep PLC enabled: no energy fade-in after a concealed
    /// frame at 16 kHz. Configuration, not state: survives `reset`.
    pub wb_glue_off: bool,
}

impl DecState {
    /// silk_init_decoder (= memset + silk_reset_decoder).
    pub fn new() -> DecState {
        let mut s = DecState {
            prev_gain_q16: 0,
            exc_q14: [0; MAX_FRAME_LENGTH],
            s_lpc_q14_buf: [0; 16],
            out_buf: [0; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
            lag_prev: 0,
            last_gain_index: 0,
            fs_khz: 0,
            fs_api_hz: 0,
            nb_subfr: 0,
            frame_length: 0,
            subfr_length: 0,
            ltp_mem_length: 0,
            lpc_order: 0,
            prev_nlsf_q15: [0; 16],
            first_frame_after_reset: false,
            pitch_lag_low_bits_icdf: &UNIFORM4_ICDF,
            pitch_contour_icdf: &PITCH_CONTOUR_ICDF,
            n_frames_decoded: 0,
            n_frames_per_packet: 0,
            ec_prev_signal_type: 0,
            ec_prev_lag_index: 0,
            vad_flags: [0; 3],
            lbrr_flag: 0,
            lbrr_flags: [0; 3],
            resampler: None,
            nlsf_cb: &NLSF_CB_NB_MB,
            indices: Indices::ZERO,
            s_cng: CngState::new(),
            loss_cnt: 0,
            prev_signal_type: 0,
            s_plc: PlcState::new(),
            osce_clamp: true,
            wb_glue_off: true,
        };
        s.reset();
        s
    }

    /// silk_reset_decoder: clears everything (the whole state is "reset" state here).
    pub fn reset(&mut self) {
        self.prev_gain_q16 = 0;
        self.exc_q14 = [0; MAX_FRAME_LENGTH];
        self.s_lpc_q14_buf = [0; 16];
        self.out_buf = [0; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH];
        self.lag_prev = 0;
        self.last_gain_index = 0;
        self.fs_khz = 0;
        self.fs_api_hz = 0;
        self.nb_subfr = 0;
        self.frame_length = 0;
        self.subfr_length = 0;
        self.ltp_mem_length = 0;
        self.lpc_order = 0;
        self.prev_nlsf_q15 = [0; 16];
        self.pitch_lag_low_bits_icdf = &UNIFORM4_ICDF;
        self.pitch_contour_icdf = &PITCH_CONTOUR_ICDF;
        self.n_frames_decoded = 0;
        self.n_frames_per_packet = 0;
        self.ec_prev_signal_type = 0;
        self.ec_prev_lag_index = 0;
        self.vad_flags = [0; 3];
        self.lbrr_flag = 0;
        self.lbrr_flags = [0; 3];
        self.resampler = None;
        self.nlsf_cb = &NLSF_CB_NB_MB;
        self.indices = Indices::ZERO;
        self.s_cng = CngState::new();
        self.loss_cnt = 0;
        self.prev_signal_type = 0;
        self.s_plc = PlcState::new();
        // Used to deactivate LSF interpolation
        self.first_frame_after_reset = true;
        self.prev_gain_q16 = 65536;
        cng_reset(self);
        plc_reset(self);
    }

    /// silk_decoder_set_fs: sets the internal rate (8, 12 or 16 kHz) and the API rate; `nb_subfr` must be set.
    pub fn set_fs(&mut self, fs_khz: i32, fs_api_hz: i32) {
        self.subfr_length = 5 * fs_khz as usize;
        let frame_length = self.nb_subfr * self.subfr_length;

        if self.fs_khz != fs_khz || self.fs_api_hz != fs_api_hz {
            self.resampler = Resampler::new(fs_khz * 1000, fs_api_hz);
            self.fs_api_hz = fs_api_hz;
        }

        if self.fs_khz != fs_khz || frame_length != self.frame_length {
            if fs_khz == 8 {
                self.pitch_contour_icdf = if self.nb_subfr == MAX_NB_SUBFR {
                    &PITCH_CONTOUR_NB_ICDF
                } else {
                    &PITCH_CONTOUR_10_MS_NB_ICDF
                };
            } else {
                self.pitch_contour_icdf = if self.nb_subfr == MAX_NB_SUBFR {
                    &PITCH_CONTOUR_ICDF
                } else {
                    &PITCH_CONTOUR_10_MS_ICDF
                };
            }
            if self.fs_khz != fs_khz {
                self.ltp_mem_length = 20 * fs_khz as usize;
                if fs_khz == 8 || fs_khz == 12 {
                    self.lpc_order = 10;
                    self.nlsf_cb = &NLSF_CB_NB_MB;
                } else {
                    self.lpc_order = 16;
                    self.nlsf_cb = &NLSF_CB_WB;
                }
                self.pitch_lag_low_bits_icdf = if fs_khz == 16 {
                    &UNIFORM8_ICDF
                } else if fs_khz == 12 {
                    &UNIFORM6_ICDF
                } else {
                    &UNIFORM4_ICDF
                };
                self.first_frame_after_reset = true;
                self.lag_prev = 100;
                self.last_gain_index = 10;
                self.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
                self.out_buf = [0; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH];
                self.s_lpc_q14_buf = [0; 16];
            }
            self.fs_khz = fs_khz;
            self.frame_length = frame_length;
        }
    }
}

#[inline]
fn ltp_gain_icdf(per_index: usize) -> &'static [u8] {
    match per_index {
        0 => &LTP_GAIN_ICDF_0,
        1 => &LTP_GAIN_ICDF_1,
        _ => &LTP_GAIN_ICDF_2,
    }
}

/// silk_decode_indices: the side information of one frame.
pub fn decode_indices(st: &mut DecState, rc: &mut RangeDecoder, frame_index: usize, decode_lbrr: bool, cond_coding: i32) {
    // Signal type and quantiser offset
    let ix = if decode_lbrr || st.vad_flags[frame_index] != 0 {
        rc.icdf(&TYPE_OFFSET_VAD_ICDF, 8) + 2
    } else {
        rc.icdf(&TYPE_OFFSET_NO_VAD_ICDF, 8)
    };
    st.indices.signal_type = (ix >> 1) as i8;
    st.indices.quant_offset_type = (ix & 1) as i8;
    let signal_type = st.indices.signal_type as i32;

    // Gains: first subframe
    if cond_coding == CODE_CONDITIONALLY {
        st.indices.gains_indices[0] = rc.icdf(&DELTA_GAIN_ICDF, 8) as i8;
    } else {
        let hi = rc.icdf(&GAIN_ICDF[signal_type as usize], 8);
        st.indices.gains_indices[0] = (hi << 3) as i8;
        st.indices.gains_indices[0] = st.indices.gains_indices[0].wrapping_add(rc.icdf(&UNIFORM8_ICDF, 8) as i8);
    }
    for i in 1..st.nb_subfr {
        st.indices.gains_indices[i] = rc.icdf(&DELTA_GAIN_ICDF, 8) as i8;
    }

    // LSF indices
    let cb = st.nlsf_cb;
    let n_vectors = cb.n_vectors as usize;
    st.indices.nlsf_indices[0] = rc.icdf(&cb.cb1_icdf[(signal_type as usize >> 1) * n_vectors..], 8) as i8;
    let mut ec_ix = [0i16; MAX_LPC_ORDER];
    let mut pred_q8 = [0u8; MAX_LPC_ORDER];
    nlsf_unpack(&mut ec_ix, &mut pred_q8, cb, st.indices.nlsf_indices[0] as usize);
    for i in 0..cb.order as usize {
        let mut ix = rc.icdf(&cb.ec_icdf[ec_ix[i] as usize..], 8);
        if ix == 0 {
            ix -= rc.icdf(&NLSF_EXT_ICDF, 8);
        } else if ix == 2 * NLSF_QUANT_MAX_AMPLITUDE {
            ix += rc.icdf(&NLSF_EXT_ICDF, 8);
        }
        st.indices.nlsf_indices[i + 1] = (ix - NLSF_QUANT_MAX_AMPLITUDE) as i8;
    }

    // LSF interpolation factor
    if st.nb_subfr == MAX_NB_SUBFR {
        st.indices.nlsf_interp_coef_q2 = rc.icdf(&NLSF_INTERPOLATION_FACTOR_ICDF, 8) as i8;
    } else {
        st.indices.nlsf_interp_coef_q2 = 4;
    }

    if signal_type == TYPE_VOICED {
        // Pitch lags
        let mut decode_absolute_lag_index = true;
        if cond_coding == CODE_CONDITIONALLY && st.ec_prev_signal_type == TYPE_VOICED {
            let mut delta_lag_index = rc.icdf(&PITCH_DELTA_ICDF, 8) as i16;
            if delta_lag_index > 0 {
                delta_lag_index -= 9;
                st.indices.lag_index = st.ec_prev_lag_index.wrapping_add(delta_lag_index);
                decode_absolute_lag_index = false;
            }
        }
        if decode_absolute_lag_index {
            st.indices.lag_index = (rc.icdf(&PITCH_LAG_ICDF, 8) as i16).wrapping_mul((st.fs_khz >> 1) as i16);
            st.indices.lag_index = st.indices.lag_index.wrapping_add(rc.icdf(st.pitch_lag_low_bits_icdf, 8) as i16);
        }
        st.ec_prev_lag_index = st.indices.lag_index;

        st.indices.contour_index = rc.icdf(st.pitch_contour_icdf, 8) as i8;

        // LTP gains
        st.indices.per_index = rc.icdf(&LTP_PER_INDEX_ICDF, 8) as i8;
        for k in 0..st.nb_subfr {
            st.indices.ltp_index[k] = rc.icdf(ltp_gain_icdf(st.indices.per_index as usize), 8) as i8;
        }

        // LTP scaling
        if cond_coding == CODE_INDEPENDENTLY {
            st.indices.ltp_scale_index = rc.icdf(&LTPSCALE_ICDF, 8) as i8;
        } else {
            st.indices.ltp_scale_index = 0;
        }
    }
    st.ec_prev_signal_type = signal_type;

    // Seed
    st.indices.seed = rc.icdf(&UNIFORM4_ICDF, 8) as i8;
}

/// silk_decode_parameters: gains, NLSFs -> LPC, pitch lags and LTP coefficients from the indices.
fn decode_parameters(st: &mut DecState, ctrl: &mut DecCtrl, cond_coding: i32) {
    gains_dequant(&mut ctrl.gains_q16, &st.indices.gains_indices, &mut st.last_gain_index, cond_coding == CODE_CONDITIONALLY, st.nb_subfr);

    // Decode NLSFs
    let mut nlsf_q15 = [0i16; MAX_LPC_ORDER];
    nlsf_decode(&mut nlsf_q15, &st.indices.nlsf_indices, st.nlsf_cb);
    let order = st.lpc_order;
    nlsf2a(&mut ctrl.pred_coef_q12[1], &nlsf_q15, order);

    // If just reset, e.g., because internal Fs changed, do not allow interpolation
    if st.first_frame_after_reset {
        st.indices.nlsf_interp_coef_q2 = 4;
    }

    if st.indices.nlsf_interp_coef_q2 < 4 {
        let mut nlsf0_q15 = [0i16; MAX_LPC_ORDER];
        for i in 0..order {
            nlsf0_q15[i] = (st.prev_nlsf_q15[i] as i32
                + (((st.indices.nlsf_interp_coef_q2 as i32).wrapping_mul(nlsf_q15[i] as i32 - st.prev_nlsf_q15[i] as i32)) >> 2))
                as i16;
        }
        nlsf2a(&mut ctrl.pred_coef_q12[0], &nlsf0_q15, order);
    } else {
        let (a, b) = ctrl.pred_coef_q12.split_at_mut(1);
        a[0][..order].copy_from_slice(&b[0][..order]);
    }

    st.prev_nlsf_q15[..order].copy_from_slice(&nlsf_q15[..order]);

    // After a packet loss do BWE of LPC coefs
    if st.loss_cnt != 0 {
        bwexpander(&mut ctrl.pred_coef_q12[0], order, BWE_AFTER_LOSS_Q16);
        bwexpander(&mut ctrl.pred_coef_q12[1], order, BWE_AFTER_LOSS_Q16);
    }

    if st.indices.signal_type as i32 == TYPE_VOICED {
        decode_pitch(st.indices.lag_index, st.indices.contour_index, &mut ctrl.pitch_l, st.fs_khz, st.nb_subfr);

        let per = (st.indices.per_index as usize).min(2);
        for k in 0..st.nb_subfr {
            let ix = st.indices.ltp_index[k] as usize;
            for i in 0..LTP_ORDER {
                let v: i8 = match per {
                    0 => LTP_GAIN_VQ_0[ix.min(7)][i],
                    1 => LTP_GAIN_VQ_1[ix.min(15)][i],
                    _ => LTP_GAIN_VQ_2[ix.min(31)][i],
                };
                ctrl.ltp_coef_q14[k * LTP_ORDER + i] = lshift(v as i32, 7) as i16;
            }
        }

        let ix = (st.indices.ltp_scale_index as usize).min(2);
        ctrl.ltp_scale_q14 = LTPSCALES_TABLE_Q14[ix] as i32;
    } else {
        for v in ctrl.pitch_l[..st.nb_subfr].iter_mut() {
            *v = 0;
        }
        for v in ctrl.ltp_coef_q14[..LTP_ORDER * st.nb_subfr].iter_mut() {
            *v = 0;
        }
        st.indices.per_index = 0;
        ctrl.ltp_scale_q14 = 0;
    }
}

/// silk_decode_core: the inverse noise shaping quantiser (excitation -> LTP -> LPC synthesis).
fn decode_core(st: &mut DecState, ctrl: &mut DecCtrl, xq: &mut [i16], pulses: &[i16]) {
    let ltp_mem = st.ltp_mem_length;
    let frame_length = st.frame_length;
    let subfr_length = st.subfr_length;
    let order = st.lpc_order;
    let mut s_ltp = [0i16; MAX_FRAME_LENGTH];
    let mut s_ltp_q15 = [0i32; 2 * MAX_FRAME_LENGTH];
    let mut pres_q14 = [0i32; MAX_SUB_FRAME_LENGTH];
    let mut s_lpc_q14 = [0i32; MAX_SUB_FRAME_LENGTH + MAX_LPC_ORDER];

    let offset_q10 = QUANTIZATION_OFFSETS_Q10[(st.indices.signal_type >> 1) as usize][(st.indices.quant_offset_type & 1) as usize] as i32;
    let nlsf_interpolation_flag = st.indices.nlsf_interp_coef_q2 < 4;

    // Decode excitation
    let mut rand_seed: i32 = st.indices.seed as i32;
    for i in 0..frame_length {
        rand_seed = rand(rand_seed);
        let mut e = lshift(pulses[i] as i32, 14);
        if e > 0 {
            e -= QUANT_LEVEL_ADJUST_Q10 << 4;
        } else if e < 0 {
            e += QUANT_LEVEL_ADJUST_Q10 << 4;
        }
        e += offset_q10 << 4;
        if rand_seed < 0 {
            e = e.wrapping_neg();
        }
        st.exc_q14[i] = e;
        rand_seed = rand_seed.wrapping_add(pulses[i] as i32);
    }

    // Copy LPC state
    s_lpc_q14[..MAX_LPC_ORDER].copy_from_slice(&st.s_lpc_q14_buf);

    let mut pexc = 0usize;
    let mut pxq = 0usize;
    let mut s_ltp_buf_idx = ltp_mem;
    let min_lag = 2 * st.fs_khz;
    let max_lag = 18 * st.fs_khz;
    let mut lag: i32 = 0;

    for k in 0..st.nb_subfr {
        let a_q12: [i16; 16] = ctrl.pred_coef_q12[k >> 1];
        let b_off = k * LTP_ORDER;
        let mut signal_type = st.indices.signal_type as i32;
        let gain_q10 = ctrl.gains_q16[k] >> 6;
        let mut inv_gain_q31 = inverse32_var_q(ctrl.gains_q16[k], 47);

        // Calculate gain adjustment factor
        let gain_adj_q16: i32;
        if ctrl.gains_q16[k] != st.prev_gain_q16 {
            gain_adj_q16 = div32_var_q(st.prev_gain_q16, ctrl.gains_q16[k], 16);
            for i in 0..MAX_LPC_ORDER {
                s_lpc_q14[i] = smulww(gain_adj_q16, s_lpc_q14[i]);
            }
        } else {
            gain_adj_q16 = 1 << 16;
        }
        st.prev_gain_q16 = ctrl.gains_q16[k];

        // Avoid abrupt transition from voiced PLC to unvoiced normal decoding
        if st.loss_cnt != 0 && st.prev_signal_type == TYPE_VOICED && st.indices.signal_type as i32 != TYPE_VOICED && k < MAX_NB_SUBFR / 2 {
            for v in ctrl.ltp_coef_q14[b_off..b_off + LTP_ORDER].iter_mut() {
                *v = 0;
            }
            ctrl.ltp_coef_q14[b_off + LTP_ORDER / 2] = 4096; // SILK_FIX_CONST(0.25, 14)
            signal_type = TYPE_VOICED;
            ctrl.pitch_l[k] = st.lag_prev;
        }

        if signal_type == TYPE_VOICED {
            lag = ctrl.pitch_l[k].clamp(min_lag, max_lag);
            // Re-whitening
            if k == 0 || (k == 2 && nlsf_interpolation_flag) {
                let start_idx = ltp_mem - lag as usize - order - LTP_ORDER / 2;
                if k == 2 {
                    st.out_buf[ltp_mem..ltp_mem + 2 * subfr_length].copy_from_slice(&xq[..2 * subfr_length]);
                }
                lpc_analysis_filter(
                    &mut s_ltp[start_idx..],
                    &st.out_buf[start_idx + k * subfr_length..],
                    &a_q12,
                    ltp_mem - start_idx,
                    order,
                );
                // After rewhitening the LTP state is unscaled
                if k == 0 {
                    // Do LTP downscaling to reduce inter-packet dependency
                    inv_gain_q31 = lshift(smulwb(inv_gain_q31, ctrl.ltp_scale_q14), 2);
                }
                for i in 0..(lag as usize + LTP_ORDER / 2) {
                    s_ltp_q15[s_ltp_buf_idx - i - 1] = smulwb(inv_gain_q31, s_ltp[ltp_mem - i - 1] as i32);
                }
            } else if gain_adj_q16 != 1 << 16 {
                // Update LTP state when Gain changes
                for i in 0..(lag as usize + LTP_ORDER / 2) {
                    s_ltp_q15[s_ltp_buf_idx - i - 1] = smulww(gain_adj_q16, s_ltp_q15[s_ltp_buf_idx - i - 1]);
                }
            }
        }

        // Long-term prediction
        if signal_type == TYPE_VOICED {
            let b = &ctrl.ltp_coef_q14[b_off..b_off + LTP_ORDER];
            let mut base = s_ltp_buf_idx - lag as usize + LTP_ORDER / 2;
            for i in 0..subfr_length {
                // Avoids introducing a bias because silk_SMLAWB() always rounds to -inf
                let mut ltp_pred_q13: i32 = 2;
                ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[base], b[0] as i32);
                ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[base - 1], b[1] as i32);
                ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[base - 2], b[2] as i32);
                ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[base - 3], b[3] as i32);
                ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[base - 4], b[4] as i32);
                base += 1;
                // Generate LPC excitation
                pres_q14[i] = st.exc_q14[pexc + i].wrapping_add(lshift(ltp_pred_q13, 1));
                // Update states
                s_ltp_q15[s_ltp_buf_idx] = lshift(pres_q14[i], 1);
                s_ltp_buf_idx += 1;
            }
        } else {
            pres_q14[..subfr_length].copy_from_slice(&st.exc_q14[pexc..pexc + subfr_length]);
        }

        for i in 0..subfr_length {
            // Short-term prediction. Avoids introducing a bias because silk_SMLAWB() always rounds to -inf
            let mut lpc_pred_q10: i32 = (order >> 1) as i32;
            for j in 0..order {
                lpc_pred_q10 = smlawb(lpc_pred_q10, s_lpc_q14[MAX_LPC_ORDER + i - 1 - j], a_q12[j] as i32);
            }
            // Add prediction to LPC excitation
            s_lpc_q14[MAX_LPC_ORDER + i] = add_sat32(pres_q14[i], lshift_sat32(lpc_pred_q10, 4));
            // Scale with gain
            xq[pxq + i] = sat16(rshift_round(smulww(s_lpc_q14[MAX_LPC_ORDER + i], gain_q10), 8)) as i16;
        }

        // Update LPC filter state
        s_lpc_q14.copy_within(subfr_length..subfr_length + MAX_LPC_ORDER, 0);
        pexc += subfr_length;
        pxq += subfr_length;
    }

    // Save LPC state
    st.s_lpc_q14_buf.copy_from_slice(&s_lpc_q14[..MAX_LPC_ORDER]);
}

/// silk_decode_frame: decodes (or conceals) one frame of `frame_length` samples into `p_out`.
fn decode_frame(st: &mut DecState, rc: Option<&mut RangeDecoder>, p_out: &mut [i16], lost_flag: i32, cond_coding: i32) -> usize {
    let l = st.frame_length;
    let mut ctrl = DecCtrl::new();

    let decode = match &rc {
        Some(_) => lost_flag == FLAG_DECODE_NORMAL || (lost_flag == FLAG_DECODE_LBRR && st.lbrr_flags[st.n_frames_decoded] == 1),
        None => false,
    };
    if decode {
        let rc = rc.unwrap();
        let mut pulses = [0i16; MAX_FRAME_LENGTH + 16];

        // Decode quantization indices of side info
        decode_indices(st, rc, st.n_frames_decoded, lost_flag != 0, cond_coding);

        // Decode quantization indices of excitation
        decode_pulses(rc, &mut pulses, st.indices.signal_type as i32, st.indices.quant_offset_type as i32, st.frame_length);

        // Decode parameters and pulse signal
        decode_parameters(st, &mut ctrl, cond_coding);

        // Run inverse NSQ
        decode_core(st, &mut ctrl, p_out, &pulses);

        // Update output buffer
        let mv_len = st.ltp_mem_length - st.frame_length;
        st.out_buf.copy_within(st.frame_length..st.frame_length + mv_len, 0);
        st.out_buf[mv_len..mv_len + st.frame_length].copy_from_slice(&p_out[..st.frame_length]);

        // OSCE builds of libopus pass 16 kHz / 20 ms frames through an output stage clamping to +-32767
        // (after the output buffer was updated, so the LTP memory keeps the unclamped samples)
        if st.osce_clamp && st.fs_khz == 16 && st.nb_subfr == 4 {
            for v in p_out[..l].iter_mut() {
                if *v < -32767 {
                    *v = -32767;
                }
            }
        }

        // Update PLC state
        plc(st, &mut ctrl, p_out, false);

        st.loss_cnt = 0;
        st.prev_signal_type = st.indices.signal_type as i32;

        // A frame has been decoded without errors
        st.first_frame_after_reset = false;
    } else {
        // Handle packet loss by extrapolation
        plc(st, &mut ctrl, p_out, true);

        // Update output buffer
        let mv_len = st.ltp_mem_length - st.frame_length;
        st.out_buf.copy_within(st.frame_length..st.frame_length + mv_len, 0);
        st.out_buf[mv_len..mv_len + st.frame_length].copy_from_slice(&p_out[..st.frame_length]);
    }

    // Comfort noise generation / estimation
    cng(st, &ctrl, p_out, l);

    // Ensure smooth connection of extrapolated and good frames
    plc_glue_frames(st, p_out, l);

    // Update some decoder state variables
    st.lag_prev = ctrl.pitch_l[st.nb_subfr - 1];

    l
}

/// The SILK decoder (one channel). See `decode` for the API contract.
pub struct SilkDecoder {
    pub st: DecState,
    /// sStereo.sMid: the last two samples of the previous frame (the resampler input is delayed by one).
    s_mid: [i16; 2],
}

impl SilkDecoder {
    pub fn new() -> SilkDecoder {
        SilkDecoder { st: DecState::new(), s_mid: [0; 2] }
    }

    /// Libopus-build compatibility switches (both default to true, matching the reference libopus build used
    /// for the tests, which has OSCE and the deep PLC compiled in). `osce_clamp`: clamp decoded 16 kHz / 20 ms
    /// frames to >= -32767 as the OSCE output stage does; `wb_glue_off`: no energy fade-in after a concealed
    /// frame at 16 kHz. Set both to false for the normative output of a plain libopus build.
    pub fn set_compat(&mut self, osce_clamp: bool, wb_glue_off: bool) {
        self.st.osce_clamp = osce_clamp;
        self.st.wb_glue_off = wb_glue_off;
    }

    /// silk_ResetDecoder: call at a mode switch (CELT-only -> SILK/hybrid) or after a long gap.
    pub fn reset(&mut self) {
        self.st.reset();
        self.s_mid = [0; 2];
    }

    /// Decodes the SILK part of a packet from `rc` (the Opus TOC byte is already stripped). internal_khz is
    /// 8, 12 or 16 (the SILK bandwidth: narrowband 8, mediumband 12, wideband 16; hybrid packets use 16).
    /// frame_ms is 10, 20, 40 or 60 (a 40 or 60 ms packet holds 2 or 3 SILK frames of 20 ms; a 10 ms packet
    /// holds one 10 ms frame). Output: 48 kHz f32 in about [-1, 1] (int16 / 32768), `out.len() >= frame_ms *
    /// 48`, one decoded packet's worth. In a hybrid packet this is called first and the CELT part continues
    /// on the same range decoder afterwards, so this does not read past the SILK part. Returns the number of
    /// samples written, or 0 on a corrupt packet.
    pub fn decode(&mut self, rc: &mut RangeDecoder, internal_khz: u32, frame_ms: u32, out: &mut [f32]) -> usize {
        self.run(Some(rc), FLAG_DECODE_NORMAL, internal_khz, frame_ms, out, &mut |_, _| {})
    }

    /// As `decode`, calling `hook` with the decoder state and the decoded internal-rate frame (8, 12 or 16
    /// kHz, 16-bit) after each frame; for tests and tracing.
    pub fn decode_with_hook(&mut self, rc: &mut RangeDecoder, internal_khz: u32, frame_ms: u32, out: &mut [f32], hook: &mut dyn FnMut(&DecState, &[i16])) -> usize {
        self.run(Some(rc), FLAG_DECODE_NORMAL, internal_khz, frame_ms, out, hook)
    }

    /// Packet loss concealment: `frame_ms` of audio for a lost packet.
    pub fn decode_lost(&mut self, internal_khz: u32, frame_ms: u32, out: &mut [f32]) -> usize {
        if frame_ms == 0 || frame_ms % 10 != 0 || frame_ms > 120 {
            return 0;
        }
        if frame_ms <= 60 && frame_ms != 30 && frame_ms != 50 {
            return self.run(None, FLAG_PACKET_LOST, internal_khz, frame_ms, out, &mut |_, _| {});
        }
        // Longer or odd lengths: conceal in chunks of 20 ms (the last one possibly 10 ms)
        let mut done = 0usize;
        let mut remaining = frame_ms;
        while remaining > 0 {
            let chunk = if remaining >= 20 { 20 } else { 10 };
            let n = self.run(None, FLAG_PACKET_LOST, internal_khz, chunk, &mut out[done..], &mut |_, _| {});
            if n == 0 {
                return 0;
            }
            done += n;
            remaining -= chunk;
        }
        done
    }

    /// Decodes the in-band FEC (LBRR) data of a packet to conceal the packet that preceded it (libopus
    /// decode_fec = 1): frames without LBRR data are concealed by the loss concealment.
    pub fn decode_fec(&mut self, rc: &mut RangeDecoder, internal_khz: u32, frame_ms: u32, out: &mut [f32]) -> usize {
        self.run(Some(rc), FLAG_DECODE_LBRR, internal_khz, frame_ms, out, &mut |_, _| {})
    }

    /// `decode_lost` with the hook of `decode_with_hook` (frame_ms 10, 20, 40 or 60).
    pub fn decode_lost_with_hook(&mut self, internal_khz: u32, frame_ms: u32, out: &mut [f32], hook: &mut dyn FnMut(&DecState, &[i16])) -> usize {
        self.run(None, FLAG_PACKET_LOST, internal_khz, frame_ms, out, hook)
    }

    /// `decode_fec` with the hook of `decode_with_hook`.
    pub fn decode_fec_with_hook(&mut self, rc: &mut RangeDecoder, internal_khz: u32, frame_ms: u32, out: &mut [f32], hook: &mut dyn FnMut(&DecState, &[i16])) -> usize {
        self.run(Some(rc), FLAG_DECODE_LBRR, internal_khz, frame_ms, out, hook)
    }

    /// silk_Decode for all the frames of one packet (mono).
    fn run(&mut self, mut rc: Option<&mut RangeDecoder>, lost_flag: i32, internal_khz: u32, frame_ms: u32, out: &mut [f32], hook: &mut dyn FnMut(&DecState, &[i16])) -> usize {
        let (frames_per_packet, nb_subfr) = match frame_ms {
            10 => (1usize, 2usize),
            20 => (1, 4),
            40 => (2, 4),
            60 => (3, 4),
            _ => return 0,
        };
        if !(internal_khz == 8 || internal_khz == 12 || internal_khz == 16) || out.len() < frame_ms as usize * 48 {
            return 0;
        }
        let fs_khz = internal_khz as i32;
        let st = &mut self.st;

        // First frame of the payload
        st.n_frames_decoded = 0;
        st.n_frames_per_packet = frames_per_packet;
        st.nb_subfr = nb_subfr;
        st.set_fs(fs_khz, 48000);
        if st.resampler.is_none() {
            return 0;
        }

        if lost_flag != FLAG_PACKET_LOST {
            let rc = match rc.as_mut() {
                Some(rc) => rc,
                None => return 0,
            };
            // Decode VAD flags and LBRR flag
            for i in 0..frames_per_packet {
                st.vad_flags[i] = rc.bit_logp(1);
            }
            st.lbrr_flag = rc.bit_logp(1);

            // Decode LBRR flags
            st.lbrr_flags = [0; 3];
            if st.lbrr_flag != 0 {
                if frames_per_packet == 1 {
                    st.lbrr_flags[0] = 1;
                } else {
                    let table: &[u8] = if frames_per_packet == 2 { &LBRR_FLAGS_2_ICDF } else { &LBRR_FLAGS_3_ICDF };
                    let lbrr_symbol = rc.icdf(table, 8) + 1;
                    for i in 0..frames_per_packet {
                        st.lbrr_flags[i] = (lbrr_symbol >> i) & 1;
                    }
                }
            }

            if lost_flag == FLAG_DECODE_NORMAL {
                // Regular decoding: skip all LBRR data
                for i in 0..frames_per_packet {
                    if st.lbrr_flags[i] != 0 {
                        let mut pulses = [0i16; MAX_FRAME_LENGTH + 16];
                        let cond_coding = if i > 0 && st.lbrr_flags[i - 1] != 0 { CODE_CONDITIONALLY } else { CODE_INDEPENDENTLY };
                        decode_indices(st, rc, i, true, cond_coding);
                        decode_pulses(rc, &mut pulses, st.indices.signal_type as i32, st.indices.quant_offset_type as i32, st.frame_length);
                    }
                }
            }
        }

        let mut written = 0usize;
        for _ in 0..frames_per_packet {
            let frame_index = st.n_frames_decoded;
            // Use independent coding if no previous frame available
            let cond_coding = if frame_index == 0 {
                CODE_INDEPENDENTLY
            } else if lost_flag == FLAG_DECODE_LBRR {
                if st.lbrr_flags[frame_index - 1] != 0 {
                    CODE_CONDITIONALLY
                } else {
                    CODE_INDEPENDENTLY
                }
            } else {
                CODE_CONDITIONALLY
            };

            // tmp = [sMid[0], sMid[1], frame...]: the resampler input starts at tmp[1]
            let mut tmp = [0i16; MAX_FRAME_LENGTH + 2];
            let n = decode_frame(st, rc.as_deref_mut(), &mut tmp[2..], lost_flag, cond_coding);
            hook(st, &tmp[2..2 + n]);
            st.n_frames_decoded += 1;

            tmp[0] = self.s_mid[0];
            tmp[1] = self.s_mid[1];
            self.s_mid[0] = tmp[n];
            self.s_mid[1] = tmp[n + 1];

            let n_out = n * 48 / fs_khz as usize;
            let mut res = [0i16; 960];
            if let Some(r) = st.resampler.as_mut() {
                r.process(&mut res[..n_out], &tmp[1..1 + n]);
            }
            if written + n_out > out.len() {
                return 0;
            }
            for i in 0..n_out {
                out[written + i] = res[i] as f32 * (1.0 / 32768.0);
            }
            written += n_out;
        }

        if lost_flag == FLAG_PACKET_LOST {
            // On packet loss, remove the gain clamping to prevent having the energy "bounce back"
            // if we lose packets when the energy is going down
            st.last_gain_index = 10;
        }
        written
    }
}
