//! Work on a thread of its own: a server-list ping, joining a server, signing in. The menus
//! keep drawing while it runs and take the result when it is there (vanilla runs these on its
//! network threads). Threads get 2 MB stacks (Mac OS X gives other threads only 512 KB).

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

struct Slot<T> {
    done: AtomicBool,
    value: UnsafeCell<Option<T>>,
}

// One thread writes the value once, then sets `done`; the other reads it only after `done`.
unsafe impl<T: Send> Sync for Slot<T> {}
unsafe impl<T: Send> Send for Slot<T> {}

/// A value some thread is making.
pub struct Task<T> {
    slot: Arc<Slot<T>>,
    taken: bool,
}

/// Moves a value to another thread when its type does not say it may (a `Game` holds raw
/// pointers nobody else touches).
pub struct Sendable<T>(pub T);
unsafe impl<T> Send for Sendable<T> {}

struct Job {
    run: Box<dyn FnOnce() + Send>,
}

extern "C" fn trampoline(arg: *mut c_void) -> *mut c_void {
    let job = unsafe { Box::from_raw(arg as *mut Job) };
    (job.run)();
    core::ptr::null_mut()
}

/// Runs `f` on a new thread. If no thread can be started it runs here and now.
pub fn spawn<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Task<T> {
    let slot = Arc::new(Slot { done: AtomicBool::new(false), value: UnsafeCell::new(None) });
    let theirs = slot.clone();
    let job = Box::new(Job {
        run: Box::new(move || {
            let v = f();
            unsafe { *theirs.value.get() = Some(v) };
            theirs.done.store(true, Ordering::Release);
        }),
    });
    let raw = Box::into_raw(job);
    // pthread_attr_t is 56 bytes on Linux and 64 on Mac OS X: room for either.
    let mut attr = [0u64; 16];
    let mut t: usize = 0;
    let started = unsafe {
        let a = attr.as_mut_ptr() as *mut c_void;
        let have_attr = crate::sys::pthread_attr_init(a) == 0;
        if have_attr {
            crate::sys::pthread_attr_setstacksize(a, 2 << 20);
        }
        let r = crate::sys::pthread_create(&mut t, if have_attr { a } else { core::ptr::null() }, trampoline, raw as *mut c_void);
        if have_attr {
            crate::sys::pthread_attr_destroy(a);
        }
        r == 0
    };
    if started {
        unsafe { crate::sys::pthread_detach(t) };
    } else {
        trampoline(raw as *mut c_void);
    }
    Task { slot, taken: false }
}

impl<T> Task<T> {
    /// The result, once (None while the thread works, and after it was taken).
    pub fn take(&mut self) -> Option<T> {
        if self.taken || !self.slot.done.load(Ordering::Acquire) {
            return None;
        }
        self.taken = true;
        unsafe { (*self.slot.value.get()).take() }
    }
}
