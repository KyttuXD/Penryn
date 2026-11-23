//! The game's side of the voice chat (Simple Voice Chat, a server mod): the pieces `session`
//! leaves to the host: the UDP socket, the Opus codec, the microphone, and the speakers.
//!
//! Off unless the player turns it on in Options > Voice Chat. Receiving works everywhere; the
//! microphone needs SDL 2.0.5 or newer (audio capture), so Mac OS X 10.6's SDL 2.0.4 listens
//! only.

use super::net::Udp;
use super::proto;
use super::session::{Action, Event, Play, PlayKind, Status, VoiceSession};
use crate::audio::Audio;
use crate::client::Game;
use crate::opus::celt_enc::CeltEncoder;
use crate::opus::decoder::OpusDecoder;
use crate::sdl::{AudioSpec, Video, AUDIO_S16LSB};
use crate::sys;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_char, c_void};

/// What the options say, each frame.
pub struct Config {
    pub enabled: bool,
    /// Push to talk (else voice activation).
    pub push_to_talk: bool,
    /// Voice activation: the peak level, in dB below full scale, that opens the microphone.
    pub threshold_db: i32,
    /// Playback gain (1.0 is the speakers' natural level).
    pub volume: f32,
    /// The push-to-talk key is down.
    pub talk_held: bool,
}

/// The listener: where the camera is and which way it looks (degrees, Minecraft's yaw).
pub struct Listener {
    pub pos: [f64; 3],
    pub yaw: f32,
}

/// Samples in a frame (20 ms at 48 kHz) and the Opus bit rate used to send speech.
const FRAME: usize = 960;
const BITRATE: u32 = 32_000;
/// Frames the microphone stays open after the key goes up (5) or the voice falls quiet (25), as the mod does.
const HANG_PTT: u32 = 5;
const HANG_VOICE: u32 = 25;

/// SDL's audio capture, through queued audio (SDL 2.0.5+): found by name so that the older SDL
/// of the Mac OS X 10.6 build still loads.
struct SdlMic {
    dev: u32,
    dequeue: unsafe extern "C" fn(u32, *mut c_void, u32) -> u32,
    queued: unsafe extern "C" fn(u32) -> u32,
    close: Option<unsafe extern "C" fn(u32)>,
}

impl SdlMic {
    fn open(v: &Video) -> Option<SdlMic> {
        unsafe {
            // RTLD_DEFAULT: 0 on Linux, -2 on Mac OS X.
            let default = if cfg!(target_os = "macos") { (-2isize) as *mut c_void } else { core::ptr::null_mut() };
            let find = |name: &str| sys::dlsym(default, sys::cstr(name).as_ptr() as *const c_char);
            let (dq, qs, cl) = (find("SDL_DequeueAudio"), find("SDL_GetQueuedAudioSize"), find("SDL_CloseAudioDevice"));
            if dq.is_null() || qs.is_null() {
                return None; // an SDL without audio capture
            }
            let want = AudioSpec {
                freq: 48000,
                format: AUDIO_S16LSB,
                channels: 1,
                silence: 0,
                samples: FRAME as u16,
                padding: 0,
                size: 0,
                callback: None,
                userdata: core::ptr::null_mut(),
            };
            let mut got: AudioSpec = core::mem::zeroed();
            let dev = (v.sdl.SDL_OpenAudioDevice)(core::ptr::null(), 1, &want, &mut got, 0);
            if dev == 0 {
                return None;
            }
            (v.sdl.SDL_PauseAudioDevice)(dev, 0);
            Some(SdlMic {
                dev,
                dequeue: core::mem::transmute(dq),
                queued: core::mem::transmute(qs),
                close: if cl.is_null() { None } else { Some(core::mem::transmute(cl)) },
            })
        }
    }

    /// Reads what has been captured, up to `max` samples.
    fn read(&mut self, out: &mut Vec<i16>, max: usize) {
        let avail = unsafe { (self.queued)(self.dev) } as usize / 2;
        let n = avail.min(max);
        if n == 0 {
            return;
        }
        let at = out.len();
        out.resize(at + n, 0);
        let got = unsafe { (self.dequeue)(self.dev, out[at..].as_mut_ptr() as *mut c_void, (n * 2) as u32) } as usize / 2;
        out.truncate(at + got);
    }
}

impl Drop for SdlMic {
    fn drop(&mut self) {
        if let Some(c) = self.close {
            unsafe { c(self.dev) };
        }
    }
}

/// The microphone: on a Mac the system's own capture (AudioQueue, whatever SDL it bundles),
/// elsewhere SDL's.
enum Mic {
    Sdl(SdlMic),
    #[cfg(target_os = "macos")]
    Queue(super::mac_mic::MacMic),
}

impl Mic {
    fn open(v: &Video) -> Option<Mic> {
        #[cfg(target_os = "macos")]
        if let Some(m) = super::mac_mic::MacMic::open() {
            crate::say!("voice: microphone opened (AudioQueue)");
            return Some(Mic::Queue(m));
        }
        let m = SdlMic::open(v)?;
        crate::say!("voice: microphone opened (SDL)");
        Some(Mic::Sdl(m))
    }

    fn read(&mut self, out: &mut Vec<i16>, max: usize) {
        match self {
            Mic::Sdl(m) => m.read(out, max),
            #[cfg(target_os = "macos")]
            Mic::Queue(m) => m.read(out, max),
        }
    }
}

/// 32768 * 10^(db / 20) for a whole number of dB (db <= 0), without libm.
fn amplitude(db: i32) -> i32 {
    let mut a = 32768.0f64;
    let step = 1.122_018_454_301_963_4f64; // 10^(1/20)
    for _ in 0..(-db).clamp(0, 120) {
        a /= step;
    }
    a as i32
}

pub struct Voice {
    session: VoiceSession,
    udp: Option<Udp>,
    channels: BTreeMap<[u8; 16], OpusDecoder>,
    enc: CeltEncoder,
    mic: Option<Mic>,
    mic_tried: bool,
    pending: Vec<i16>,
    speaking: bool,
    hang: u32,
    enabled: bool,
    max_payload: usize,
    last_status: String,
    /// PENRYN_VOICE_TONE=1 (tests): speaks a 440 Hz tone instead of reading a microphone. The
    /// next 20 ms frame's time and the tone's phase.
    tone: Option<(u64, f64)>,
    heard: u32,
    /// PENRYN_VOICE_LEVEL=1 (tests): logs the microphone's peak level about once a second.
    level_log: bool,
    frames: u32,
}

impl Voice {
    pub fn new() -> Voice {
        let iv = Box::new(|| {
            let mut iv = [0u8; 12];
            match sys::read_urandom(12) {
                Some(b) if b.len() == 12 => iv.copy_from_slice(&b),
                _ => {
                    let t = (sys::now() * 1e9) as u64;
                    for (i, x) in iv.iter_mut().enumerate() {
                        *x = (t >> (i % 8 * 8)) as u8 ^ (i as u8).wrapping_mul(151);
                    }
                }
            }
            iv
        });
        let mut enc = CeltEncoder::new();
        // Wideband (8 kHz of audio) suits speech at this rate.
        enc.set_end_band(17);
        Voice {
            session: VoiceSession::new(proto::DEFAULT_COMPAT, iv),
            udp: None,
            channels: BTreeMap::new(),
            enc,
            mic: None,
            mic_tried: false,
            pending: Vec::new(),
            speaking: false,
            hang: 0,
            enabled: false,
            max_payload: 1275,
            last_status: String::new(),
            tone: sys::env("PENRYN_VOICE_TONE").map(|_| (0, 0.0)),
            heard: 0,
            level_log: sys::env("PENRYN_VOICE_LEVEL").is_some(),
            frames: 0,
        }
    }

    /// The status for the options screen.
    pub fn status_text(&self) -> String {
        let mut t = self.session.status().text();
        if self.session.is_connected() && self.enabled && self.mic.is_none() && self.mic_tried {
            t.push_str(" (no microphone: listening only)");
        }
        t
    }

    /// One frame of work: the plugin messages that came in, the UDP socket, time, the
    /// microphone; then what the session asked for.
    pub fn update(&mut self, now_ms: u64, g: &mut Game, audio: Option<&mut Audio>, video: &Video, cfg: &Config, me: &Listener) {
        g.voice_wanted = cfg.enabled;
        let mut actions: Vec<Action> = Vec::new();
        if cfg.enabled != self.enabled {
            self.enabled = cfg.enabled;
            actions.extend(self.session.handle_at(now_ms, if cfg.enabled { Event::Enable } else { Event::Disable }));
            if !cfg.enabled {
                self.mic = None;
                self.mic_tried = false;
                self.pending.clear();
                self.speaking = false;
            }
        }
        if !self.enabled && !self.session.is_connected() && g.voice_in.is_empty() {
            self.run(actions, g, None, me, cfg);
            return;
        }
        for (channel, payload) in core::mem::take(&mut g.voice_in) {
            let ev = if channel == proto::CH_SECRET { Event::Secret(payload) } else { Event::Plugin { channel, payload } };
            actions.extend(self.session.handle_at(now_ms, ev));
        }
        if let Some(u) = self.udp.as_mut() {
            let mut buf = [0u8; 2048];
            for _ in 0..64 {
                match u.recv(&mut buf) {
                    Some(n) => actions.extend(self.session.handle_at(now_ms, Event::Udp(buf[..n].to_vec()))),
                    None => break,
                }
            }
        }
        actions.extend(self.session.handle_at(now_ms, Event::Tick(now_ms)));
        if self.enabled && self.session.is_connected() {
            self.microphone(now_ms, video, cfg, &mut actions);
        }
        self.run(actions, g, audio, me, cfg);
    }

    fn microphone(&mut self, now_ms: u64, video: &Video, cfg: &Config, actions: &mut Vec<Action>) {
        if let Some((next, phase)) = self.tone.as_mut() {
            // A test tone at the pace of a microphone.
            if *next == 0 {
                *next = now_ms;
            }
            while *next <= now_ms {
                for _ in 0..FRAME {
                    self.pending.push((crate::math::sin(*phase) * 8000.0) as i16);
                    *phase += 2.0 * core::f64::consts::PI * 440.0 / 48000.0;
                }
                *next += 20;
            }
        } else {
            if self.mic.is_none() && !self.mic_tried {
                self.mic_tried = true;
                self.mic = Mic::open(video);
            }
            let Some(mic) = self.mic.as_mut() else { return };
            mic.read(&mut self.pending, FRAME * 8);
        }
        // More than a quarter second behind (a stall): keep the newest.
        if self.pending.len() > FRAME * 12 {
            let cut = self.pending.len() - FRAME * 4;
            self.pending.drain(..cut);
        }
        let threshold = amplitude(cfg.threshold_db);
        while self.pending.len() >= FRAME {
            let frame: Vec<i16> = self.pending.drain(..FRAME).collect();
            let peak = frame.iter().map(|s| (*s as i32).abs()).max().unwrap_or(0);
            self.frames += 1;
            if self.level_log && self.frames % 50 == 0 {
                crate::say!("voice: microphone peak {} of 32768", peak);
            }
            let loud = self.tone.is_some() || if cfg.push_to_talk { cfg.talk_held } else { peak >= threshold };
            if loud {
                self.hang = if cfg.push_to_talk { HANG_PTT } else { HANG_VOICE };
            }
            if loud || self.hang > 0 {
                if !loud {
                    self.hang -= 1;
                }
                let opus = self.enc.encode(&frame, BITRATE, self.max_payload.min(1275));
                self.speaking = true;
                actions.extend(self.session.handle_at(now_ms, Event::Mic { opus, whispering: false }));
            } else if self.speaking {
                self.speaking = false;
                actions.extend(self.session.handle_at(now_ms, Event::EndOfSpeech));
            }
        }
    }

    fn run(&mut self, actions: Vec<Action>, g: &mut Game, mut audio: Option<&mut Audio>, me: &Listener, cfg: &Config) {
        for a in actions {
            match a {
                Action::SendPlugin { channel, payload } => {
                    let _ = g.send_payload(&channel, &payload);
                }
                Action::Connect(info) => {
                    self.max_payload = info.max_payload;
                    let host = info.host.clone().unwrap_or_else(|| g.server_host.clone());
                    match Udp::connect(&host, info.port) {
                        Ok(u) => self.udp = Some(u),
                        Err(e) => g.add_chat(format!("\u{a7}c[Voice chat: {}]", e)),
                    }
                }
                Action::Close => self.udp = None,
                Action::SendUdp(b) => {
                    if let Some(u) = self.udp.as_mut() {
                        u.send(&b);
                    }
                }
                Action::Play(p) => self.play(p, g, audio.as_deref_mut(), me, cfg),
                Action::CloseChannel(id) => {
                    self.channels.remove(&id);
                }
                Action::Status(s) => {
                    let t = s.text();
                    if t != self.last_status {
                        self.last_status = t.clone();
                        if s != Status::Disabled {
                            g.add_chat(format!("\u{a7}7[{}]", t));
                        }
                    }
                }
                Action::PlayerState(_) | Action::Group(_) => {}
            }
        }
    }

    /// Decodes a sound packet (concealing the packets lost before it) and hands it to the mixer
    /// at the volume and pan the mod would give it.
    fn play(&mut self, p: Play, g: &Game, mut audio: Option<&mut Audio>, me: &Listener, cfg: &Config) {
        let dec = self.channels.entry(p.channel_id).or_insert_with(OpusDecoder::new);
        if p.reset {
            dec.reset();
        }
        if p.opus.is_empty() {
            dec.reset();
            return;
        }
        let (volume, pan) = match &p.kind {
            PlayKind::Group => (1.0f32, (1.0f32, 1.0f32)),
            PlayKind::Player { distance, .. } => {
                if p.sender == g.uuid {
                    (1.0, (1.0, 1.0))
                } else {
                    let Some(e) = g.entities.values().find(|e| e.uuid == p.sender) else { return };
                    match spatial(me, [e.pos[0], e.pos[1] + 1.62, e.pos[2]], *distance as f64) {
                        Some(v) => v,
                        None => return,
                    }
                }
            }
            PlayKind::Location { x, y, z, distance } => match spatial(me, [*x, *y, *z], *distance as f64) {
                Some(v) => v,
                None => return,
            },
        };
        let gain = [cfg.volume * volume * pan.0, cfg.volume * volume * pan.1];
        let id = u64::from_be_bytes([p.channel_id[0], p.channel_id[1], p.channel_id[2], p.channel_id[3], p.channel_id[4], p.channel_id[5], p.channel_id[6], p.channel_id[7]]);
        let mut buf = alloc::vec![0f32; 5760];
        self.heard += 1;
        if self.heard % 50 == 1 {
            crate::say!("voice: packet {} from {:02x}{:02x}{:02x}{:02x}, {} bytes, gain {:.2} {:.2}", self.heard, p.channel_id[0], p.channel_id[1], p.channel_id[2], p.channel_id[3], p.opus.len(), gain[0], gain[1]);
        }
        for _ in 0..p.lost.min(5) {
            let n = dec.decode_lost_float(FRAME, &mut buf);
            if let Some(a) = audio.as_deref_mut() {
                a.voice_push(id, &buf[..n.min(buf.len())], gain);
            }
        }
        if let Ok(n) = dec.decode_float(&p.opus, &mut buf) {
            if self.heard % 50 == 1 {
                let peak = buf[..n.min(buf.len())].iter().fold(0f32, |m, s| if *s > m { *s } else if -*s > m { -*s } else { m });
                crate::say!("voice: decoded {} samples, peak {:.3}", n, peak);
            }
            if let Some(a) = audio.as_deref_mut() {
                a.voice_push(id, &buf[..n.min(buf.len())], gain);
            }
        }
    }
}

/// How loud and where a sound at `at` is heard by `me`: the mod's distance fall-off (full
/// to half the range, then straight down to nothing at the range) and its stereo pan (the
/// "reduced" speaker of protocol section 7.4). None: beyond the range, not played.
fn spatial(me: &Listener, at: [f64; 3], range: f64) -> Option<(f32, (f32, f32))> {
    let (dx, dy, dz) = (at[0] - me.pos[0], at[1] - me.pos[1], at[2] - me.pos[2]);
    let d = crate::math::sqrt(dx * dx + dy * dy + dz * dz);
    if d > range + 1.0 {
        return None;
    }
    let half = range / 2.0;
    let volume = if d <= half { 1.0 } else { (1.0 - (d - half) / half).clamp(0.0, 1.0) } as f32;
    // The pan: the direction from the listener, turned by where he looks.
    let flat = crate::math::sqrt(dx * dx + dz * dz);
    let (ux, uz) = if flat > 1e-6 { (dx / flat, dz / flat) } else { (0.0, 1.0) };
    let diff_angle = crate::math::atan2(-ux, uz) * (180.0 / crate::math::PI);
    let angle = crate::entities::wrap_deg((diff_angle - me.yaw as f64) as f32) as f64;
    let dif = (dy.abs() / 32.0).min(1.0);
    let rot = angle / 180.0;
    let mut perc = rot;
    if rot < -0.5 {
        perc = -(0.5 + (rot + 0.5));
    } else if rot > 0.5 {
        perc = 0.5 - (rot - 0.5);
    }
    perc *= 1.0 - dif;
    let min_vol = 0.3;
    let mut left = if perc < 0.0 { (perc * 1.4).abs() + min_vol } else { min_vol };
    let mut right = if perc >= 0.0 { perc * 1.4 + min_vol } else { min_vol };
    let fill = 1.0 - left.max(right);
    left += fill;
    right += fill;
    Some((volume, (left as f32, right as f32)))
}
