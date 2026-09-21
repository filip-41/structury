#![no_main]

//! Scan + materialize + encode must not panic; valid RFC input round-trips.

use libfuzzer_sys::fuzz_target;
use structury::{Demand, Strictness};
use structury_json::{EncodeOptions, JsonInput, ScanRequest, Source, encode, scan};
use structury_json_fuzz::{MAX_INPUT, value};

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let demand = Demand::Whole;
    let req = ScanRequest::new(JsonInput::Text, core::slice::from_ref(&demand)).with_strictness(Strictness::Strict);
    let Ok(result) = scan(data, &req) else {
        return;
    };
    let Ok(tree) = value(&result.answers[0]) else {
        return;
    };
    let mut out = Vec::new();
    encode(Source::Value(&tree), &EncodeOptions::compact(), &mut out).expect("parsed RFC value must encode");
    let decoded = scan(&out, &req).expect("encoded RFC value must scan strictly");
    let roundtrip = value(&decoded.answers[0]).expect("encoded value must materialize");
    assert_eq!(roundtrip, tree, "encoding must preserve the parsed value");
});
