#![no_main]

//! Every Path mark equals the equivalent lookup on a fully materialized tree.

use libfuzzer_sys::fuzz_target;
use structury::{Answer, Demand, Step, Strictness};
use structury_json::{JsonInput, ScanRequest, scan};
use structury_json_fuzz::{MAX_INPUT, value};

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let whole = Demand::Whole;
    let req = ScanRequest::new(JsonInput::Text, core::slice::from_ref(&whole)).with_strictness(Strictness::Strict);
    let Ok(result) = scan(data, &req) else {
        return;
    };
    let Ok(tree) = value(&result.answers[0]) else {
        return;
    };
    let path = Demand::path(vec![Step::Index(0)]);
    let preq = ScanRequest::new(JsonInput::Text, core::slice::from_ref(&path));
    let Ok(pm) = scan(data, &preq) else {
        return;
    };
    match &pm.answers[0] {
        Answer::Missing | Answer::TypeMismatch { .. } => {
            assert!(tree.element(0).is_none());
        }
        mark => {
            let got = value(mark);
            if let Ok(v) = got {
                assert_eq!(Some(v), tree.element(0).cloned());
            }
        }
    }
});
