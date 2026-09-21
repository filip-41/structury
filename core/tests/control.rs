//! The host control handle: cancellation, used bytes, and clock fields.

use core::sync::atomic::Ordering;

use structury::Control;

fn zero() -> u64 {
    0
}

#[test]
fn new_starts_clear_and_cancel_latches() {
    let control = Control::new(4096, Some(10), zero);
    assert_eq!(control.stop.load(Ordering::Relaxed), 0);
    assert_eq!(control.used.load(Ordering::Relaxed), 0);
    assert_eq!(control.ceiling, 4096);
    assert_eq!(control.deadline, Some(10));
    assert_eq!((control.now)(), 0);

    control.cancel();
    assert_ne!(control.stop.load(Ordering::Relaxed), 0, "cancel latches the stop");
    control.stop.store(0, Ordering::Relaxed);

    let unlimited = Control::new(u64::MAX, None, zero);
    assert_eq!(unlimited.ceiling, u64::MAX);
    assert_eq!(unlimited.deadline, None);
}

#[test]
fn used_accumulates_and_cancel_repeats_safely() {
    let control = Control::new(u64::MAX, None, zero);
    control.used.fetch_add(512, Ordering::Relaxed);
    control.used.fetch_add(1024, Ordering::Relaxed);
    assert_eq!(control.used.load(Ordering::Relaxed), 1536);

    control.cancel();
    control.cancel();
    assert_ne!(control.stop.load(Ordering::Relaxed), 0, "cancel stays latched");

    let clocked = Control::new(1, Some(0), || 42);
    assert_eq!(clocked.ceiling, 1);
    assert_eq!(clocked.deadline, Some(0));
    assert_eq!((clocked.now)(), 42);
}
