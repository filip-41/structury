//! Parallel value-encode composition: chunk, encode serially, stitch.
//!
//! Thread fan-out is the host's job (as with `Drive`); these tests lock the
//! codec-side contract: byte-identity with serial, and the serial fallbacks.

mod common;

use std::fmt::Write as _;

use structury::Value;
use structury_json::{
    EncodeOptions, ItemRange, ValuePlan, encode_value, encode_value_chunk, plan_encode_value, stitch_value_chunks,
};

/// Array of `n` small objects, parsed to an owned value.
fn array_value(n: usize) -> Value {
    let mut src = String::from("[");
    for i in 0..n {
        if i > 0 {
            src.push(',');
        }
        let _ = write!(src, "{{\"id\":{i},\"name\":\"u{i}\"}}");
    }
    src.push(']');
    common::parsed(src.as_bytes()).expect("array parses")
}

/// Serial per-part encode + stitch, the host loop without threads.
fn parallel(value: &Value, opts: EncodeOptions, parts: usize) -> Option<Vec<u8>> {
    let ValuePlan::Chunks(ranges) = plan_encode_value(value, &opts, parts) else {
        return None;
    };
    let mut chunks = Vec::with_capacity(ranges.len());
    for range in &ranges {
        let mut buf = Vec::new();
        encode_value_chunk(value, range, &opts, &mut buf).expect("chunk encodes");
        chunks.push(buf);
    }
    Some(stitch_value_chunks(&chunks, &opts))
}

#[test]
fn compact_and_pretty_match_serial() {
    let value = array_value(3000);
    for opts in [EncodeOptions::compact(), EncodeOptions::pretty()] {
        let serial = encode_value(&value, &opts).expect("serial");
        for parts in [1, 2, 4, 8] {
            assert_eq!(
                parallel(&value, opts, parts).expect("chunks"),
                serial,
                "{opts:?} {parts}"
            );
        }
    }
}

#[test]
fn knobs_match_serial() {
    let value = array_value(3000);
    let opts = EncodeOptions::compact().with_sort_keys(true).with_ascii(true);
    let serial = encode_value(&value, &opts).expect("serial");
    assert_eq!(parallel(&value, opts, 4).expect("chunks"), serial);
}

#[test]
fn serial_fallbacks() {
    let object = common::parsed(br#"{"a":1}"#).expect("object parses");
    assert_eq!(
        plan_encode_value(&object, &EncodeOptions::compact(), 8),
        ValuePlan::Serial
    );
    let one = array_value(1);
    assert_eq!(plan_encode_value(&one, &EncodeOptions::compact(), 8), ValuePlan::Serial);
    let small = array_value(10);
    assert_eq!(
        plan_encode_value(&small, &EncodeOptions::compact(), 8),
        ValuePlan::Serial,
        "below the per-part minimum collapses"
    );
    let framed = EncodeOptions::compact().with_framing(structury_json::ItemFraming::NdjsonLf);
    assert_eq!(plan_encode_value(&array_value(500), &framed, 8), ValuePlan::Serial);
    assert!(parallel(&object, EncodeOptions::compact(), 8).is_none());
}

#[test]
fn chunk_plan_covers_every_item_once() {
    let value = array_value(3000);
    let ValuePlan::Chunks(ranges) = plan_encode_value(&value, &EncodeOptions::compact(), 8) else {
        panic!("3000 items split");
    };
    assert_eq!(
        ranges.first(),
        Some(&ItemRange {
            lo: 0,
            hi: ranges[0].hi
        })
    );
    assert_eq!(ranges.last().map(|range| range.hi), Some(3000));
    for window in ranges.windows(2) {
        assert_eq!(window[0].hi, window[1].lo, "contiguous: {window:?}");
    }
}

#[test]
fn bad_chunks_are_shape_refusals() {
    let value = array_value(10);
    let mut out = Vec::new();
    assert!(encode_value_chunk(&value, &ItemRange { lo: 5, hi: 3 }, &EncodeOptions::compact(), &mut out).is_err());
    assert!(
        encode_value_chunk(
            &value,
            &ItemRange { lo: 0, hi: 11 },
            &EncodeOptions::compact(),
            &mut out
        )
        .is_err()
    );
    let object = common::parsed(br#"{"a":1}"#).expect("object parses");
    assert!(
        encode_value_chunk(
            &object,
            &ItemRange { lo: 0, hi: 1 },
            &EncodeOptions::compact(),
            &mut out
        )
        .is_err()
    );
}

#[test]
fn stitch_skips_empty_chunks() {
    let opts = EncodeOptions::compact();
    assert_eq!(stitch_value_chunks(&[], &opts), b"[]");
    assert_eq!(stitch_value_chunks(&[Vec::new(), b"1".to_vec()], &opts), b"[1]");
    let pretty = EncodeOptions::pretty();
    assert_eq!(stitch_value_chunks(&[], &pretty), b"[]");
}
