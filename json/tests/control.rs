//! The memory controls, the host-hitch-to-serial fallback, plus the control stop
//! that unwinds an open element loop.
//!
//! `cargo test -p structury-json --test control`.

use core::sync::atomic::{AtomicU64, Ordering};

use structury::{Demand, ErrorClass};
use structury_json::{scan, scan_controlled};

mod common;

use common::{benign, big_array, control, fingerprint, mat, nested_projection, project_root, text};

fn demand() -> Demand {
    project_root(&["id", "name"])
}

/// Row 10: a ceiling, deadline or cancel refuses with a `Control` error and no
/// partial answer, while a benign or under-ceiling control is answer-identical.
#[test]
fn scan_controlled_stops_are_control_errors_and_benign_is_identical() {
    let src = big_array(20_000);
    let demand = demand();
    let request = text(core::slice::from_ref(&demand));
    let serial = scan(src.as_bytes(), &request).expect("no ceiling succeeds");
    assert_eq!(serial.answers.len(), 1);

    for (what, ctrl, code) in [
        ("ceiling", control(0, 4096, 1024, None, || 0), "memory-exceeded"),
        ("deadline", control(0, 0, u64::MAX, Some(0), || 1), "deadline-exceeded"),
        ("cancel latch", control(1, 0, u64::MAX, None, || 0), "cancelled"),
    ] {
        let error = scan_controlled(src.as_bytes(), &request, &ctrl).expect_err("must refuse");
        assert_eq!(error.class(), ErrorClass::Control, "{what}");
        assert_eq!(error.code(), code, "{what}");
    }

    for ctrl in [control(0, 512, 1024, None, || 0), benign()] {
        let got = scan_controlled(src.as_bytes(), &request, &ctrl).expect("no stop");
        assert_eq!(fingerprint(&got), fingerprint(&serial));
    }
}

/// Row 10: a ticking clock proves the stop is prompt, not a full scan.
#[test]
fn cancel_stops_promptly_and_leaves_no_answer() {
    static POLLS: AtomicU64 = AtomicU64::new(0);
    fn clock() -> u64 {
        POLLS.fetch_add(1, Ordering::Relaxed)
    }

    let src = big_array(50_000);
    let demand = demand();
    let request = text(core::slice::from_ref(&demand));
    let soon = control(0, 0, u64::MAX, Some(3), clock);
    let error = scan_controlled(src.as_bytes(), &request, &soon).expect_err("deadline must stop");
    assert_eq!(error.class(), ErrorClass::Control);
    let offset = error.offset() as usize;
    assert!(offset < src.len() / 8, "stopped at {offset} of {}", src.len());
}

/// Row 10: the stream door polls per record too.
#[test]
fn a_stream_polls_per_record_too() {
    let whole = Demand::Whole;
    let request = common::stream(core::slice::from_ref(&whole));
    let cancelled = control(1, 0, u64::MAX, None, || 0);
    let error = scan_controlled(b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n", &request, &cancelled)
        .expect_err("cancel must stop the stream");
    assert_eq!(error.class(), ErrorClass::Control);
    assert_eq!(error.code(), "cancelled");
}

/// A control stop that fires while an element loop is open refuses with no
/// partial answer; a benign control is answer-identical.
#[test]
fn controlled_stop_inside_an_open_element_loop_refuses() {
    static POLLS: AtomicU64 = AtomicU64::new(0);
    fn clock() -> u64 {
        POLLS.fetch_add(1, Ordering::Relaxed)
    }

    let mut src = String::from("{\"rows\":[");
    for i in 0..20_000 {
        if i > 0 {
            src.push(',');
        }
        src.push_str("{\"a\":{\"b\":{\"c\":{\"d\":");
        src.push_str(&i.to_string());
        src.push_str("}}}}");
    }
    src.push_str("]}");
    let src = src.into_bytes();
    let demand = nested_projection();
    let request = text(core::slice::from_ref(&demand));

    let soon = control(0, 0, u64::MAX, Some(3), clock);
    let error = scan_controlled(&src, &request, &soon).expect_err("deadline must stop");
    assert_eq!(error.class(), ErrorClass::Control);

    let plain = scan(&src, &request).expect("plain");
    let controlled = scan_controlled(&src, &request, &benign()).expect("benign");
    assert_eq!(
        mat(&plain.answers[0]),
        mat(&controlled.answers[0]),
        "a benign control must not change the answer"
    );
}

/// A fused `Count` oracle walks the container itself, so it must poll the
/// control per counted child rather than once at entry.
#[test]
fn a_fused_count_oracle_polls_control_per_child() {
    static TICKS: AtomicU64 = AtomicU64::new(0);
    fn tick() -> u64 {
        TICKS.fetch_add(1, Ordering::Relaxed) + 1
    }

    TICKS.store(0, Ordering::Relaxed);
    let demand = Demand::Oracle(structury::Oracle::Count);
    let request = text(core::slice::from_ref(&demand));
    // Deadline 2 with a ticking clock: the entry poll sees 1, a per-child poll
    // reaches 2 and stops.
    let ctrl = control(0, 0, u64::MAX, Some(2), tick);
    let error = scan_controlled(b"[1,2,3,4,5,6,7,8]", &request, &ctrl).expect_err("per-child poll must stop");
    assert_eq!(error.class(), ErrorClass::Control);
}
