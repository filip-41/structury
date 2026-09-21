#![no_main]

//! Valid and invalid JSON decode across framings and strictness.

use libfuzzer_sys::fuzz_target;
use structury::Strictness;
use structury_json::JsonInput;
use structury_json_fuzz::{MAX_INPUT, borrow_whole, scan_facts, scan_whole};

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    borrow_whole(data);
    scan_whole(data, JsonInput::Text, Strictness::Strict);
    scan_whole(data, JsonInput::Text, Strictness::Structural);
    scan_whole(data, JsonInput::Text, Strictness::Lazy);
    scan_facts(data);
    let Some((selector, payload)) = data.split_first() else {
        return;
    };
    let input = match selector % 4 {
        0 => JsonInput::Text,
        1 => JsonInput::Adjacent,
        2 => JsonInput::Ndjson,
        _ => JsonInput::JsonSeq,
    };
    scan_whole(payload, input, Strictness::Structural);
});
