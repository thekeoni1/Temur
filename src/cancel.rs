//! Cooperative cancellation token (T6). A cloneable flag the render thread
//! sets and the blocking agent/provider stack polls at its natural pause
//! points (SSE frame boundaries, retry backoff slices, tool wait loops).
//! Purely cooperative: a fully stalled TCP read cannot observe it — the
//! double-Ctrl+C force-quit remains the escape hatch for that case.

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
        self.0.load(Ordering::SeqCst)
    }

    /// Reset for the next turn (stale-flag defense: `Session::turn` clears
    /// at entry so a set-after-turn-end Esc cannot cancel a future turn).
    pub fn clear(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
