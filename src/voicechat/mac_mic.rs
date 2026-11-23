//! The Mac's microphone through AudioQueue (AudioToolbox, there since Mac OS X 10.5): the SDL2
//! Penryn bundles for Mac OS X 10.6 (2.0.4) cannot capture audio, so the voice chat asks the
//! system directly. AudioToolbox is opened at run time (dlopen), so nothing is linked and a Mac
//! without it simply has no microphone.
//!
//! The queue calls `on_input` on a thread of its own with each buffer of 16-bit mono samples at
//! 48 kHz; they go into a ring that the game thread reads (one writer, one reader, a counter that
//! only grows says how much has been written).

use crate::sys;
use alloc::vec::Vec;
use core::ffi::{c_char, c_void};
use core::sync::atomic::{AtomicUsize, Ordering};

const RING: usize = 48000;
static mut SAMPLES: [i16; RING] = [0; RING];
static WRITTEN: AtomicUsize = AtomicUsize::new(0);

/// AudioStreamBasicDescription.
#[repr(C)]
struct Format {
    rate: f64,
    format: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits: u32,
    reserved: u32,
}

/// AudioQueueBuffer.
#[repr(C)]
struct Buffer {
    capacity: u32,
    data: *mut c_void,
    size: u32,
    user: *mut c_void,
    desc_capacity: u32,
    descs: *mut c_void,
    desc_count: u32,
}

type InputCallback = extern "C" fn(*mut c_void, *mut c_void, *mut Buffer, *const c_void, u32, *const c_void);
type NewInput = unsafe extern "C" fn(*const Format, InputCallback, *mut c_void, *mut c_void, *mut c_void, u32, *mut *mut c_void) -> i32;
type Allocate = unsafe extern "C" fn(*mut c_void, u32, *mut *mut Buffer) -> i32;
type Enqueue = unsafe extern "C" fn(*mut c_void, *mut Buffer, u32, *const c_void) -> i32;
type Start = unsafe extern "C" fn(*mut c_void, *const c_void) -> i32;
type Stop = unsafe extern "C" fn(*mut c_void, u8) -> i32;

static mut ENQUEUE: Option<Enqueue> = None;

extern "C" fn on_input(_user: *mut c_void, queue: *mut c_void, buf: *mut Buffer, _time: *const c_void, _packets: u32, _descs: *const c_void) {
    unsafe {
        let b = &*buf;
        let n = b.size as usize / 2;
        let src = core::slice::from_raw_parts(b.data as *const i16, n);
        let ring = &mut *core::ptr::addr_of_mut!(SAMPLES);
        let mut w = WRITTEN.load(Ordering::Relaxed);
        for &s in src {
            ring[w % RING] = s;
            w += 1;
        }
        WRITTEN.store(w, Ordering::Release);
        // Give the buffer back to the queue to be filled again.
        if let Some(enq) = *core::ptr::addr_of!(ENQUEUE) {
            enq(queue, buf, 0, core::ptr::null());
        }
    }
}

pub struct MacMic {
    queue: *mut c_void,
    stop: Stop,
    dispose: Stop,
    read: usize,
}

impl MacMic {
    /// Opens the default input device and starts capturing. None when AudioToolbox or an input
    /// device is missing. (On Mac OS X 10.14 and newer the first start asks the player for
    /// permission; until it is given the queue delivers silence.)
    pub fn open() -> Option<MacMic> {
        unsafe {
            let lib = sys::dlopen(sys::cstr("/System/Library/Frameworks/AudioToolbox.framework/AudioToolbox").as_ptr() as *const c_char, sys::RTLD_NOW);
            if lib.is_null() {
                return None;
            }
            let find = |n: &str| sys::dlsym(lib, sys::cstr(n).as_ptr() as *const c_char);
            let (new_input, allocate, enqueue, start, stop, dispose) = (
                find("AudioQueueNewInput"),
                find("AudioQueueAllocateBuffer"),
                find("AudioQueueEnqueueBuffer"),
                find("AudioQueueStart"),
                find("AudioQueueStop"),
                find("AudioQueueDispose"),
            );
            if [new_input, allocate, enqueue, start, stop, dispose].iter().any(|p| p.is_null()) {
                return None;
            }
            let new_input: NewInput = core::mem::transmute(new_input);
            let allocate: Allocate = core::mem::transmute(allocate);
            let enqueue: Enqueue = core::mem::transmute(enqueue);
            let start: Start = core::mem::transmute(start);
            let stop: Stop = core::mem::transmute(stop);
            let dispose: Stop = core::mem::transmute(dispose);
            *core::ptr::addr_of_mut!(ENQUEUE) = Some(enqueue);
            // 'lpcm', signed integer | packed (4 | 8): 16-bit mono at 48 kHz, as the voice chat wants.
            let format = Format { rate: 48000.0, format: 0x6C70_636D, flags: 12, bytes_per_packet: 2, frames_per_packet: 1, bytes_per_frame: 2, channels: 1, bits: 16, reserved: 0 };
            let mut queue: *mut c_void = core::ptr::null_mut();
            if new_input(&format, on_input, core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut(), 0, &mut queue) != 0 || queue.is_null() {
                return None;
            }
            // Four buffers of 20 ms.
            for _ in 0..4 {
                let mut b: *mut Buffer = core::ptr::null_mut();
                if allocate(queue, 1920, &mut b) == 0 && !b.is_null() {
                    enqueue(queue, b, 0, core::ptr::null());
                }
            }
            if start(queue, core::ptr::null()) != 0 {
                dispose(queue, 1);
                return None;
            }
            Some(MacMic { queue, stop, dispose, read: WRITTEN.load(Ordering::Acquire) })
        }
    }

    /// What has been captured since the last call, up to `max` samples (older audio is dropped
    /// when the reader falls a whole ring behind).
    pub fn read(&mut self, out: &mut Vec<i16>, max: usize) {
        let written = WRITTEN.load(Ordering::Acquire);
        if written.wrapping_sub(self.read) > RING {
            self.read = written - RING / 2;
        }
        let ring = unsafe { &*core::ptr::addr_of!(SAMPLES) };
        let n = written.wrapping_sub(self.read).min(max);
        for i in 0..n {
            out.push(ring[(self.read + i) % RING]);
        }
        self.read += n;
    }
}

impl Drop for MacMic {
    fn drop(&mut self) {
        unsafe {
            (self.stop)(self.queue, 1);
            (self.dispose)(self.queue, 1);
        }
    }
}
