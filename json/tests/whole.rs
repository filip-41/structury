//! Whole + Lazy holds a span; Strict fully validates; materializing a Whole
//! answer survives pathological shapes.

mod common;

use crate::common::*;

use structury::{Answer, Demand, Strictness};
use structury_json::{JsonInput, ScanRequest, scan};

#[test]
fn whole_lazy_holds_span() {
    let src = br#"{"a":[1,2,3]}"#;
    let demand = Demand::Whole;
    let req = common::req_with(
        JsonInput::Text,
        core::slice::from_ref(&demand),
        Strictness::Lazy,
        structury_json::Dialect::Rfc8259,
    );
    let result = scan(src, &req).expect("scan");
    assert!(matches!(result.answers[0], Answer::Document(_)));
    let v = common::mat_value(&result.answers[0]).expect("mat");
    assert!(matches!(v, structury::Value::Object(_)));
}

#[test]
fn whole_strict_rejects_leading_zero() {
    let demand = Demand::Whole;
    let req = common::req_with(
        JsonInput::Text,
        core::slice::from_ref(&demand),
        Strictness::Strict,
        structury_json::Dialect::Rfc8259,
    );
    assert!(scan(br#"{"a":01}"#, &req).is_err());
}

/// Arena sizing must not panic or corrupt an answer on pathological shapes.
#[test]
fn arena_sizing_survives_pathological_shapes() {
    // Huge flat array.
    let mut flat = Vec::new();
    flat.push(b'[');
    for i in 0..300_000u32 {
        if i > 0 {
            flat.push(b',');
        }
        flat.extend_from_slice(b"0");
    }
    flat.push(b']');
    // Deep nesting to the bound.
    let depth = structury_json::MAX_NESTING as usize - 1;
    let mut deep = vec![b'['; depth];
    deep.extend_from_slice(&vec![b']'; depth]);
    // One enormous string.
    let huge = {
        let mut v = b"[\"".to_vec();
        v.extend(core::iter::repeat_n(b'a', 1 << 20));
        v.extend_from_slice(b"\"]");
        v
    };
    // All-escape string.
    let escapes = {
        let mut v = b"[\"".to_vec();
        for _ in 0..50_000 {
            v.extend_from_slice(b"\\u0041\\n\\t\\\\");
        }
        v.extend_from_slice(b"\"]");
        v
    };
    for (name, src) in [("flat", flat), ("deep", deep), ("huge", huge), ("escapes", escapes)] {
        let request = ScanRequest::new(JsonInput::Text, &[Demand::Whole]);
        let answer = scan(&src, &request).unwrap().answers.remove(0);
        let owned = mat_owned(&answer).expect("owned");
        let value = mat_value(&answer).expect("value");
        println!(
            "{name}: len={} owned_root={:?} value_ok={}",
            src.len(),
            owned.root(),
            value != structury::Value::Null
        );
    }
}
