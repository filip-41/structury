//! Host control: cancellation, a deadline, and the memory ceiling.
//!
//! The host writes the handle and the codec reads it once per container child or
//! stream record (not per byte; a fused `DescendCount` walk checks once at
//! entry). A stop refuses with a [`ErrorClass::Control`] error; without a handle
//! every poll folds away.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// A host control handle.
///
/// `stop` and `used` may change while a scan runs (they are atomics); `ceiling`,
/// `deadline`, and `now` must be set before it starts, because they are plain
/// fields the codec reads without synchronization. `now` is a function pointer,
/// so a capturing closure cannot be used.
#[derive(Debug)]
pub struct Control {
    /// Cancellation latch: `0` keeps going, any other value stops the request.
    pub stop: AtomicU8,
    /// Bytes the host measured as live for this request.
    pub used: AtomicU64,
    /// Physical-memory ceiling in bytes. [`u64::MAX`] means no ceiling is
    /// reachable while `used < u64::MAX`; a saturated `used` still refuses.
    pub ceiling: u64,
    /// Deadline on the host clock's unit; `None` is no deadline.
    pub deadline: Option<u64>,
    /// The host clock, read against [`deadline`](Self::deadline).
    pub now: fn() -> u64,
}

impl Control {
    /// A handle with `stop` clear and `used` zero.
    #[must_use]
    pub const fn new(ceiling: u64, deadline: Option<u64>, now: fn() -> u64) -> Self {
        Self {
            stop: AtomicU8::new(0),
            used: AtomicU64::new(0),
            ceiling,
            deadline,
            now,
        }
    }

    /// Ask the codec to stop at its next boundary.
    pub fn cancel(&self) {
        self.stop.store(1, Ordering::Relaxed);
    }
}
