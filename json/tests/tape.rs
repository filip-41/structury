//! Differential: the arena `OwnedDocument` decodes to the same `Value` tree as
//! `materialize`, including duplicate-key last-wins and escape decoding.

mod common;

use std::fmt::Write as _;

use structury::{Answer, ByteRange, ColumnCell, Columns, Demand, Document, Step, Value};
use structury_json::{Dialect, scan};

use common::{FIXTURES, full_span};

fn tape_value(bytes: &[u8]) -> Value {
    common::mat_owned(&full_span(bytes)).expect("tape").to_value()
}

/// Every read form must decode to the same tree: source-backed, owned arena,
/// `materialize`'s on-demand `Value`, and the direct `parse` entry.
#[test]
fn all_forms_agree_on_full_documents() {
    for src in FIXTURES {
        let answer = full_span(src);
        let borrowed = common::mat_borrowed(&answer).expect("borrowed");
        let owned = common::mat_owned(&answer).expect("owned");
        let value = common::mat_value(&answer).expect("value");
        let parsed = common::parsed(src).expect("parse");
        let label = String::from_utf8_lossy(src);
        assert_eq!(borrowed.to_value(), owned.to_value(), "borrowed/owned src={label}");
        assert_eq!(owned.to_value(), value, "owned/value src={label}");
        assert_eq!(value, parsed, "value/parse src={label}");
    }
}

#[test]
fn duplicate_keys_are_last_wins() {
    let src = br#"{"a":1,"a":2,"b":0,"a":3}"#;
    let document = common::mat_owned(&full_span(src)).expect("tape");
    let root = document.root();
    assert_eq!(root.len(), 2);
    let a = root.member("a").expect("a");
    assert_eq!(a.number(), "3");
    let names: Vec<&str> = root.members().map(|(name, _)| name).collect();
    assert_eq!(names, vec!["a", "b"]);
}

#[test]
fn escaped_strings_decode_to_the_same_bytes() {
    let src = br#"{"k":"\u0041\u00e9\n\u20ac\ud83d\ude00\\\""}"#;
    let value = common::parsed(src).expect("value");
    assert_eq!(tape_value(src), value);
    let document = common::mat_owned(&full_span(src)).expect("tape");
    let text = document.root().member("k").expect("k");
    assert_eq!(text.str(), "Aé\n€😀\\\"");
}

#[test]
fn demand_answers_match_between_value_and_tape() {
    let paths: &[Vec<Step>] = &[
        vec![],
        vec![Step::Key("a".into())],
        vec![Step::Key("b".into()), Step::Key("c".into()), Step::Index(-1)],
        vec![Step::Key("users".into()), Step::Index(0), Step::Key("id".into())],
        vec![Step::Index(0)],
        vec![Step::Key("dup".into())],
        vec![Step::Key("nope".into())],
    ];
    for src in FIXTURES {
        for steps in paths {
            let demand = if steps.is_empty() {
                Demand::Whole
            } else {
                Demand::path(steps.clone())
            };
            let req = common::text(core::slice::from_ref(&demand));
            let result = scan(src, &req).expect("scan");
            let answer = &result.answers[0];
            if matches!(answer, Answer::Missing | Answer::TypeMismatch { .. }) {
                continue;
            }
            let value = common::mat_value(answer).expect("value");
            let document = common::mat_owned(answer).expect("tape");
            assert_eq!(
                document.to_value(),
                value,
                "src={} path={steps:?}",
                String::from_utf8_lossy(src)
            );
        }
    }
}

#[test]
fn json5_tape_matches_materialize() {
    let src = br"{a:0x1F,b:'x\u00e9',c:.5,d:+2,e:-0,f:Infinity,g:NaN}";
    let span = ByteRange::try_new(0, src.len()).expect("ordered");
    let document =
        common::mat_owned_dialect(&Answer::Document(Document::from_span(src, span)), Dialect::Json5).expect("tape");
    let value = common::parsed_dialect(src, Dialect::Json5).expect("value");
    let root = document.root();
    assert_eq!(document.to_value(), value);
    assert_eq!(root.member("a").expect("a").number(), "31");
    assert_eq!(root.member("f").expect("f").number(), "Infinity");
    assert_eq!(root.member("g").expect("g").number(), "NaN");
}

#[test]
fn many_unique_keys_roundtrip() {
    let mut src = String::from("{");
    for i in 0..300 {
        if i > 0 {
            src.push(',');
        }
        write!(src, "\"key-{i}\":{i}").expect("write");
    }
    src.push_str(",\"key-0\":999}");
    let bytes = src.as_bytes();
    let value = common::parsed(bytes).expect("value");
    let document = common::mat_owned(&full_span(bytes)).expect("tape");
    let root = document.root();
    assert_eq!(document.to_value(), value);
    assert_eq!(root.len(), 300);
    assert_eq!(root.member("key-0").expect("key-0").number(), "999");
}

#[test]
fn column_batch_matches_materialize() {
    let src = b"[10,20]";
    let span = |start, end| ColumnCell::Span(ByteRange::try_new(start, end).expect("ordered"));
    let cases = [
        (
            vec!["a".to_string(), "b".to_string()],
            vec![span(1, 3), span(4, 6), ColumnCell::Absent, span(1, 3)],
        ),
        (vec!["$".to_string()], vec![span(1, 3), span(4, 6)]),
    ];
    for (fields, cells) in cases {
        let mut batch = Columns::new(src, fields);
        for cell in cells {
            batch.push(cell);
        }
        let answer = Answer::Columns(batch);
        let value = common::mat_value_dialect(&answer, Dialect::Rfc8259).expect("value");
        let document = common::mat_owned_dialect(&answer, Dialect::Rfc8259).expect("tape");
        assert_eq!(document.to_value(), value);
        assert_eq!(document.root().len(), 2);
    }
}
