//! Opus (RFC 6716), written for Penryn: a Rust port of libopus (BSD-3, see each file's header).
//! A mono 48 kHz decoder (SILK, CELT, hybrid) and a CELT-only encoder, for the voice chat.
#![allow(dead_code, clippy::all)]
pub mod celt_bands;
pub mod celt_cwrs;
pub mod celt_dec;
pub mod celt_enc;
pub mod celt_enc_alloc;
pub mod celt_enc_bands;
pub mod celt_enc_energy;
pub mod celt_enc_math;
pub mod celt_enc_mdct;
pub mod celt_enc_vq;
pub mod celt_laplace;
pub mod celt_mdct;
pub mod celt_modes;
pub mod celt_pitch;
pub mod celt_quant;
pub mod celt_rate;
pub mod celt_tables;
pub mod celt_vq;
pub mod decoder;
pub mod mathops;
pub mod rangedec;
pub mod rangeenc;
pub mod silk_dec;
pub mod silk_dsp;
pub mod silk_math;
pub mod silk_plc;
pub mod silk_pulses;
pub mod silk_resampler;
pub mod silk_tables;

/// Checks for `penryn selftest`: (what, got, want). A 1 kHz tone goes through the encoder and the
/// decoder and comes back as a tone of about the same size; and AES-GCM matches NIST's test case 2.
pub fn selftest() -> alloc::vec::Vec<(&'static str, alloc::string::String, &'static str)> {
    use alloc::format;
    use alloc::vec::Vec;
    let mut out = Vec::new();
    let mut enc = celt_enc::CeltEncoder::new();
    enc.set_end_band(17);
    let mut dec = decoder::OpusDecoder::new();
    let mut pcm = Vec::new();
    let (mut sent, mut peak_in, mut peak_out) = (0usize, 0i32, 0i32);
    let mut ok_frames = 0;
    for f in 0..30usize {
        let frame: Vec<i16> = (0..960).map(|i| (crate::math::sin(2.0 * core::f64::consts::PI * 1000.0 * (f * 960 + i) as f64 / 48000.0) * 12000.0) as i16).collect();
        let pk = enc.encode(&frame, 32000, 1275);
        sent += pk.len();
        pcm.clear();
        pcm.resize(5760, 0i16);
        if let Ok(n) = dec.decode(&pk, &mut pcm) {
            if n == 960 {
                ok_frames += 1;
                if f >= 10 {
                    peak_in = peak_in.max(frame.iter().map(|s| (*s as i32).abs()).max().unwrap_or(0));
                    peak_out = peak_out.max(pcm[..n].iter().map(|s| (*s as i32).abs()).max().unwrap_or(0));
                }
            }
        }
    }
    out.push(("opus: 30 frames encode and decode", format!("{}", ok_frames), "30"));
    out.push(("opus: 32 kbit/s frames are 81 bytes (80 and the TOC)", format!("{}", sent / 30), "81"));
    out.push(("opus: the tone comes back at its level", format!("{}", peak_out * 10 / peak_in.max(1) >= 8 && peak_out * 10 / peak_in.max(1) <= 12), "true"));
    // NIST GCM test case 2: key 0, IV 0, one zero block.
    let sealed = crate::voicechat::gcm::seal(&[0u8; 16], &[0u8; 12], &[0u8; 16]);
    let hexs: alloc::string::String = sealed.iter().map(|b| format!("{:02x}", b)).collect();
    out.push(("voicechat: AES-GCM (NIST case 2)", hexs, "0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf"));
    out
}
