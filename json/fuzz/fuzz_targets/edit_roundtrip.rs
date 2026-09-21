#![no_main]

//! Edit splices over arbitrary input: `Set`/`Insert`/`Delete`/`ReplaceMember`/
//! `Clear` under each dialect must not panic, and a successful output must
//! re-scan under Strict.

use libfuzzer_sys::fuzz_target;
use structury::{Demand, Name, Step, Strictness, Value};
use structury_json::{Dialect, Edit, EditOptions, JsonInput, ScanRequest, edit, scan};
use structury_json_fuzz::MAX_INPUT;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let selector = data.first().copied().unwrap_or(0);
    let dialect = match selector % 3 {
        0 => Dialect::Json5,
        1 => Dialect::Jsonc,
        _ => Dialect::Rfc8259,
    };
    let key: Name = "k".into();
    let edits = [
        Edit::Set {
            path: vec![Step::Key(key.clone())],
            value: Value::Str("v".into()),
        },
        Edit::Insert {
            path: vec![Step::Index(0)],
            value: Value::Null,
        },
        Edit::Delete {
            path: vec![Step::Key(key.clone())],
        },
        Edit::ReplaceMember {
            path: vec![Step::Key(key.clone())],
            key: String::from("kk"),
            value: Value::Bool(true),
        },
        Edit::Clear { path: Vec::new() },
    ];
    let opts = EditOptions::default().with_dialect(dialect);
    let Ok(out) = edit(data, &edits, opts) else {
        return;
    };
    let whole = Demand::Whole;
    let req = ScanRequest::new(JsonInput::Text, core::slice::from_ref(&whole))
        .with_strictness(Strictness::Strict)
        .with_dialect(dialect);
    let _ = scan(&out, &req).expect("successful edit must produce strictly valid output");
});
