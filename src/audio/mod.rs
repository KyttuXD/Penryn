//! Sound as vanilla plays it: the game's own sound files (sounds.bin, Ogg Vorbis as the game
//! has them) and sound events (sounds.json: weighted variants with volume and pitch), mixed in
//! stereo on SDL's audio thread with vanilla's linear distance attenuation and panning, and
//! music streamed a packet at a time. The Vorbis decoder is VoxelRust's (MIT, same author).

pub mod ogg;
pub mod ogg_stream;
mod vorbis;

use crate::sdl::{self, AudioSpec, Sdl};
use crate::sys;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicPtr, Ordering};

/// The mixer's rate: what the sounds are resampled to.
pub const RATE: u32 = 22050;
/// Longest sound decoded whole (streamed sounds have no limit), in seconds.
pub const MAX_SECONDS: u32 = 60;

/// What the readers give: interleaved samples in -1..1.
pub struct Pcm {
    pub rate: u32,
    pub channels: usize,
    pub data: Vec<f32>,
}

impl Pcm {
    /// One channel at RATE, 16-bit.
    fn mono(self) -> Option<Vec<i16>> {
        let (rate, ch) = (self.rate, self.channels);
        if ch == 0 || ch > 8 || !(1000..=192_000).contains(&rate) {
            return None;
        }
        let frames = self.data.len() / ch;
        if frames == 0 {
            return None;
        }
        let mono = |f: usize| -> f32 { self.data[f * ch..f * ch + ch].iter().sum::<f32>() / ch as f32 };
        let out_len = ((frames as u64 * RATE as u64) / rate as u64).max(1) as usize;
        let mut out = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let pos = i as f64 * rate as f64 / RATE as f64;
            let k = pos as usize;
            let frac = (pos - k as f64) as f32;
            let (a, b) = (mono(k.min(frames - 1)), mono((k + 1).min(frames - 1)));
            out.push(((a + (b - a) * frac).clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        Some(out)
    }
}

#[derive(Clone, Copy)]
pub struct Entry {
    pub file: u32,
    pub volume: f32,
    pub pitch: f32,
    pub weight: u16,
    pub stream: bool,
    pub attenuation: u8,
}

/// Mixer categories (SoundSource): master is applied to all.
pub const MUSIC: usize = 1;

struct Voice {
    data: &'static [i16],
    pos: f64,
    step: f64,
    gain: [f32; 2],
    category: usize,
}

/// What the mixer plays of the music: decoded mono samples at RATE, and where playing is.
/// The decoder itself lives in `Audio`, on the game's thread, so the mixer never waits on it.
struct Music {
    buf: Vec<i16>,
    read: usize,
    done: bool,
    gain: f32,
}

struct Decoder {
    stream: ogg_stream::OggStream,
    rate: u32,
    channels: usize,
    /// Fractional read position carried between blocks (resampling).
    t: f64,
}

struct Mix {
    voices: Vec<Voice>,
    music: Option<Music>,
    volumes: [f32; 11],
}

static MIX: AtomicPtr<Mix> = AtomicPtr::new(core::ptr::null_mut());

/// SDL's audio callback: adds every voice and the music into the stereo buffer.
unsafe extern "C" fn callback(_ud: *mut c_void, stream: *mut u8, len: c_int) {
    let out = core::slice::from_raw_parts_mut(stream as *mut i16, len as usize / 2);
    let mix = MIX.load(Ordering::Acquire);
    if mix.is_null() {
        out.iter_mut().for_each(|s| *s = 0);
        return;
    }
    let mix = &mut *mix;
    // No allocation on the audio thread: the device asks for 1024 frames (2048 samples).
    let mut buf = [0f32; 8192];
    let frames = (out.len() / 2).min(4096);
    let acc = &mut buf[..frames * 2];
    let master = mix.volumes[0];
    mix.voices.retain_mut(|v| {
        let g = [v.gain[0] * master * mix.volumes[v.category], v.gain[1] * master * mix.volumes[v.category]];
        for f in 0..frames {
            let i = v.pos as usize;
            if i + 1 >= v.data.len() {
                return false;
            }
            let frac = (v.pos - i as f64) as f32;
            let s = v.data[i] as f32 + (v.data[i + 1] as f32 - v.data[i] as f32) * frac;
            acc[2 * f] += s * g[0];
            acc[2 * f + 1] += s * g[1];
            v.pos += v.step;
        }
        true
    });
    if let Some(m) = mix.music.as_mut() {
        let g = m.gain * master * mix.volumes[MUSIC];
        for f in 0..frames {
            if m.read >= m.buf.len() {
                break;
            }
            let s = m.buf[m.read] as f32 * g;
            acc[2 * f] += s;
            acc[2 * f + 1] += s;
            m.read += 1;
        }
    }
    for (o, a) in out.iter_mut().zip(acc.iter()) {
        *o = a.clamp(-32768.0, 32767.0) as i16;
    }
}

pub struct Audio {
    device: u32,
    lock: unsafe extern "C" fn(u32),
    unlock: unsafe extern "C" fn(u32),
    pack: sys::File,
    files: Vec<(u64, u32)>,
    events: Vec<Vec<Entry>>,
    named: BTreeMap<String, Vec<Entry>>,
    ids: BTreeMap<String, u32>,
    cache: BTreeMap<u32, &'static [i16]>,
    rng: u64,
    listener: [f64; 3],
    yaw: f32,
    decoder: Option<Decoder>,
    /// Milliseconds spent decoding sound in this second (PENRYN_PERF).
    pub busy_ms: f64,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*d.get(o)?, *d.get(o + 1)?]))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn entries(d: &[u8], p: &mut usize) -> Option<Vec<Entry>> {
    let n = u16_at(d, *p)? as usize;
    *p += 2;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let r = d.get(*p..*p + 16)?;
        v.push(Entry {
            file: u32_at(r, 0)?,
            volume: f32::from_bits(u32_at(r, 4)?),
            pitch: f32::from_bits(u32_at(r, 8)?),
            weight: u16_at(r, 12)?,
            stream: r[14] != 0,
            attenuation: r[15],
        });
        *p += 16;
    }
    Some(v)
}

impl Audio {
    /// Opens the sound pack and the audio device. `None` without either (the game plays silent).
    pub fn open(sdl: &Sdl, path: &str) -> Option<Audio> {
        let pack = sys::File::open(path)?;
        // The header: read in growing pieces until it parses.
        let head = pack.read_at(0, 16)?;
        if &head[..4] != b"PNS1" {
            return None;
        }
        let mut size = 1 << 20;
        let (files, events, named, ids) = loop {
            let d = pack.read_at(4, size).or_else(|| pack.read_at(4, size / 2))?;
            if let Some(t) = Self::parse(&d) {
                break t;
            }
            if size > 64 << 20 {
                return None;
            }
            size *= 2;
        };
        unsafe {
            if (sdl.SDL_InitSubSystem)(sdl::INIT_AUDIO) != 0 {
                return None;
            }
            let want = AudioSpec {
                freq: RATE as c_int,
                format: sdl::AUDIO_S16LSB,
                channels: 2,
                silence: 0,
                samples: 1024,
                padding: 0,
                size: 0,
                callback: Some(callback),
                userdata: core::ptr::null_mut(),
            };
            let mut got: AudioSpec = core::mem::zeroed();
            let dev = (sdl.SDL_OpenAudioDevice)(core::ptr::null(), 0, &want, &mut got, 0);
            if dev == 0 {
                return None;
            }
            let mix = Box::new(Mix { voices: Vec::new(), music: None, volumes: [1.0; 11] });
            MIX.store(Box::into_raw(mix), Ordering::Release);
            (sdl.SDL_PauseAudioDevice)(dev, 0);
            Some(Audio {
                device: dev,
                lock: sdl.SDL_LockAudioDevice,
                unlock: sdl.SDL_UnlockAudioDevice,
                pack,
                files,
                events,
                named,
                ids,
                cache: BTreeMap::new(),
                rng: (sys::now() * 1e6) as u64 | 1,
                listener: [0.0; 3],
                yaw: 0.0,
                decoder: None,
                busy_ms: 0.0,
            })
        }
    }

    #[allow(clippy::type_complexity)]
    fn parse(d: &[u8]) -> Option<(Vec<(u64, u32)>, Vec<Vec<Entry>>, BTreeMap<String, Vec<Entry>>, BTreeMap<String, u32>)> {
        let mut p = 0;
        let n = u32_at(d, p)? as usize;
        p += 4;
        let mut files = Vec::with_capacity(n);
        for _ in 0..n {
            let l = u16_at(d, p)? as usize;
            p += 2 + l;
            let off = u64::from_le_bytes(d.get(p..p + 8)?.try_into().ok()?);
            let size = u32_at(d, p + 8)?;
            files.push((off, size));
            p += 12;
        }
        let n = u32_at(d, p)? as usize;
        p += 4;
        let mut events = Vec::with_capacity(n);
        for _ in 0..n {
            events.push(entries(d, &mut p)?);
        }
        let n = u32_at(d, p)? as usize;
        p += 4;
        let mut named = BTreeMap::new();
        for _ in 0..n {
            let l = u16_at(d, p)? as usize;
            let name = String::from_utf8_lossy(d.get(p + 2..p + 2 + l)?).into_owned();
            p += 2 + l;
            named.insert(name, entries(d, &mut p)?);
        }
        let n = u32_at(d, p)? as usize;
        p += 4;
        let mut ids = BTreeMap::new();
        for i in 0..n {
            let l = u16_at(d, p)? as usize;
            ids.insert(String::from_utf8_lossy(d.get(p + 2..p + 2 + l)?).into_owned(), i as u32);
            p += 2 + l;
        }
        Some((files, events, named, ids))
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    fn pick(&mut self, list: &[Entry]) -> Option<Entry> {
        let total: u32 = list.iter().map(|e| e.weight.max(1) as u32).sum();
        if total == 0 {
            return None;
        }
        let mut r = (self.rand() * total as f32) as u32;
        for e in list {
            let w = e.weight.max(1) as u32;
            if r < w {
                return Some(*e);
            }
            r -= w;
        }
        list.last().copied()
    }

    fn samples(&mut self, file: u32) -> Option<&'static [i16]> {
        if let Some(s) = self.cache.get(&file) {
            return Some(s);
        }
        let &(off, size) = self.files.get(file as usize)?;
        let t0 = sys::now();
        let bytes = self.pack.read_at(off, size as usize)?;
        let pcm = ogg::ogg(&bytes)?.mono()?;
        self.busy_ms += (sys::now() - t0) * 1000.0;
        let s: &'static [i16] = Box::leak(pcm.into_boxed_slice());
        self.cache.insert(file, s);
        Some(s)
    }

    pub fn set_listener(&mut self, pos: [f64; 3], yaw: f32) {
        self.listener = pos;
        self.yaw = yaw;
    }

    /// Plays sound event `id` (the sound-event registry id), at `pos` (None: no position, as
    /// UI sounds), with the event's volume and pitch multiplied by `volume` and `pitch`.
    pub fn play(&mut self, id: u32, pos: Option<[f64; 3]>, volume: f32, pitch: f32, category: usize) {
        let Some(list) = self.events.get(id as usize).cloned() else { return };
        self.play_list(&list, pos, volume, pitch, category);
    }

    /// Plays an event by its name (registry or sounds.json): client-side sounds.
    pub fn play_named(&mut self, name: &str, pos: Option<[f64; 3]>, volume: f32, pitch: f32, category: usize) {
        if let Some(&id) = self.ids.get(name) {
            return self.play(id, pos, volume, pitch, category);
        }
        let Some(list) = self.named.get(name).cloned() else { return };
        self.play_list(&list, pos, volume, pitch, category);
    }

    fn play_list(&mut self, list: &[Entry], pos: Option<[f64; 3]>, volume: f32, pitch: f32, category: usize) {
        let Some(e) = self.pick(list) else { return };
        let vol = (e.volume * volume).max(0.0);
        let pit = (e.pitch * pitch).clamp(0.5, 2.0);
        // Vanilla: linear fade to silence at attenuation_distance x max(volume, 1); louder than
        // 1 only reaches further.
        let gain = match pos {
            None => [vol.min(1.0) * 0.707, vol.min(1.0) * 0.707],
            Some(p) => {
                let d = [p[0] - self.listener[0], p[1] - self.listener[1], p[2] - self.listener[2]];
                let dist = crate::math::sqrt(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as f32;
                let range = e.attenuation as f32 * vol.max(1.0);
                let g = vol.min(1.0) * (1.0 - dist / range).max(0.0);
                if g <= 0.001 {
                    return;
                }
                // Pan by how far to the listener's right the sound is.
                let y = crate::math::to_rad(self.yaw as f64);
                let right = [-crate::math::cos(y), 0.0, -crate::math::sin(y)];
                let pan = if dist > 0.01 { ((d[0] * right[0] + d[2] * right[2]) as f32 / dist).clamp(-1.0, 1.0) } else { 0.0 };
                let l = crate::math::sqrt(((1.0 - pan) / 2.0) as f64) as f32;
                let r = crate::math::sqrt(((1.0 + pan) / 2.0) as f64) as f32;
                [g * l, g * r]
            }
        };
        if e.stream {
            self.start_music(e.file, vol.min(1.0));
            return;
        }
        let Some(data) = self.samples(e.file) else { return };
        let v = Voice { data, pos: 0.0, step: pit as f64, gain, category };
        unsafe { (self.lock)(self.device) };
        let mix = unsafe { &mut *MIX.load(Ordering::Acquire) };
        if mix.voices.len() < 48 {
            mix.voices.push(v);
        }
        unsafe { (self.unlock)(self.device) };
    }

    /// Starts a streamed sound (music, records) in place of the one playing.
    fn start_music(&mut self, file: u32, gain: f32) {
        let Some(&(off, size)) = self.files.get(file as usize) else { return };
        let Some(bytes) = self.pack.read_at(off, size as usize) else { return };
        let Some(stream) = ogg_stream::OggStream::open(bytes) else { return };
        let (rate, channels) = (stream.rate, stream.channels as usize);
        self.decoder = Some(Decoder { stream, rate, channels, t: 0.0 });
        let m = Music { buf: Vec::new(), read: 0, done: false, gain };
        unsafe { (self.lock)(self.device) };
        let mix = unsafe { &mut *MIX.load(Ordering::Acquire) };
        mix.music = Some(m);
        unsafe { (self.unlock)(self.device) };
    }

    pub fn music_playing(&self) -> bool {
        let mix = unsafe { &*MIX.load(Ordering::Acquire) };
        mix.music.as_ref().map_or(false, |m| !m.done || m.read < m.buf.len())
    }

    /// Decodes music ahead of the mixer (two seconds), spending at most ~2 ms a frame, and
    /// only takes the mixer's lock to hand the samples over.
    pub fn feed_music(&mut self) {
        let (queued, active) = {
            let mix = unsafe { &*MIX.load(Ordering::Acquire) };
            match mix.music.as_ref() {
                Some(m) => (m.buf.len() - m.read, true),
                None => (0, false),
            }
        };
        if !active {
            self.decoder = None;
            return;
        }
        let mut out: Vec<i16> = Vec::new();
        let mut done = false;
        if let Some(d) = self.decoder.as_mut() {
            let t0 = sys::now();
            let mut block = Vec::new();
            while queued + out.len() < RATE as usize * 2 && sys::now() - t0 < 0.002 {
                block.clear();
                if !d.stream.next_block(&mut block) {
                    done = true;
                    break;
                }
                let ch = d.channels.max(1);
                let frames = block.len() / ch;
                let ratio = d.rate as f64 / RATE as f64;
                while (d.t as usize) < frames {
                    let k = d.t as usize;
                    let s: f32 = block[k * ch..k * ch + ch].iter().sum::<f32>() / ch as f32;
                    out.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
                    d.t += ratio;
                }
                d.t -= frames as f64;
            }
            self.busy_ms += (sys::now() - t0) * 1000.0;
        } else {
            done = true;
        }
        if done {
            self.decoder = None;
        }
        unsafe { (self.lock)(self.device) };
        let mix = unsafe { &mut *MIX.load(Ordering::Acquire) };
        if let Some(m) = mix.music.as_mut() {
            if m.read > RATE as usize * 4 {
                m.buf.drain(..m.read);
                m.read = 0;
            }
            m.buf.extend_from_slice(&out);
            m.done |= done;
            if m.done && m.read >= m.buf.len() {
                mix.music = None;
            }
        }
        unsafe { (self.unlock)(self.device) };
    }

    pub fn has(&self, name: &str) -> bool {
        self.ids.contains_key(name) || self.named.contains_key(name)
    }
}
