//! Plain-REPL SIGINT handling (F4, v0.1.1). The TUI's raw mode never
//! generates SIGINT, so this module is installed ONLY in plain mode; TUI
//! Ctrl+C semantics (clear input / quit / double-press force-quit) are
//! untouched.
//!
//! Semantics: the FIRST Ctrl+C sets a process-global interrupt flag that
//! [`crate::cancel::CancelToken::is_set`] ORs in — the running turn lands
//! cooperatively exactly like a TUI Esc. A SECOND Ctrl+C while the flag is
//! still set force-quits with exit code 130 (the flag is cleared at each
//! submission, so the two-press escape hatch re-arms every turn). The
//! handler is async-signal-safe only: one atomic swap, and `_exit` on the
//! second press.
//!
//! `sigaction` is installed WITHOUT `SA_RESTART`, deliberately: a blocked
//! read may then return EINTR instead of silently restarting, and the
//! provider treats a read error with the token set as a graceful stop (F5)
//! — so even a mid-read Ctrl+C can land the turn cleanly instead of
//! waiting for the next frame. (Rust's buffered readers retry EINTR
//! internally, so this is an opportunistic improvement at the raw-read
//! layer, not a guarantee; the cooperative checkpoints remain the primary
//! landing sites, and the second Ctrl+C remains the hard escape hatch for
//! a fully stalled stream.)

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// True once SIGINT arrived and has not been cleared. ORed into every
/// [`crate::cancel::CancelToken`].
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Reset the flag (the plain REPL clears at submission, together with the
/// token — see the F7 invariant on `Session::turn`).
pub fn clear() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}

extern "C" fn on_sigint(_sig: libc::c_int) {
    // Async-signal-safe only: atomic swap; _exit on the second press.
    if INTERRUPTED.swap(true, Ordering::SeqCst) {
        unsafe { libc::_exit(130) };
    }
}

/// Install the SIGINT handler for plain-REPL mode. Call once at startup,
/// only when the plain REPL is the active UI.
pub fn install_plain_repl_handler() -> std::io::Result<()> {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigint as extern "C" fn(libc::c_int) as usize;
        sa.sa_flags = 0; // deliberately NO SA_RESTART (see module docs)
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}
