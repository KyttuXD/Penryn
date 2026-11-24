//! Animated textures (animations.bin, "PNAS", made by make_assets.py from the .mcmeta animations):
//! every game tick each animation steps along its frame order, and once a frame the ones that
//! changed are written into the atlas texture (glTexSubImage2D, like vanilla's
//! TextureAtlasSprite.Ticker.tickAndUpload), mip levels 1 to 4 included, made with the same
//! alpha-weighted box filter as Renderer::load_atlas. A copy of the atlas' level 0 is kept so that
//! a rectangle that is not a multiple of 16 still gets the mips load_atlas would have made.

use crate::sdl::*;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

/// At most this many animations are uploaded a frame (round robin: the rest wait for the next frame).
const MAX_PER_FRAME: usize = 48;
/// ... and this many atlas pixels (an animation always gets through when it is the first of the frame).
const MAX_PIXELS: usize = 262144;
pub const MAGIC: &[u8; 4] = b"PNAS";

/// The CRC-32 (zlib's) of the atlas' pixels: the stamp the baker puts in animations.bin, so the file is only used with
/// the atlas it was baked for (an older atlas.bin has other pixels, so its animations are none, never garbage).
pub fn stamp(px: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    let mut c = !0u32;
    for &b in px {
        c = table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

struct Item {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    interpolate: bool,
    /// (frame, ticks) in the order they show.
    seq: Vec<(u16, u16)>,
    /// The frames, w x h RGBA each.
    frames: Vec<u8>,
    pos: usize,
    sub: u32,
    /// The frame in the atlas now (-1: the first, baked one, which is not any frame number).
    shown: i32,
    dirty: bool,
}

/// What to give the card for one animation: the aligned block at x, y (a multiple of 16) with w x h
/// pixels, as levels 0 to 4.
pub struct Patch {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub levels: Vec<Vec<u8>>,
}

#[derive(Default)]
pub struct Anim {
    w: usize,
    h: usize,
    /// The atlas' level 0 (only kept when something animates).
    px: Vec<u8>,
    list: Vec<Item>,
    cursor: usize,
}

fn u16_at(d: &[u8], o: usize) -> Option<usize> {
    Some(u16::from_le_bytes([*d.get(o)?, *d.get(o + 1)?]) as usize)
}

/// One mip step: 2x2 boxes, colour weighted by alpha so cut-out edges do not darken.
pub fn halve(level: &[u8], lw: usize, lh: usize) -> Vec<u8> {
    let (nw, nh) = (lw / 2, lh / 2);
    let mut next = alloc::vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            let mut c = [0u32; 4];
            let mut wsum = 0u32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let o = ((2 * y + dy) * lw + 2 * x + dx) * 4;
                let a = level[o + 3] as u32;
                for k in 0..3 {
                    c[k] += level[o + k] as u32 * a;
                }
                c[3] += a;
                wsum += a;
            }
            let o = (y * nw + x) * 4;
            for k in 0..3 {
                next[o + k] = if wsum > 0 { (c[k] / wsum) as u8 } else { 0 };
            }
            next[o + 3] = (c[3] / 4) as u8;
        }
    }
    next
}

impl Anim {
    /// The animations of `file` over an atlas of w x h whose level 0 is `atlas` (RGBA). Damaged or for
    /// another atlas size: none.
    pub fn parse(file: &[u8], atlas: &[u8], w: usize, h: usize) -> Anim {
        Self::try_parse(file, atlas, w, h).unwrap_or_default()
    }

    fn try_parse(d: &[u8], atlas: &[u8], w: usize, h: usize) -> Option<Anim> {
        if d.get(..4)? != MAGIC || u16_at(d, 4)? != w || u16_at(d, 6)? != h || atlas.len() < w * h * 4 {
            return None;
        }
        let count = u32::from_le_bytes(d.get(8..12)?.try_into().ok()?) as usize;
        if u32::from_le_bytes(d.get(12..16)?.try_into().ok()?) != stamp(atlas.get(..w * h * 4)?) {
            return None; // baked for another atlas.bin
        }
        let mut p = 16;
        let mut list = Vec::new();
        for _ in 0..count.min(100000) {
            let (x, y, iw, ih) = (u16_at(d, p)?, u16_at(d, p + 2)?, u16_at(d, p + 4)?, u16_at(d, p + 6)?);
            let flags = *d.get(p + 8)?;
            let nframes = u16_at(d, p + 9)?;
            let nseq = u16_at(d, p + 11)?;
            p += 13;
            if iw == 0 || ih == 0 || x + iw > w || y + ih > h || nframes == 0 || nseq == 0 {
                return None;
            }
            let mut seq = Vec::with_capacity(nseq);
            for _ in 0..nseq {
                let (f, t) = (u16_at(d, p)?, u16_at(d, p + 2)?);
                p += 4;
                if f >= nframes {
                    return None;
                }
                seq.push((f as u16, (t as u16).max(1)));
            }
            let size = nframes.checked_mul(iw * ih * 4)?;
            let frames = d.get(p..p.checked_add(size)?)?.to_vec();
            p += size;
            list.push(Item { x, y, w: iw, h: ih, interpolate: flags & 1 != 0, seq, frames, pos: 0, sub: 0, shown: -1, dirty: false });
        }
        if list.is_empty() {
            return Some(Anim::default());
        }
        Some(Anim { w, h, px: atlas[..w * h * 4].to_vec(), list, cursor: 0 })
    }

    /// How many textures animate.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// One game tick: every animation moves on, and the ones that show something else are marked.
    pub fn advance(&mut self) {
        for it in self.list.iter_mut() {
            it.sub += 1;
            if it.sub >= it.seq[it.pos].1 as u32 {
                it.pos = (it.pos + 1) % it.seq.len();
                it.sub = 0;
            }
            let cur = it.seq[it.pos].0 as i32;
            let next = it.seq[(it.pos + 1) % it.seq.len()].0 as i32;
            // An interpolating one changes every tick (while it has a different frame to blend towards).
            if cur != it.shown || (it.interpolate && cur != next) {
                it.dirty = true;
            }
        }
    }

    /// The pixels of an animation now (its frame, or the blend of this and the next as vanilla's interpolate
    /// does), and the frame number.
    fn current(it: &Item) -> (Vec<u8>, i32) {
        let n = it.w * it.h * 4;
        let cur = it.seq[it.pos].0 as usize;
        let next = it.seq[(it.pos + 1) % it.seq.len()].0 as usize;
        let a = &it.frames[cur * n..(cur + 1) * n];
        if !it.interpolate || cur == next {
            return (a.to_vec(), cur as i32);
        }
        let b = &it.frames[next * n..(next + 1) * n];
        let (num, den) = (it.sub, it.seq[it.pos].1 as u32);
        let mut out = alloc::vec![0u8; n];
        for i in 0..n {
            out[i] = ((a[i] as u32 * (den - num) + b[i] as u32 * num) / den) as u8;
        }
        (out, cur as i32)
    }

    /// The changed animations' patches for this frame (at most MAX_PER_FRAME and MAX_PIXELS' worth, round
    /// robin from where the last frame stopped), written into the kept level 0 as well. Each comes with the
    /// animation's own x, y, w, h.
    pub fn changes(&mut self) -> Vec<(Patch, [usize; 4])> {
        let mut out = Vec::new();
        let n = self.list.len();
        let mut pixels = 0;
        let mut last = self.cursor;
        for k in 0..n {
            let i = (self.cursor + k) % n;
            if !self.list[i].dirty {
                continue;
            }
            let (x, y, w, h) = (self.list[i].x, self.list[i].y, self.list[i].w, self.list[i].h);
            let (ax, ay) = (x & !15, y & !15);
            let (bx, by) = (((x + w + 15) & !15).min(self.w), ((y + h + 15) & !15).min(self.h));
            if !out.is_empty() && (out.len() >= MAX_PER_FRAME || pixels + (bx - ax) * (by - ay) > MAX_PIXELS) {
                break;
            }
            let (px, shown) = Self::current(&self.list[i]);
            self.list[i].shown = shown;
            self.list[i].dirty = false;
            for r in 0..h {
                let o = ((y + r) * self.w + x) * 4;
                self.px[o..o + w * 4].copy_from_slice(&px[r * w * 4..(r + 1) * w * 4]);
            }
            let (pw, ph) = (bx - ax, by - ay);
            let mut level = Vec::with_capacity(pw * ph * 4);
            for r in 0..ph {
                let o = ((ay + r) * self.w + ax) * 4;
                level.extend_from_slice(&self.px[o..o + pw * 4]);
            }
            let mut levels = Vec::new();
            let (mut lw, mut lh) = (pw, ph);
            for lv in 0..5 {
                levels.push(level.clone());
                if lv == 4 || lw < 2 || lh < 2 {
                    break;
                }
                level = halve(&level, lw, lh);
                lw /= 2;
                lh /= 2;
            }
            pixels += pw * ph;
            last = i + 1;
            out.push((Patch { x: ax, y: ay, w: pw, h: ph, levels }, [x, y, w, h]));
        }
        if !out.is_empty() {
            self.cursor = last % n;
        }
        out
    }

    /// Writes this frame's changes into the atlas texture `tex` (and the new alpha into `alpha`).
    pub fn upload(&mut self, gl: &Gl, tex: u32, alpha: &mut [u8]) {
        if self.list.is_empty() || tex == 0 {
            return;
        }
        let ch = self.changes();
        if ch.is_empty() {
            return;
        }
        unsafe {
            (gl.ActiveTexture)(TEXTURE0);
            (gl.BindTexture)(TEXTURE_2D, tex);
            (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
            for (p, [x, y, w, h]) in &ch {
                for (lv, data) in p.levels.iter().enumerate() {
                    let (lw, lh) = ((p.w >> lv).max(1), (p.h >> lv).max(1));
                    (gl.TexSubImage2D)(TEXTURE_2D, lv as i32, (p.x >> lv) as i32, (p.y >> lv) as i32, lw as i32, lh as i32, RGBA, UNSIGNED_BYTE, data.as_ptr() as *const c_void);
                }
                if alpha.len() >= self.w * self.h {
                    for r in 0..*h {
                        for c in 0..*w {
                            alpha[(y + r) * self.w + x + c] = self.px[((y + r) * self.w + x + c) * 4 + 3];
                        }
                    }
                }
            }
        }
    }
}

/// Reads the animations of the game files in use for an atlas of w x h whose level 0 is `atlas`. A pack
/// stack that changes atlas.bin without its own animations.bin has none (the base file's rectangles
/// belong to the base atlas).
pub fn load(atlas: &[u8], w: usize, h: usize) -> Anim {
    if crate::packs::changes("atlas.bin") && !crate::packs::changes("animations.bin") {
        return Anim::default();
    }
    match crate::sys::read_file(&crate::packs::path("animations.bin")) {
        Some(d) => Anim::parse(&d, atlas, w, h),
        None => Anim::default(),
    }
}

// ------------------------------------------------------------------ checks

fn put(v: &mut Vec<u8>, a: usize) {
    v.extend_from_slice(&(a as u16).to_le_bytes());
}

type Rect<'a> = (usize, usize, usize, usize, bool, &'a [(usize, usize)], &'a [u8]);

/// A file of animations at (x, y, w, h), interpolate, the order (frame, ticks), and the colour of each frame.
fn make_file(aw: usize, ah: usize, rects: &[Rect], atlas: &[u8]) -> Vec<u8> {
    let mut v = Vec::from(&MAGIC[..]);
    put(&mut v, aw);
    put(&mut v, ah);
    v.extend_from_slice(&(rects.len() as u32).to_le_bytes());
    v.extend_from_slice(&stamp(atlas).to_le_bytes());
    for &(x, y, w, h, interp, seq, colors) in rects {
        for a in [x, y, w, h] {
            put(&mut v, a);
        }
        v.push(interp as u8);
        put(&mut v, colors.len());
        put(&mut v, seq.len());
        for &(f, t) in seq {
            put(&mut v, f);
            put(&mut v, t);
        }
        for &c in colors {
            for _ in 0..w * h {
                v.extend_from_slice(&[c, c, c, 255]);
            }
        }
    }
    v
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    use alloc::format;
    let mut out = Vec::new();
    let (aw, ah) = (64usize, 64usize);
    let atlas = alloc::vec![7u8; aw * ah * 4];
    let f = make_file(aw, ah, &[(16, 16, 16, 16, false, &[(0, 2), (1, 2)], &[10, 200]), (20, 17, 5, 3, false, &[(0, 1), (1, 3)], &[100, 50])], &atlas);
    let mut a = Anim::parse(&f, &atlas, aw, ah);
    out.push(("anim parse", format!("{}", a.len()), "2"));
    let mut other = atlas.clone();
    other[100] ^= 1;
    out.push(("anim other bake of the atlas", format!("{}", Anim::parse(&f, &other, aw, ah).len()), "0"));
    out.push(("anim crc32", format!("{:08x}", stamp(b"123456789")), "cbf43926"));
    out.push(("anim damaged", format!("{}", Anim::parse(&f[..f.len() - 5], &atlas, aw, ah).len()), "0"));
    out.push(("anim other atlas", format!("{}", Anim::parse(&f, &atlas, 32, 64).len()), "0"));
    out.push(("anim nothing before the first tick", format!("{}", a.changes().len()), "0"));
    // Tick 1: the second (time 1) moves to frame 1; the first (time 2) is still on frame 0, which was never shown: marked too.
    a.advance();
    let c = a.changes();
    out.push(("anim first tick uploads both", format!("{}", c.len()), "2"));
    let p0 = &c[0].0;
    out.push(("anim patch place", format!("{} {} {}x{} {}", p0.x, p0.y, p0.w, p0.h, p0.levels.len()), "16 16 16x16 5"));
    out.push(("anim patch level 0", format!("{}", p0.levels[0][0]), "10"));
    out.push(("anim patch level 4 size", format!("{}", p0.levels[4].len()), "4"));
    out.push(("anim patch level 4 colour", format!("{}", p0.levels[4][0]), "10"));
    // The second, misaligned (20, 17, 5, 3): the patch is 16..32 x 16..32 with the neighbours' atlas pixels around it.
    let p1 = &c[1].0;
    let o = ((17 - 16) * 16 + (20 - 16)) * 4;
    out.push(("anim misaligned patch", format!("{} {} {}x{} px {}", p1.x, p1.y, p1.w, p1.h, p1.levels[0][o]), "16 16 16x16 px 50"));
    out.push(("anim neighbour kept", format!("{}", p1.levels[0][0]), "10"));    // Tick 2: the first reaches frame 1 (200); the second (time 3) stays: one change.
    a.advance();
    let c = a.changes();
    out.push(("anim tick 2 uploads one", format!("{} {}", c.len(), c.first().map_or(0, |c| c.0.levels[0][0])), "1 200"));
    out.push(("anim idle", format!("{}", a.changes().len()), "0"));
    out.push(("anim halve", format!("{:?}", &halve(&[10, 20, 30, 255, 30, 40, 50, 255, 0, 0, 0, 0, 0, 0, 0, 0], 2, 2)[..4]), "[20, 30, 40, 127]"));
    // Interpolation: 0 -> 100 over 4 ticks.
    let f = make_file(aw, ah, &[(0, 0, 16, 16, true, &[(0, 4), (1, 4)], &[0, 100])], &atlas);
    let mut a = Anim::parse(&f, &atlas, aw, ah);
    let mut seen = Vec::new();
    for _ in 0..8 {
        a.advance();
        seen.push(a.changes().first().map_or(-1, |c| c.0.levels[0][0] as i32));
    }
    out.push(("anim interpolate", format!("{:?}", seen), "[25, 50, 75, 100, 75, 50, 25, 0]"));
    // Round robin: 60 animations, MAX_PER_FRAME a frame, the rest next frame.
    let rects: Vec<Rect> = (0..60).map(|i| ((i % 4) * 16, (i / 4) * 16 % 128, 16, 16, false, &[(0usize, 1usize), (1, 1)][..], &[1u8, 2][..])).collect();
    let big = alloc::vec![0u8; 128 * 128 * 4];
    let f = make_file(128, 128, &rects, &big);
    let mut a = Anim::parse(&f, &big, 128, 128);
    a.advance();
    let n1 = a.changes().len();
    let n2 = a.changes().len();
    out.push(("anim cap", format!("{} {}", n1, n2), "48 12"));
    // The game files made by the baker (when there are any): every animation parses.
    let atlas = crate::sys::read_file(&crate::packs::path("atlas.bin"));
    let file = crate::sys::read_file(&crate::packs::path("animations.bin"));
    if let (Some(at), Some(f)) = (atlas, file) {
        if at.len() > 8 && f.len() >= 12 {
            let (w, h) = (u16::from_le_bytes([at[4], at[5]]) as usize, u16::from_le_bytes([at[6], at[7]]) as usize);
            let want = u32::from_le_bytes([f[8], f[9], f[10], f[11]]) as usize;
            let stamped = f.len() >= 16 && u32::from_le_bytes([f[12], f[13], f[14], f[15]]) == stamp(&at[8..]);
            let got = Anim::parse(&f, &at[8..], w, h).len();
            out.push(("anim animations.bin parses", format!("{}", !stamped || (got == want && want > 0)), "true"));
        }
    }
    out
}
