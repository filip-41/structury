//! Compact/pretty/verbatim encode and the one `Source` entry.

mod common;

use std::borrow::Cow;

use structury::{Answer, ByteRange, Demand, Fact, FactOwner, FactRole, Strictness};
use structury_json::{
    Dialect, EncodeOptions, Indent, ItemFraming, JsonInput, Source, encode, encode_document, encode_value, scan,
    validate,
};

/// A `Strict` request with one demand.
fn strict_req(demand: &Demand, dialect: Dialect) -> structury_json::ScanRequest<'_> {
    common::req_with(
        JsonInput::Text,
        core::slice::from_ref(demand),
        Strictness::Strict,
        dialect,
    )
}

fn scan_value(bytes: &[u8]) -> structury::Value {
    let demand = Demand::Whole;
    let result = scan(bytes, &strict_req(&demand, structury_json::Dialect::Rfc8259)).expect("scan");
    common::mat_value(&result.answers[0]).expect("materialize")
}

/// A Strict/Whole document for `src`, cloned out of the scan result.
fn strict_document(src: &[u8], dialect: Dialect) -> structury::Document<'_> {
    let demand = Demand::Whole;
    let result = scan(src, &strict_req(&demand, dialect)).expect("scan");
    match &result.answers[0] {
        Answer::Document(doc) => doc.clone(),
        _ => panic!("document"),
    }
}

#[test]
fn compact_encode_roundtrips_fixture_values() {
    for source in common::FIXTURES {
        let value = scan_value(source);
        let mut encoded = Vec::new();
        encode(Source::Value(&value), &EncodeOptions::compact(), &mut encoded).expect("encode parsed value");
        assert_eq!(
            scan_value(&encoded),
            value,
            "source: {:?}",
            String::from_utf8_lossy(source)
        );
    }
}

#[test]
fn compact_encode_keeps_authored_number_spellings() {
    for (src, want) in [
        ("1.50", "1.50"),
        ("-0", "-0"),
        ("1e2", "1e2"),
        ("1E+2", "1E+2"),
        ("1.50e0", "1.50e0"),
        ("0.00123", "0.00123"),
    ] {
        let value = scan_value(src.as_bytes());
        let mut bytes = Vec::new();
        encode(Source::Value(&value), &EncodeOptions::compact(), &mut bytes).expect("encode");
        assert_eq!(bytes, want.as_bytes(), "encode {src}");
    }
}

#[test]
fn encode_refuses_unvalidated_document() {
    let demand = Demand::Whole;
    let req = common::req_with(
        JsonInput::Text,
        core::slice::from_ref(&demand),
        Strictness::Lazy,
        structury_json::Dialect::Rfc8259,
    );
    let src = br#"{"a":01}"#;
    let result = scan(src, &req).expect("Lazy Whole locates structure");
    let Answer::Document(doc) = &result.answers[0] else {
        panic!("document");
    };
    let mut out = Vec::new();
    assert!(encode(Source::Document(doc), &EncodeOptions::compact(), &mut out).is_err());
}

#[test]
fn encode_refuses_structural_whole_document() {
    let demand = Demand::Whole;
    let req = common::req_with(
        JsonInput::Text,
        core::slice::from_ref(&demand),
        Strictness::Structural,
        structury_json::Dialect::Rfc8259,
    );
    let src = br#"{"a":1}"#;
    let result = scan(src, &req).expect("scan");
    let Answer::Document(doc) = &result.answers[0] else {
        panic!("document");
    };
    let mut out = Vec::new();
    assert!(
        encode(Source::Document(doc), &EncodeOptions::compact(), &mut out).is_err(),
        "write gate is Strict-only, not Structural Whole"
    );
}

#[test]
fn encode_document_verbatim_when_validated() {
    let src = br#"{"a":1}"#;
    let doc = strict_document(src, Dialect::Rfc8259);
    let mut out = Vec::new();
    encode(Source::Document(&doc), &EncodeOptions::compact(), &mut out).expect("encode");
    assert_eq!(out, src);
}

#[test]
fn pretty_document_matches_value_pretty() {
    let src = br#"{"a":[1,2],"b":"x"}"#;
    let v = scan_value(src);
    let mut from_value = Vec::new();
    encode(Source::Value(&v), &EncodeOptions::pretty(), &mut from_value).expect("encode");

    let doc = strict_document(src, Dialect::Rfc8259);
    let mut from_doc = Vec::new();
    encode(Source::Document(&doc), &EncodeOptions::pretty(), &mut from_doc).expect("pretty");
    assert_eq!(from_doc, from_value);
}

#[test]
fn encode_applies_stream_framing_and_tab_indent() {
    let v = common::parsed(br#"{"a":1}"#).expect("parse");

    let mut ndjson = EncodeOptions::compact();
    ndjson.framing = ItemFraming::NdjsonLf;
    let mut out = Vec::new();
    encode(Source::Value(&v), &ndjson, &mut out).expect("encode");
    assert_eq!(out, b"{\"a\":1}\n");

    let mut seq = EncodeOptions::compact();
    seq.framing = ItemFraming::JsonSeq;
    let mut out = Vec::new();
    encode(Source::Value(&v), &seq, &mut out).expect("encode");
    assert_eq!(out, b"\x1e{\"a\":1}\n");

    let mut tab = EncodeOptions::pretty();
    tab.indent = Indent::Tab;
    let mut out = Vec::new();
    encode(Source::Value(&v), &tab, &mut out).expect("encode");
    assert_eq!(out, b"{\n\t\"a\": 1\n}");
}

#[test]
fn compact_document_keeps_decimal_signed_zero_and_object() {
    for src in [b"1.50".as_slice(), b"-0", br#"{"a":-0}"#] {
        let doc = strict_document(src, Dialect::Rfc8259);
        let mut out = Vec::new();
        encode(Source::Document(&doc), &EncodeOptions::compact(), &mut out).expect("encode");
        assert_eq!(out, src);
    }
}

#[test]
fn canonical_document_walk_drops_comments_and_keeps_json5() {
    let doc = strict_document(b"{ /* c */ \"a\": 1 }", Dialect::Jsonc);
    let canonical = EncodeOptions::compact()
        .with_dialect(Dialect::Jsonc)
        .with_verbatim(false);
    let mut out = Vec::new();
    encode(Source::Document(&doc), &canonical, &mut out).expect("jsonc");
    assert_eq!(out, br#"{"a":1}"#);
    assert!(
        validate(b"{ /* c */ \"a\": 1 }", Dialect::Rfc8259).is_err(),
        "rfc refuses a comment"
    );
    let doc = strict_document(br#"{"a": 0x1F}"#, Dialect::Json5);
    let canonical = EncodeOptions::compact()
        .with_dialect(Dialect::Json5)
        .with_verbatim(false);
    out.clear();
    encode(Source::Document(&doc), &canonical, &mut out).expect("json5");
    assert_eq!(out, br#"{"a":0x1F}"#, "canonical walk keeps the JSON5 spelling");
}

#[test]
fn pretty_document_emits_jsonc_comments_once() {
    let src = br#"{"a":1, /* c */ "b":2}"#;
    let demand = Demand::Whole;
    let mut req = common::req_with(
        JsonInput::Text,
        core::slice::from_ref(&demand),
        Strictness::Strict,
        Dialect::Jsonc,
    );
    req.facts = true;
    let result = scan(src, &req).expect("scan");
    let Answer::Document(doc) = &result.answers[0] else {
        panic!("document");
    };
    let opts = EncodeOptions::pretty().with_dialect(Dialect::Jsonc);
    let mut out = Vec::new();
    encode(Source::Document(doc), &opts, &mut out).expect("pretty");
    validate(&out, Dialect::Jsonc).expect("output valid jsonc");
    let text = core::str::from_utf8(&out).expect("utf8");
    assert_eq!(
        text.matches("/* c */").count(),
        1,
        "dual records emit one comment: {text}"
    );
}

#[test]
fn encode_writes_canonical_comment_without_a_glyph() {
    let src = b"[1, 2]";
    let root = ByteRange::try_new(0, src.len()).expect("ordered");
    // A query scan keeps facts without glyphs; the encoder falls back to `//`.
    let glyphless = Fact::new(
        FactRole::CommentLead,
        Cow::Borrowed(" c "),
        None,
        FactOwner::node(ByteRange::try_new(4, 5).expect("ordered")),
    );
    let mut doc = structury::Document::from_span_validated(src, root, Dialect::Rfc8259.grammar_tag());
    doc.set_facts(vec![glyphless]).expect("one ordered fact");
    let mut out = Vec::new();
    encode(Source::Document(&doc), &EncodeOptions::pretty(), &mut out).expect("pretty");
    validate(&out, Dialect::Jsonc).expect("output valid jsonc");
    assert!(core::str::from_utf8(&out).expect("utf8").contains("// c"), "{out:?}");
}

#[test]
fn verbatim_write_requires_the_validating_dialect() {
    let doc = strict_document(b"{/* c */\"a\":1}", Dialect::Jsonc);
    assert_eq!(doc.grammar(), Dialect::Jsonc.grammar_tag());

    let mut out = Vec::new();
    let error = encode(Source::Document(&doc), &EncodeOptions::compact(), &mut out)
        .expect_err("rfc options over a jsonc document");
    assert_eq!(error.class(), structury::ErrorClass::Write);
    assert_eq!(error.code(), "dialect-mismatch");

    let matching = EncodeOptions::compact().with_dialect(Dialect::Jsonc);
    encode(Source::Document(&doc), &matching, &mut out).expect("matching dialect memcpys");
    assert_eq!(out, b"{/* c */\"a\":1}");

    out.clear();
    encode(Source::Document(&doc), &matching.with_verbatim(false), &mut out).expect("canonical rewrite");
    assert_eq!(out, br#"{"a":1}"#);
}

#[test]
fn grammar_tag_matches_the_scan_dialect() {
    for dialect in [Dialect::Rfc8259, Dialect::Jsonc, Dialect::Json5] {
        let doc = strict_document(b"{\"a\":1}", dialect);
        assert_eq!(doc.grammar(), dialect.grammar_tag());
    }
}

#[test]
fn sort_keys_orders_members_stably() {
    let value = scan_value(br#"{"b":1,"a":2}"#);
    let sorted = EncodeOptions::compact().with_sort_keys(true);
    assert_eq!(encode_value(&value, &sorted).expect("sorted"), br#"{"a":2,"b":1}"#);
    assert_eq!(
        encode_value(&value, &EncodeOptions::compact()).expect("authored order"),
        br#"{"b":1,"a":2}"#
    );
}

#[test]
fn ascii_escapes_non_ascii_with_surrogate_pairs() {
    let value = scan_value("\"caf\\u00e9 \\ud83d\\ude00\"".as_bytes());
    let ascii = EncodeOptions::compact().with_ascii(true);
    assert_eq!(
        encode_value(&value, &ascii).expect("ascii"),
        br#""caf\u00e9 \ud83d\ude00""#
    );
    assert_eq!(
        encode_value(&value, &EncodeOptions::compact()).expect("utf8"),
        "\"café 😀\"".as_bytes()
    );
}

#[test]
fn value_nesting_past_the_bound_is_refused() {
    let mut value = structury::Value::Null;
    for _ in 0..300 {
        value = structury::Value::Array(vec![value]);
    }
    let error = encode_value(&value, &EncodeOptions::compact()).expect_err("nesting bound");
    assert_eq!(error.class(), structury::ErrorClass::Limit);
    assert_eq!(error.code(), "nesting");
}

#[test]
fn encode_wrappers_match_the_source_entry() {
    let value = scan_value(br#"{"b":1,"a":[true,null]}"#);
    let mut out = Vec::new();
    encode(Source::Value(&value), &EncodeOptions::compact(), &mut out).expect("encode");
    assert_eq!(
        encode_value(&value, &EncodeOptions::compact()).expect("value wrapper"),
        out
    );

    let doc = strict_document(br#"{"a":1}"#, Dialect::Rfc8259);
    out.clear();
    encode(Source::Document(&doc), &EncodeOptions::compact(), &mut out).expect("encode");
    assert_eq!(
        encode_document(&doc, &EncodeOptions::compact()).expect("document wrapper"),
        out
    );
}
