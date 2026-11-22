//! SDL2 and OpenGL 1.5 (fixed function), both looked up at run time.
//!
//! Only functions present in SDL 2.0.0 and OpenGL 1.5 are used, so the official SDL 2.0.4
//! framework on Mac OS X 10.6 and a Radeon HD 2600's OpenGL 2.1 both do. The approach and the
//! event offsets are VoxelRust's (src/sys/sdl.rs, gl.rs; MIT, same author).

use crate::sys;
use alloc::boxed::Box;
use alloc::string::String;
use core::ffi::{c_char, c_int, c_void};

#[repr(C, align(8))]
pub struct Event(pub [u8; 64]);

impl Event {
    pub fn u32_at(&self, o: usize) -> u32 {
        u32::from_ne_bytes([self.0[o], self.0[o + 1], self.0[o + 2], self.0[o + 3]])
    }
    pub fn i32_at(&self, o: usize) -> i32 {
        self.u32_at(o) as i32
    }
}

pub const INIT_VIDEO: u32 = 0x20;
pub const WINDOW_OPENGL: u32 = 0x2;
pub const WINDOW_RESIZABLE: u32 = 0x20;
pub const WINDOW_FULLSCREEN_DESKTOP: u32 = 0x1001;
pub const WINDOWPOS_CENTERED: c_int = 0x2FFF_0000;
pub const GL_DOUBLEBUFFER: c_int = 5;
pub const GL_DEPTH_SIZE: c_int = 6;

pub const EV_QUIT: u32 = 0x100;
pub const EV_WINDOW: u32 = 0x200;
pub const EV_KEYDOWN: u32 = 0x300;
pub const EV_KEYUP: u32 = 0x301;
pub const EV_TEXTINPUT: u32 = 0x303;
pub const EV_MOUSEMOTION: u32 = 0x400;
pub const EV_MOUSEDOWN: u32 = 0x401;
pub const EV_MOUSEUP: u32 = 0x402;
pub const EV_MOUSEWHEEL: u32 = 0x403;
pub const WINEV_SIZE_CHANGED: u8 = 6;
pub const WINEV_FOCUS_LOST: u8 = 13;

macro_rules! api {
    ($struct:ident, $prefix:expr, $abi:literal, $($name:ident($($t:ty),*) $(-> $r:ty)?;)*) => {
        #[allow(non_snake_case)]
        pub struct $struct { $(pub $name: unsafe extern $abi fn($($t),*) $(-> $r)?,)* }
        impl $struct {
            unsafe fn load(get: &dyn Fn(&str) -> *mut c_void) -> Result<$struct, &'static str> {
                Ok($struct {
                    $($name: {
                        let p = get(stringify!($name));
                        if p.is_null() { return Err(stringify!($name)); }
                        core::mem::transmute::<*mut c_void, unsafe extern $abi fn($($t),*) $(-> $r)?>(p)
                    },)*
                })
            }
        }
    };
}

api! { Sdl, "", "C",
    SDL_Init(u32) -> c_int;
    SDL_Quit();
    SDL_GetError() -> *const c_char;
    SDL_CreateWindow(*const c_char, c_int, c_int, c_int, c_int, u32) -> *mut c_void;
    SDL_GetWindowSize(*mut c_void, *mut c_int, *mut c_int);
    SDL_SetWindowFullscreen(*mut c_void, u32) -> c_int;
    SDL_SetWindowTitle(*mut c_void, *const c_char);
    SDL_GL_SetAttribute(c_int, c_int) -> c_int;
    SDL_GL_CreateContext(*mut c_void) -> *mut c_void;
    SDL_GL_SetSwapInterval(c_int) -> c_int;
    SDL_GL_SwapWindow(*mut c_void);
    SDL_GL_GetProcAddress(*const c_char) -> *mut c_void;
    SDL_PollEvent(*mut Event) -> c_int;
    SDL_SetRelativeMouseMode(c_int) -> c_int;
    SDL_StartTextInput();
    SDL_StopTextInput();
    SDL_GetClipboardText() -> *mut c_char;
    SDL_free(*mut c_void);
    SDL_Delay(u32);
    SDL_InitSubSystem(u32) -> c_int;
    SDL_OpenAudioDevice(*const c_char, c_int, *const AudioSpec, *mut AudioSpec, c_int) -> u32;
    SDL_PauseAudioDevice(u32, c_int);
    SDL_LockAudioDevice(u32);
    SDL_UnlockAudioDevice(u32);
}

/// SDL_AudioSpec (the same in every SDL2).
#[repr(C)]
pub struct AudioSpec {
    pub freq: c_int,
    pub format: u16,
    pub channels: u8,
    pub silence: u8,
    pub samples: u16,
    pub padding: u16,
    pub size: u32,
    pub callback: Option<unsafe extern "C" fn(*mut c_void, *mut u8, c_int)>,
    pub userdata: *mut c_void,
}

pub const INIT_AUDIO: u32 = 0x10;
pub const AUDIO_S16LSB: u16 = 0x8010;

pub const COLOR_BUFFER_BIT: u32 = 0x4000;
pub const DEPTH_BUFFER_BIT: u32 = 0x100;
pub const LINES: u32 = 1;
pub const QUADS: u32 = 7;
pub const DEPTH_TEST: u32 = 0x0B71;
pub const CULL_FACE: u32 = 0x0B44;
pub const BLEND: u32 = 0x0BE2;
pub const FOG: u32 = 0x0B60;
pub const MODELVIEW: u32 = 0x1700;
pub const PROJECTION: u32 = 0x1701;
pub const UNSIGNED_BYTE: u32 = 0x1401;
pub const FLOAT: u32 = 0x1406;
pub const SHORT: u32 = 0x1402;
pub const TEXTURE: u32 = 0x1702;
pub const VERTEX_ARRAY: u32 = 0x8074;
pub const COLOR_ARRAY: u32 = 0x8076;
pub const ARRAY_BUFFER: u32 = 0x8892;
pub const STATIC_DRAW: u32 = 0x88E4;
pub const FOG_MODE: u32 = 0x0B65;
pub const FOG_START: u32 = 0x0B63;
pub const FOG_END: u32 = 0x0B64;
pub const FOG_COLOR: u32 = 0x0B66;
pub const LINEAR_FOG: i32 = 0x2601;
pub const SRC_ALPHA: u32 = 0x0302;
pub const ONE_MINUS_SRC_COLOR: u32 = 0x0301;
pub const SRC_COLOR: u32 = 0x0300;
pub const ONE: u32 = 1;
pub const DST_COLOR: u32 = 0x0306;
pub const ONE_MINUS_DST_COLOR: u32 = 0x0307;
pub const ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
pub const BACK: u32 = 0x0405;
pub const RENDERER: u32 = 0x1F01;
pub const RGBA: u32 = 0x1908;
pub const TEXTURE_2D: u32 = 0x0DE1;
pub const TEXTURE_COORD_ARRAY: u32 = 0x8078;
pub const ALPHA_TEST: u32 = 0x0BC0;
pub const GREATER: u32 = 0x0204;
pub const LESS: u32 = 0x0201;
pub const LEQUAL: u32 = 0x0203;
pub const TEXTURE_MAG_FILTER: u32 = 0x2800;
pub const TEXTURE_MIN_FILTER: u32 = 0x2801;
pub const TEXTURE_MAX_LEVEL: u32 = 0x813D;
pub const NEAREST: i32 = 0x2600;
pub const NEAREST_MIPMAP_LINEAR: i32 = 0x2702;
pub const UNPACK_ALIGNMENT: u32 = 0x0CF5;
pub const PACK_ALIGNMENT: u32 = 0x0D05;
pub const VERSION: u32 = 0x1F02;

api! { Gl, "gl", "system",
    Clear(u32);
    ClearColor(f32, f32, f32, f32);
    Enable(u32);
    Disable(u32);
    Viewport(i32, i32, i32, i32);
    MatrixMode(u32);
    LoadMatrixf(*const f32);
    LoadIdentity();
    EnableClientState(u32);
    DisableClientState(u32);
    VertexPointer(i32, u32, i32, *const c_void);
    ColorPointer(i32, u32, i32, *const c_void);
    DrawArrays(u32, i32, i32);
    Fogf(u32, f32);
    Fogfv(u32, *const f32);
    Fogi(u32, i32);
    BlendFunc(u32, u32);
    DepthMask(u8);
    DepthFunc(u32);
    CullFace(u32);
    GetString(u32) -> *const u8;
    LineWidth(f32);
    GenBuffers(i32, *mut u32);
    BindBuffer(u32, u32);
    BufferData(u32, isize, *const c_void, u32);
    DeleteBuffers(i32, *const u32);
    ReadPixels(i32, i32, i32, i32, u32, u32, *mut c_void);
    GenTextures(i32, *mut u32);
    BindTexture(u32, u32);
    TexImage2D(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
    TexParameteri(u32, u32, i32);
    TexCoordPointer(i32, u32, i32, *const c_void);
    AlphaFunc(u32, f32);
    PixelStorei(u32, i32);
}

pub struct Video {
    pub sdl: Sdl,
    pub gl: Gl,
    pub window: *mut c_void,
}

#[cfg(target_os = "linux")]
const SDL_NAMES: &[&str] = &["libSDL2-2.0.so.0", "libSDL2.so"];
#[cfg(target_os = "macos")]
const SDL_NAMES: &[&str] = &[
    "@executable_path/libSDL2-2.0.0.dylib",
    "@executable_path/../Frameworks/SDL2.framework/SDL2",
    "@executable_path/SDL2.framework/SDL2",
    "/Library/Frameworks/SDL2.framework/SDL2",
    "/usr/local/lib/libSDL2-2.0.0.dylib",
    "/opt/local/lib/libSDL2-2.0.0.dylib",
];
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const SDL_NAMES: &[&str] = &["libSDL2.so"];

impl Video {
    pub fn open(title: &str, w: i32, h: i32, fullscreen: bool) -> Result<Video, String> {
        let mut lib = core::ptr::null_mut();
        let mut names: alloc::vec::Vec<String> = SDL_NAMES.iter().map(|s| String::from(*s)).collect();
        if let Some(p) = sys::env("PENRYN_SDL2") {
            names.insert(0, p);
        }
        for n in &names {
            let c = sys::cstr(n);
            // RTLD_NOW | RTLD_GLOBAL
            let global = if cfg!(target_os = "macos") { 0x8 } else { 0x100 };
            lib = unsafe { sys::dlopen(c.as_ptr() as *const c_char, sys::RTLD_NOW | global) };
            if !lib.is_null() {
                break;
            }
        }
        if lib.is_null() {
            return Err(String::from(
                "SDL2 was not found: put SDL2.framework beside penryn (or set PENRYN_SDL2 to the library)",
            ));
        }
        let sym = |n: &str| unsafe { sys::dlsym(lib, sys::cstr(n).as_ptr() as *const c_char) };
        let sdl = unsafe { Sdl::load(&sym) }.map_err(|m| alloc::format!("this SDL2 has no {}", m))?;
        unsafe {
            if (sdl.SDL_Init)(INIT_VIDEO) != 0 {
                return Err(alloc::format!("SDL_Init: {}", sys::from_cstr((sdl.SDL_GetError)())));
            }
            (sdl.SDL_GL_SetAttribute)(GL_DOUBLEBUFFER, 1);
            (sdl.SDL_GL_SetAttribute)(GL_DEPTH_SIZE, 24);
            let t = sys::cstr(title);
            let mut flags = WINDOW_OPENGL | WINDOW_RESIZABLE;
            if fullscreen {
                flags |= WINDOW_FULLSCREEN_DESKTOP;
            }
            let window = (sdl.SDL_CreateWindow)(t.as_ptr() as *const c_char, WINDOWPOS_CENTERED, WINDOWPOS_CENTERED, w, h, flags);
            if window.is_null() {
                return Err(alloc::format!("cannot open a window: {}", sys::from_cstr((sdl.SDL_GetError)())));
            }
            if (sdl.SDL_GL_CreateContext)(window).is_null() {
                return Err(alloc::format!("no OpenGL: {}", sys::from_cstr((sdl.SDL_GetError)())));
            }
            (sdl.SDL_GL_SetSwapInterval)(1);
            let get = |n: &str| {
                let mut p = (sdl.SDL_GL_GetProcAddress)(sys::cstr(&alloc::format!("gl{}", n)).as_ptr() as *const c_char);
                if p.is_null() {
                    p = (sdl.SDL_GL_GetProcAddress)(sys::cstr(&alloc::format!("gl{}ARB", n)).as_ptr() as *const c_char);
                }
                p
            };
            let gl = Gl::load(&get).map_err(|m| alloc::format!("this OpenGL driver has no gl{} (OpenGL 1.5 is needed)", m))?;
            Ok(Video { sdl, gl, window })
        }
    }

    pub fn size(&self) -> (i32, i32) {
        let (mut w, mut h) = (0, 0);
        unsafe { (self.sdl.SDL_GetWindowSize)(self.window, &mut w, &mut h) };
        (w.max(1), h.max(1))
    }

    pub fn gl_string(&self, what: u32) -> String {
        let p = unsafe { (self.gl.GetString)(what) };
        if p.is_null() {
            String::new()
        } else {
            unsafe { sys::from_cstr(p as *const c_char) }
        }
    }

    pub fn poll(&self) -> Option<Event> {
        let mut e = Box::new(Event([0; 64]));
        if unsafe { (self.sdl.SDL_PollEvent)(&mut *e) } != 0 {
            Some(*e)
        } else {
            None
        }
    }

    pub fn swap(&self) {
        unsafe { (self.sdl.SDL_GL_SwapWindow)(self.window) }
    }

    pub fn grab_mouse(&self, on: bool) {
        unsafe { (self.sdl.SDL_SetRelativeMouseMode)(on as c_int) };
    }

    pub fn text_input(&self, on: bool) {
        unsafe {
            if on {
                (self.sdl.SDL_StartTextInput)()
            } else {
                (self.sdl.SDL_StopTextInput)()
            }
        }
    }

    pub fn clipboard(&self) -> String {
        unsafe {
            let p = (self.sdl.SDL_GetClipboardText)();
            if p.is_null() {
                return String::new();
            }
            let s = sys::from_cstr(p);
            (self.sdl.SDL_free)(p as *mut c_void);
            s
        }
    }
}

/// The window's picture as a PNG (stored zlib, no compression), for tests and F2.
pub fn screenshot_png(gl: &Gl, w: i32, h: i32) -> alloc::vec::Vec<u8> {
    let mut px = alloc::vec![0u8; (w * h * 4) as usize];
    unsafe {
        (gl.PixelStorei)(PACK_ALIGNMENT, 1);
        (gl.ReadPixels)(0, 0, w, h, RGBA, UNSIGNED_BYTE, px.as_mut_ptr() as *mut c_void);
    }
    let mut raw = alloc::vec::Vec::with_capacity(((w * 3 + 1) * h) as usize);
    for y in (0..h).rev() {
        raw.push(0);
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            raw.extend_from_slice(&px[i..i + 3]);
        }
    }
    let mut out = alloc::vec::Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let chunk = |out: &mut alloc::vec::Vec<u8>, kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = alloc::vec::Vec::from(kind);
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = alloc::vec::Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &crate::zlib::store(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn crc32(d: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in d {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}
