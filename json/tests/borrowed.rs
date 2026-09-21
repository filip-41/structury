//! Differential: the borrowed zero-copy view decodes to the same answers as
//! `materialize` and its owned arena twin, `materialize_tape`.

mod common;

use std::fmt::Write as _;

use structury::{Answer, ByteRange, Document, Value, ValueKind};
use structury_json::Dialect;

use common::{FIXTURES, full_span};

fn borrowed(bytes: &[u8]) -> structury::BorrowedDocument<'_> {
    common::mat_borrowed(&full_span(bytes)).expect("borrowed")
}

fn digest(value: structury::BorrowedValue<'_>) -> (usize, usize) {
    let mut counts = (0, 0);
    digest_into(value, &mut counts.0, &mut counts.1);
    counts
}

fn digest_into(value: structury::BorrowedValue<'_>, strings: &mut usize, bytes: &mut usize) {
    match value.kind() {
        ValueKind::String => {
            *strings += 1;
            *bytes += value.as_bytes().len();
        }
        ValueKind::Array => {
            for element in value.elements() {
                digest_into(element, strings, bytes);
            }
        }
        ValueKind::Object => {
            for member in value.member_values() {
                digest_into(member, strings, bytes);
            }
        }
        _ => {}
    }
}

fn materialize_digest(value: &Value) -> (usize, usize) {
    let mut counts = (0, 0);
    materialize_digest_into(value, &mut counts.0, &mut counts.1);
    counts
}

fn materialize_digest_into(value: &Value, strings: &mut usize, bytes: &mut usize) {
    match value {
        Value::Str(text) => {
            *strings += 1;
            *bytes += text.len();
        }
        Value::Array(items) => {
            for item in items {
                materialize_digest_into(item, strings, bytes);
            }
        }
        Value::Object(members) => {
            for (_, member) in members {
                materialize_digest_into(member, strings, bytes);
            }
        }
        _ => {}
    }
}

#[test]
fn borrowed_matches_materialize_and_the_owned_tape() {
    for src in FIXTURES {
        let value = common::parsed(src).expect("value");
        let document = borrowed(src);
        assert_eq!(document.to_value(), value, "value src={}", String::from_utf8_lossy(src));
        assert_eq!(
            document.to_value(),
            common::mat_owned(&full_span(src)).expect("tape").to_value(),
            "owned twin src={}",
            String::from_utf8_lossy(src)
        );
        assert_eq!(
            digest(document.root()),
            materialize_digest(&value),
            "string digest src={}",
            String::from_utf8_lossy(src)
        );
    }
}

#[test]
fn duplicate_keys_are_last_wins() {
    let src = br#"{"a":1,"a":2,"b":0,"a":3}"#;
    let document = borrowed(src);
    let root = document.root();
    assert_eq!(root.len(), 2);
    let a = root.member("a").expect("a");
    assert_eq!(a.number(), "3");
    let names: Vec<&str> = root.members().map(|(name, _)| name).collect();
    assert_eq!(names, vec!["a", "b"]);
}

#[test]
fn escaped_strings_decode_to_the_same_bytes() {
    let src = br#"{"k":"\u0041\u00e9\n\u20ac\ud83d\ude00\\\"","\u0042":"v"}"#;
    let value = common::parsed(src).expect("value");
    let document = borrowed(src);
    assert_eq!(document.to_value(), value);
    let root = document.root();
    assert_eq!(root.member("k").expect("k").str(), "Aé\n€😀\\\"");
    assert_eq!(root.member("B").expect("B").str(), "v");
    // Only the decoded escaped text spills: the 13-byte value and the 1-byte
    // `\u0042` key; the plain `"v"` value stays a source slice.
    assert_eq!(document.spilled_bytes(), 14, "escaped text is the only spill");
}

#[test]
fn a_plain_document_borrows_every_payload() {
    let src = br#"{"users":[{"id":1,"name":"cafe","tags":["x","yy"]}]}"#;
    let document = borrowed(src);
    assert_eq!(document.spilled_bytes(), 0, "no payload copied");
    // Payload slices point into the source.
    let start = document.source().as_ptr() as usize;
    let end = start + document.source().len();
    let name = document
        .root()
        .member("users")
        .expect("users")
        .element(0)
        .expect("first");
    let at = name.member("name").expect("name").as_bytes().as_ptr() as usize;
    assert!((start..end).contains(&at));
}

#[test]
fn json5_borrowed_matches_materialize() {
    let src = br"{a:0x1F,b:'x\u00e9',c:.5,d:+2,e:-0,f:Infinity,g:NaN}";
    let span = ByteRange::try_new(0, src.len()).expect("ordered");
    let document = common::mat_borrowed_dialect(&Answer::Document(Document::from_span(src, span)), Dialect::Json5)
        .expect("borrowed");
    assert_eq!(document.to_value(), parse_dialect_value(src));
    let root = document.root();
    assert_eq!(root.member("a").expect("a").number(), "31");
    assert_eq!(root.member("d").expect("d").number(), "2");
    assert_eq!(root.member("f").expect("f").number(), "Infinity");
    assert_eq!(root.member("g").expect("g").number(), "NaN");
    assert_eq!(
        document.to_value(),
        common::mat_owned_dialect(&Answer::Document(Document::from_span(src, span)), Dialect::Json5)
            .expect("tape")
            .to_value()
    );
}

fn parse_dialect_value(src: &[u8]) -> Value {
    common::parsed_dialect(src, Dialect::Json5).expect("value")
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
    let document = borrowed(bytes);
    assert_eq!(document.spilled_bytes(), 0, "keys and numbers stay borrowed");
    assert_eq!(document.to_value(), common::parsed(bytes).expect("value"));
    let root = document.root();
    assert_eq!(root.len(), 300);
    assert_eq!(root.member("key-0").expect("key-0").number(), "999");
}

#[test]
fn navigation_matches_the_owned_view() {
    let src = br#"{"u":[{"id":1,"score":-2.5},{"id":2,"score":3}],"n":null}"#;
    let borrowed = borrowed(src);
    let tape = common::mat_owned(&full_span(src)).expect("tape");
    for key in ["u", "n", "missing"] {
        let a = borrowed.root().member(key).map(structury::BorrowedValue::kind);
        let b = tape.root().member(key).map(structury::OwnedValue::kind);
        assert_eq!(a, b, "get({key})");
    }
    let a = borrowed.root().member("u").expect("u");
    let b = tape.root().member("u").expect("u");
    assert_eq!(a.len(), b.len());
    for i in 0..a.len() {
        assert_eq!(
            a.element(i).expect("element").kind(),
            b.element(i).expect("element").kind()
        );
    }
    let first = a.element(0).expect("first");
    assert_eq!(first.member("id").expect("id").to_i64(), Some(1));
    assert_eq!(first.member("score").expect("score").number(), "-2.5");
    assert_eq!(first.member("score").expect("score").to_i64(), None);
    assert_eq!(first.element(0), None, "an object has no elements");
}

#[test]
fn column_batch_reads_in_every_form() {
    let src = b"[1,2]";
    let cell = structury::ColumnCell::Span(ByteRange::try_new(1, 2).expect("ordered"));
    let mut batch = structury::Columns::new(src, vec!["$".into()]);
    batch.push(cell);
    let answer = Answer::Columns(batch);
    let borrowed = common::mat_borrowed(&answer).expect("borrowed columns");
    let owned = common::mat_owned(&answer).expect("owned columns");
    let value = common::mat_value(&answer).expect("value columns");
    assert_eq!(borrowed.to_value(), value);
    assert_eq!(owned.to_value(), value);
}

/// The view and the owned tape must agree on acceptance and on the exact error,
/// not just on the decoded value.
fn assert_parity(src: &[u8], dialect: Dialect) {
    let span = ByteRange::try_new(0, src.len()).expect("ordered");
    let answer = Answer::Document(Document::from_span(src, span));
    match (
        common::mat_borrowed_dialect(&answer, dialect),
        common::mat_owned_dialect(&answer, dialect),
    ) {
        (Ok(view), Ok(tape)) => assert_eq!(view.to_value(), tape.to_value()),
        (Err(a), Err(b)) => assert_eq!(a, b, "error src={:?}", String::from_utf8_lossy(src)),
        (a, b) => panic!(
            "acceptance disagrees src={:?}: borrowed={} tape={}",
            String::from_utf8_lossy(src),
            a.is_ok(),
            b.is_ok()
        ),
    }
}

#[test]
fn selected_span_errors_use_source_offsets() {
    use structury::{ColumnCell, Columns};
    use structury_json::{Form, MaterializeOptions, materialize};

    for (selected, dialect, expected_offset, expected_code) in [
        ("01", Dialect::Rfc8259, 1, "invalid-number"),
        ("[1 2]", Dialect::Rfc8259, 3, "expected-comma"),
        ("/*", Dialect::Jsonc, 0, "unterminated-comment"),
        ("1 /*", Dialect::Jsonc, 2, "unterminated-comment"),
        ("1 true", Dialect::Rfc8259, 2, "trailing-content"),
    ] {
        let source = format!("{{\"x\":{selected}}}");
        let span = ByteRange::try_new(5, 5 + selected.len()).expect("ordered");
        let mut columns = Columns::new(source.as_bytes(), vec!["$".into()]);
        columns.push(ColumnCell::Span(span));
        for answer in [
            Answer::Document(Document::from_span(source.as_bytes(), span)),
            Answer::Columns(columns),
        ] {
            for form in [Form::Borrowed, Form::Owned, Form::Value] {
                let error = materialize(&answer, MaterializeOptions::new(dialect, form)).expect_err("invalid value");
                assert_eq!(error.code(), expected_code, "{selected:?}, {form:?}");
                assert_eq!(error.offset(), 5 + expected_offset, "{selected:?}, {form:?}");
            }
        }
    }
}

#[test]
fn bom_parse_errors_use_source_offsets() {
    use structury_json::{Form, MaterializeOptions, parse};

    for form in [Form::Borrowed, Form::Owned, Form::Value] {
        let error = parse(b"\xef\xbb\xbf01", MaterializeOptions::default().with_form(form)).expect_err("leading zero");
        assert_eq!(error.code(), "invalid-number");
        assert_eq!(error.offset(), 4, "{form:?}");
    }
}

#[test]
fn invalid_inputs_are_refused_identically() {
    for src in [
        &b""[..],
        b"{",
        b"[1,",
        b"{\"a\"}",
        b"{\"a\":}",
        b"[01]",
        b"1.",
        b"1e",
        b"\"\\x\"",
        b"\"\\u12\"",
        b"tru",
        b"nul",
        b"[1,2,]",
        b"{\"a\":1,}",
        b"\xff",
        b"{\"a\":1} x",
        b"[,]",
        b"\"\\ud83d\"",
        b"-",
        b"+1",
    ] {
        assert_parity(src, Dialect::Rfc8259);
    }
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    reason = "the xorshift bytes are intentionally truncated to the chosen width"
)]
fn random_inputs_agree_with_the_owned_tape() {
    // Deterministic xorshift: arbitrary bytes exercise the shared refusals, and
    // JSON-ish fragments exercise the value paths under both dialects.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let rfc = b"{}[]\",:0123456789.eE+-truefalsnul \\\t\n\x7f\xc3\xa9\xff";
    let json5 = b"{}[],:\"'0123456789.eE+-xXINrufalsntnul \\\t\n\xc3\xa9\xff";
    for dialect in [Dialect::Rfc8259, Dialect::Json5] {
        let alphabet: &[u8] = if matches!(dialect, Dialect::Json5) { json5 } else { rfc };
        for _ in 0..20_000 {
            let len = (next() % 48) as usize;
            let mut src = Vec::with_capacity(len);
            for _ in 0..len {
                let pick = (next() % 6) as usize;
                if pick == 0 {
                    src.push(alphabet[(next() as usize) % alphabet.len()]);
                } else {
                    src.push((next() % 256) as u8);
                }
            }
            assert_parity(&src, dialect);
        }
    }
}
