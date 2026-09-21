#![no_main]

//! Demands on mismatched kinds yield Missing/TypeMismatch, never panic.

use libfuzzer_sys::fuzz_target;
use structury::{Demand, Oracle, Step};
use structury_json::{JsonInput, ScanRequest, scan};
use structury_json_fuzz::MAX_INPUT;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let demands = [
        Demand::path(vec![Step::Key("nope".into())]),
        Demand::path(vec![Step::Index(0)]),
        Demand::Oracle(Oracle::MemberNames),
        Demand::Collection {
            fields: None,
            nested: None,
        },
    ];
    let req = ScanRequest::new(JsonInput::Text, &demands);
    let _ = scan(data, &req);
});
