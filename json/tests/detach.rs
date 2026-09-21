//! Detach receipt: owning a selection must not retain the source.
//!
//! A one-field selection from a ~28 MB input must copy only the selection's
//! payloads. The counting allocator measures the bytes the `detach` call itself
//! allocates (the input is already resident), and
//! a whole-document detach is the control that proves the metric scales.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use structury::{Answer, ByteRange, Document, Value};

struct Counting;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

#[allow(clippy::cast_possible_truncation)]
fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

// SAFETY: each hook forwards to `System` unchanged; the counters are atomic and
// touch no allocator state.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(as_u64(layout.size()), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(as_u64(new_size), Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The measured bytes and allocation count of `f`.
fn measured<T>(f: impl FnOnce() -> T) -> (T, u64, u64) {
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    let value = f();
    ACTIVE.store(false, Ordering::Relaxed);
    (value, ALLOCS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed))
}

/// ~28 MB of `{"rows":[{"id":..,"name":"user-N","pad":".."},..]}`.
fn big_input() -> String {
    let mut src = String::with_capacity(29 << 20);
    src.push_str("{\"rows\":[");
    for i in 0..240_000u32 {
        if i > 0 {
            src.push(',');
        }
        src.push_str("{\"id\":");
        src.push_str(&i.to_string());
        src.push_str(",\"name\":\"user-");
        src.push_str(&i.to_string());
        src.push_str("\",\"pad\":\"");
        for _ in 0..5 {
            src.push_str("0123456789abcdef");
        }
        src.push_str("\"}");
    }
    src.push_str("]}");
    src
}

#[test]
fn detach_copies_only_the_selection() {
    let src = big_input();
    assert!(src.len() >= 28 << 20, "fixture is {} B", src.len());
    let span = ByteRange::try_new(0, src.len()).expect("ordered");
    let answer = Answer::Document(Document::from_span(src.as_bytes(), span));
    let document = common::mat_borrowed(&answer).expect("borrowed");

    // One field from one row: the detached arena must be the selection's bytes.
    let (name, allocs, bytes) = measured(|| {
        document
            .root()
            .member("rows")
            .expect("rows")
            .element(0)
            .expect("row")
            .member("name")
            .expect("name")
            .detach()
    });
    assert!(allocs <= 8, "one-field detach allocated {allocs} times");
    assert!(bytes <= 256, "one-field detach allocated {bytes} B");
    assert_eq!(name.to_value(), Value::Str("user-0".into()));

    // Control: the whole document detach must scale with the document.
    let (whole, _, whole_bytes) = measured(|| document.detach());
    assert!(whole_bytes > 10 << 20, "whole detach allocated {whole_bytes} B");

    // The detached arenas own their payloads: dropping the source and the
    // borrowed view leaves them readable.
    let name_value = name.to_value();
    let whole_value = whole.to_value();
    drop(document);
    drop(src);
    assert_eq!(name_value, Value::Str("user-0".into()));
    assert_eq!(document_value_len(&whole_value), 240_000);
}

/// Row count, read back after the source is gone.
fn document_value_len(value: &Value) -> usize {
    let Value::Object(members) = value else {
        panic!("object");
    };
    let (_, rows) = members.iter().find(|(key, _)| key.as_str() == "rows").expect("rows");
    let Value::Array(rows) = rows else { panic!("array") };
    rows.len()
}
