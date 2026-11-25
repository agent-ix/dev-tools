// SPDX-License-Identifier: MIT
//! Capture signal dispositions before the Rust runtime changes SIGPIPE.

use std::sync::atomic::{AtomicBool, Ordering};

static SIGPIPE_IGNORED: AtomicBool = AtomicBool::new(false);
static SIGCHLD_IGNORED: AtomicBool = AtomicBool::new(false);

#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
#[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
#[used]
static CAPTURE_INHERITED_SIGNALS: extern "C" fn() = capture_inherited_signals;

extern "C" fn capture_inherited_signals() {
    capture(libc::SIGPIPE, &SIGPIPE_IGNORED);
    capture(libc::SIGCHLD, &SIGCHLD_IGNORED);
}

fn capture(signal: libc::c_int, destination: &AtomicBool) {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    // SAFETY: `sigaction` only reads the current action because the new-action pointer is null;
    // the old-action pointer addresses writable storage with the correct libc layout.
    let result = unsafe { libc::sigaction(signal, std::ptr::null(), action.as_mut_ptr()) };
    if result == 0 {
        // SAFETY: libc initialized the old-action structure on success.
        let action = unsafe { action.assume_init() };
        destination.store(action.sa_sigaction == libc::SIG_IGN, Ordering::Relaxed);
    }
}

pub(crate) fn sigpipe_ignored() -> bool {
    SIGPIPE_IGNORED.load(Ordering::Relaxed)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn sigchld_ignored() -> bool {
    SIGCHLD_IGNORED.load(Ordering::Relaxed)
}
