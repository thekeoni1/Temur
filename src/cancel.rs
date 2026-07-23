//! Cooperative cancellation token (T6). A cloneable flag the render thread
//! sets and the blocking agent/provider stack polls at its natural pause
//! points (SSE frame boundaries, retry backoff slices, tool wait loops).
//! Purely cooperative: a fully stalled TCP read cannot observe it — the
//! double-Ctrl+C force-quit remains the escape hatch for that case.
//!
//! F4: `is_set` also ORs in the plain-REPL SIGINT flag
//! ([`crate::signal::interrupted`]), so a plain-mode Ctrl+C interrupts a
//! turn through exactly the same checkpoints as a TUI Esc; `clear` resets
//! both. One process runs one session, so a process-global flag folding
//! into every clone is sound.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent.
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst) || crate::signal::interrupted()
    }

    /// Reset for the next turn — called at SUBMISSION by the component that
    /// serializes input (F7 invariant on `Session::turn`). Clears both the
    /// token and the plain-REPL SIGINT flag.
    pub fn clear(&self) {
        self.0.store(false, Ordering::SeqCst);
        crate::signal::clear();
    }
}
