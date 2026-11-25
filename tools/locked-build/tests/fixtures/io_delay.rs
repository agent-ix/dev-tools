// SPDX-License-Identifier: MIT
//! Linux `LD_PRELOAD` fixture: delays selected observer config reads or state fsyncs.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

static CONFIG_READS: AtomicUsize = AtomicUsize::new(0);
static STATE_FSYNCS: AtomicUsize = AtomicUsize::new(0);

fn selected(fd: c_int, mode: &str, needle: &str) -> bool {
    std::env::var("LOCKED_BUILD_DELAY_IO").is_ok_and(|value| value == mode)
        && std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .is_ok_and(|path| path.to_string_lossy().contains(needle))
}

fn next(symbol: &CStr) -> *mut c_void {
    // SAFETY: RTLD_NEXT is the all-ones pointer and the name is NUL terminated.
    unsafe { dlsym(usize::MAX as *mut c_void, symbol.as_ptr()) }
}

/// # Safety
/// Same contract as `read(2)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn read(fd: c_int, buffer: *mut c_void, length: usize) -> isize {
    if selected(fd, "config-read", "policy.json") && CONFIG_READS.fetch_add(1, Ordering::SeqCst) == 0
    {
        std::thread::sleep(Duration::from_secs(2));
    }
    // SAFETY: the next `read` symbol has this signature; arguments are forwarded unchanged.
    unsafe {
        let real: unsafe extern "C" fn(c_int, *mut c_void, usize) -> isize =
            std::mem::transmute(next(c"read"));
        real(fd, buffer, length)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fsync(fd: c_int) -> c_int {
    if selected(fd, "state-fsync", "observe.json.next")
        && STATE_FSYNCS.fetch_add(1, Ordering::SeqCst) == 0
    {
        std::thread::sleep(Duration::from_secs(2));
    }
    if selected(fd, "late-then-stalled", "observe.json.next") {
        let first = STATE_FSYNCS.fetch_add(1, Ordering::SeqCst) == 0;
        std::thread::sleep(Duration::from_millis(if first { 900 } else { 5_000 }));
    }
    // SAFETY: the next `fsync` symbol has this signature.
    unsafe {
        let real: unsafe extern "C" fn(c_int) -> c_int = std::mem::transmute(next(c"fsync"));
        real(fd)
    }
}
