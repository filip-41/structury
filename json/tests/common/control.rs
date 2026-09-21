//! Host-control fixtures shared by the control and stack suites.

use core::sync::atomic::{AtomicU8, AtomicU64};

use structury::Control;

pub(crate) fn control(stop: u8, used: u64, ceiling: u64, deadline: Option<u64>, now: fn() -> u64) -> Control {
    Control {
        stop: AtomicU8::new(stop),
        used: AtomicU64::new(used),
        ceiling,
        deadline,
        now,
    }
}

/// A control that never stops.
pub(crate) fn benign() -> Control {
    control(0, 0, u64::MAX, None, || 0)
}
