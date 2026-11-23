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

//! The CELT bit allocation (celt/rate.c, decoder side, mono): how many 1/8 bits each band gets,
//! how they split into fine energy and PVQ pulses, and the pulse cache lookups.

use super::celt_modes::{CeltMode, NB_ALLOC_VECTORS, NB_EBANDS};
use super::rangedec::RangeDecoder;

pub const BITRES: i32 = 3;
pub const MAX_PSEUDO: i32 = 40;
pub const LOG_MAX_PSEUDO: usize = 6;
pub const MAX_FINE_BITS: i32 = 8;
pub const FINE_OFFSET: i32 = 21;
pub const QTHETA_OFFSET: i32 = 4;
const ALLOC_STEPS: i32 = 6;

const LOG2_FRAC_TABLE: [u8; 24] = [
    0, 8, 13, 16, 19, 21, 23, 24, 26, 27, 28, 29, 30, 31, 32, 32, 33, 34, 34, 35, 36, 36, 37, 37,
];

/// `get_pulses`: the number of pulses for a pseudo-pulse index.
#[inline]
pub fn get_pulses(i: i32) -> i32 {
    if i < 8 {
        i
    } else {
        (8 + (i & 7)) << ((i >> 3) - 1)
    }
}

#[inline]
fn at(cache: &[u8], i: i32) -> i32 {
    if i < 0 {
        return 0;
    }
    cache.get(i as usize).copied().unwrap_or(0) as i32
}

/// `bits2pulses`: the pseudo-pulse index whose cost is closest to `bits` (1/8 bits). `lm` may be -1.
pub fn bits2pulses(band: usize, lm: i32, bits: i32) -> i32 {
    let cache = CeltMode::cache_bits((lm + 1) as usize, band);
    if cache.is_empty() {
        return 0;
    }
    let mut lo = 0i32;
    let mut hi = at(cache, 0);
    let bits = bits - 1;
    for _ in 0..LOG_MAX_PSEUDO {
        let mid = (lo + hi + 1) >> 1;
        if at(cache, mid) >= bits {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let lo_cost = if lo == 0 { -1 } else { at(cache, lo) };
    if bits - lo_cost <= at(cache, hi) - bits {
        lo
    } else {
        hi
    }
}

/// `pulses2bits`: the cost in 1/8 bits of a pseudo-pulse index.
pub fn pulses2bits(band: usize, lm: i32, pulses: i32) -> i32 {
    if pulses == 0 {
        return 0;
    }
    let cache = CeltMode::cache_bits((lm + 1) as usize, band);
    at(cache, pulses) + 1
}

/// `init_caps` (mono): the maximum useful allocation of each band.
pub fn init_caps(lm: usize, cap: &mut [i32; NB_EBANDS]) {
    let caps = CeltMode::cache_caps(2 * lm);
    for i in 0..NB_EBANDS {
        let n = (CeltMode::ebands(i + 1) - CeltMode::ebands(i)) << lm;
        cap[i] = ((caps[i] as i32 + 64) * n) >> 2;
    }
}

/// `clt_compute_allocation` + `interp_bits2pulses` for one channel, decoding. Returns the number of
/// coded bands; fills `pulses` (1/8 bits for PVQ), `ebits` (fine energy bits), `fine_priority`.
#[allow(clippy::too_many_arguments)]
pub fn clt_compute_allocation(
    start: usize,
    end: usize,
    offsets: &[i32; NB_EBANDS],
    cap: &[i32; NB_EBANDS],
    alloc_trim: i32,
    total: i32,
    balance_out: &mut i32,
    pulses: &mut [i32; NB_EBANDS],
    ebits: &mut [i32; NB_EBANDS],
    fine_priority: &mut [i32; NB_EBANDS],
    lm: i32,
    rc: &mut RangeDecoder,
) -> usize {
    let c = 1i32;
    let mut total = total.max(0);
    let mut skip_start = start;
    // Reserve a bit to signal the end of manually skipped bands.
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let mut bits1 = [0i32; NB_EBANDS];
    let mut bits2 = [0i32; NB_EBANDS];
    let mut thresh = [0i32; NB_EBANDS];
    let mut trim_offset = [0i32; NB_EBANDS];
    let eb = |i: usize| CeltMode::ebands(i);

    for j in start..end {
        // Below this threshold, we're sure not to allocate any PVQ bits
        thresh[j] = (c << BITRES).max(((3 * (eb(j + 1) - eb(j))) << lm << BITRES) >> 4);
        // Tilt of the allocation curve
        trim_offset[j] = (c * (eb(j + 1) - eb(j)) * (alloc_trim - 5 - lm) * ((end - j - 1) as i32)
            * (1 << (lm + BITRES)))
            >> 6;
        // Giving less resolution to single-coefficient bands because they get more benefit from
        // having one coarse value per coefficient
        if (eb(j + 1) - eb(j)) << lm == 1 {
            trim_offset[j] -= c << BITRES;
        }
    }
    let mut lo: i32 = 1;
    let mut hi: i32 = NB_ALLOC_VECTORS as i32 - 1;
    loop {
        let mut done = false;
        let mut psum = 0i32;
        let mid = (lo + hi) >> 1;
        let mut j = end;
        while j > start {
            j -= 1;
            let n = eb(j + 1) - eb(j);
            let mut bitsj = (c * n * CeltMode::alloc_vector(mid as usize, j)) << lm >> 2;
            if bitsj > 0 {
                bitsj = (bitsj + trim_offset[j]).max(0);
            }
            bitsj += offsets[j];
            if bitsj >= thresh[j] || done {
                done = true;
                // Don't allocate more than we can actually use
                psum += bitsj.min(cap[j]);
            } else if bitsj >= c << BITRES {
                psum += c << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
        if lo > hi {
            break;
        }
    }
    hi = lo;
    lo -= 1;
    for j in start..end {
        let n = eb(j + 1) - eb(j);
        let mut bits1j = (c * n * CeltMode::alloc_vector(lo as usize, j)) << lm >> 2;
        let mut bits2j = if hi >= NB_ALLOC_VECTORS as i32 {
            cap[j]
        } else {
            (c * n * CeltMode::alloc_vector(hi as usize, j)) << lm >> 2
        };
        if bits1j > 0 {
            bits1j = (bits1j + trim_offset[j]).max(0);
        }
        if bits2j > 0 {
            bits2j = (bits2j + trim_offset[j]).max(0);
        }
        if lo > 0 {
            bits1j += offsets[j];
        }
        bits2j += offsets[j];
        if offsets[j] > 0 {
            skip_start = j;
        }
        bits2j = (bits2j - bits1j).max(0);
        bits1[j] = bits1j;
        bits2[j] = bits2j;
    }

    // interp_bits2pulses
    let alloc_floor = c << BITRES;
    let log_m = lm << BITRES;
    let mut lo: i32 = 0;
    let mut hi: i32 = 1 << ALLOC_STEPS;
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0i32;
        let mut done = false;
        let mut j = end;
        while j > start {
            j -= 1;
            let tmp = bits1[j] + ((mid * bits2[j]) >> ALLOC_STEPS);
            if tmp >= thresh[j] || done {
                done = true;
                // Don't allocate more than we can actually use
                psum += tmp.min(cap[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut psum = 0i32;
    let mut done = false;
    let mut j = end;
    while j > start {
        j -= 1;
        let mut tmp = bits1[j] + ((lo * bits2[j]) >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            if tmp >= alloc_floor {
                tmp = alloc_floor;
            } else {
                tmp = 0;
            }
        } else {
            done = true;
        }
        // Don't allocate more than we can actually use
        tmp = tmp.min(cap[j]);
        pulses[j] = tmp;
        psum += tmp;
    }

    // Decide which bands to skip, working backwards from the end.
    let mut coded_bands = end;
    loop {
        let j = coded_bands as i32 - 1;
        // Never skip the first band, nor a band that has been boosted by dynalloc.
        if j <= skip_start as i32 {
            // Give the bit we reserved to end skipping back.
            total += skip_rsv;
            break;
        }
        let j = j as usize;
        // Figure out how many left-over bits we would be adding to this band. This can include
        // bits we've stolen back from higher, skipped bands.
        let mut left = total - psum;
        let width_all = eb(coded_bands) - eb(start);
        let percoeff = left / width_all;
        left -= width_all * percoeff;
        let rem = (left - (eb(j) - eb(start))).max(0);
        let band_width = eb(coded_bands) - eb(j);
        let mut band_bits = pulses[j] + percoeff * band_width + rem;
        // Only code a skip decision if we're above the threshold for this band. Otherwise it is
        // force-skipped. This ensures that we have enough bits to code the skip flag.
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if rc.bit_logp(1) != 0 {
                break;
            }
            // We used a bit to skip this band.
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        // Reclaim the bits originally allocated to this band.
        psum -= pulses[j];
        if band_bits >= alloc_floor {
            // If we have enough for a fine energy bit per channel, use it.
            psum += alloc_floor;
            pulses[j] = alloc_floor;
        } else {
            // Otherwise this band gets nothing at all.
            pulses[j] = 0;
        }
        coded_bands -= 1;
    }
    let _ = LOG2_FRAC_TABLE; // only used for the stereo intensity reservation

    // (mono: no intensity and dual stereo parameters)
    // Allocate the remaining bits
    let mut left = total - psum;
    let width_all = eb(coded_bands) - eb(start);
    let percoeff = left / width_all;
    left -= width_all * percoeff;
    for j in start..coded_bands {
        pulses[j] += percoeff * (eb(j + 1) - eb(j));
    }
    for j in start..coded_bands {
        let tmp = left.min(eb(j + 1) - eb(j));
        pulses[j] += tmp;
        left -= tmp;
    }

    let mut balance = 0i32;
    let mut j = start;
    while j < coded_bands {
        let n0 = eb(j + 1) - eb(j);
        let n = n0 << lm;
        let bit = pulses[j] + balance;
        let mut excess;
        if n > 1 {
            excess = (bit - cap[j]).max(0);
            pulses[j] = bit - excess;

            let den = c * n;
            let nclogn = den * (CeltMode::log_n(j) + log_m);

            // Offset for the number of fine bits by log2(N)/2 + FINE_OFFSET compared to their
            // "fair share" of total/N
            let mut offset = (nclogn >> 1) - den * FINE_OFFSET;

            // N=2 is the only point that doesn't match the curve
            if n == 2 {
                offset += (den << BITRES) >> 2;
            }

            // Changing the offset for allocating the second and third fine energy bit
            if pulses[j] + offset < (den * 2) << BITRES {
                offset += nclogn >> 2;
            } else if pulses[j] + offset < (den * 3) << BITRES {
                offset += nclogn >> 3;
            }

            // Divide with rounding
            ebits[j] = (pulses[j] + offset + (den << (BITRES - 1))).max(0);
            ebits[j] = (ebits[j] / den) >> BITRES;

            // Make sure not to bust
            if c * ebits[j] > (pulses[j] >> BITRES) {
                ebits[j] = pulses[j] >> 0 >> BITRES;
            }

            // More than that is useless because that's about as far as PVQ can go
            ebits[j] = ebits[j].min(MAX_FINE_BITS);

            // If we rounded down or capped this band, make it a candidate for the final fine
            // energy pass
            fine_priority[j] = (ebits[j] * (den << BITRES) >= pulses[j] + offset) as i32;

            // Remove the allocated fine bits; the rest are assigned to PVQ
            pulses[j] -= (c * ebits[j]) << BITRES;
        } else {
            // For N=1, all bits go to fine energy except for a single sign bit
            excess = (bit - (c << BITRES)).max(0);
            pulses[j] = bit - excess;
            ebits[j] = 0;
            fine_priority[j] = 1;
        }

        // Fine energy can't take advantage of the re-balancing in quant_all_bands(). Instead, do
        // the re-balancing here.
        if excess > 0 {
            let extra_fine = (excess >> BITRES).min(MAX_FINE_BITS - ebits[j]);
            ebits[j] += extra_fine;
            let extra_bits = (extra_fine * c) << BITRES;
            fine_priority[j] = (extra_bits >= excess - balance) as i32;
            excess -= extra_bits;
        }
        balance = excess;
        j += 1;
    }
    // Save any remaining bits over the cap for the rebalancing in quant_all_bands().
    *balance_out = balance;

    // The skipped bands use all their bits for fine energy.
    while j < end {
        ebits[j] = pulses[j] >> BITRES;
        pulses[j] = 0;
        fine_priority[j] = (ebits[j] < 1) as i32;
        j += 1;
    }
    coded_bands
}
