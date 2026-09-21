//! JSONC comments / trailing commas, the facts surface (including fused facts),
//! and the JSON5 subset.

mod common;

use crate::common::*;

use structury::{Answer, ByteRange, Demand, FactOwner, FactRole, Range, Step, Strictness, Value};
use structury_json::{Dialect, Edit, EditOptions, JsonInput, ScanRequest, edit, scan, validate};

fn req(demands: &[Demand], strictness: Strictness, dialect: Dialect) -> ScanRequest<'_> {
    common::req_with(structury_json::JsonInput::Text, demands, strictness, dialect)
}

fn scan_doc<'src>(src: &'src [u8], request: &ScanRequest<'_>) -> structury::Document<'src> {
    let result = scan(src, request).expect("scan");
    match result.answers.into_iter().next().expect("mark") {
        Answer::Document(d) => d,
        other => panic!("expected document, got {other:?}"),
    }
}

fn doc(src: &[u8], strictness: Strictness, dialect: Dialect) -> structury::Document<'_> {
    let demand = Demand::Whole;
    scan_doc(src, &req(core::slice::from_ref(&demand), strictness, dialect))
}

fn facts_doc(src: &[u8], strictness: Strictness, dialect: Dialect) -> structury::Document<'_> {
    let demand = Demand::Whole;
    let request = req(core::slice::from_ref(&demand), strictness, dialect).with_facts(true);
    scan_doc(src, &request)
}

fn number(v: &Value) -> &str {
    match v {
        Value::Number(n) => n.spelling(),
        other => panic!("expected number, got {other:?}"),
    }
}

#[test]
fn jsonc_comments_are_trivia_everywhere() {
    let src = br#"// leading
{
  "a": /* inline */ 1,
  // between members
  "b": [
    2, // element trailing
    /* block */ 3
  ]
} // trailing
"#;
    validate(src, Dialect::Jsonc).expect("jsonc valid");
    let value = common::parsed_dialect(src, Dialect::Jsonc).expect("parse");
    assert_eq!(
        value.member("a"),
        Some(&Value::Number(structury::Number::parse("1").unwrap()))
    );
    match value.member("b") {
        Some(Value::Array(items)) => assert_eq!(items.len(), 2),
        other => panic!("{other:?}"),
    }
}

#[test]
fn jsonc_trailing_commas_allowed() {
    let src = br#"{"a":[1,2,],"b":{"c":3,},}"#;
    validate(src, Dialect::Jsonc).expect("trailing commas");
    let value = common::parsed_dialect(src, Dialect::Jsonc).expect("parse");
    assert_eq!(
        value.member("a").and_then(|a| a.element(1)),
        Some(&Value::Number(structury::Number::parse("2").unwrap()))
    );
}

#[test]
fn rfc_still_rejects_jsonc_syntax() {
    assert!(validate(br#"{"a":1,}"#, Dialect::Rfc8259).is_err(), "trailing comma");
    assert!(validate(br#"{"a":1}// c"#, Dialect::Rfc8259).is_err(), "comment");
    assert!(validate(b"/* c */ 1", Dialect::Rfc8259).is_err(), "comment");
}

#[test]
fn jsonc_unterminated_block_comment_is_error() {
    let err = validate(b"[1 /* no end", Dialect::Jsonc).expect_err("unterminated");
    assert_eq!(err.code(), "unterminated-comment");
}

#[test]
fn jsonc_lone_slash_is_error() {
    let err = validate(b"[1 / 2]", Dialect::Jsonc).expect_err("lone slash");
    assert_eq!(err.code(), "invalid-comment");
}

#[test]
fn jsonc_comment_is_not_a_value() {
    let err = validate(b"// only a comment\n", Dialect::Jsonc).expect_err("no value");
    assert_eq!(err.code(), "expected-value");
}

#[test]
fn jsonc_comment_markers_inside_strings_are_content() {
    let src = br#"{"url":"http://x/*y*/"}"#;
    validate(src, Dialect::Jsonc).expect("valid");
    let value = common::parsed_dialect(src, Dialect::Jsonc).expect("parse");
    assert_eq!(value.member("url"), Some(&Value::Str("http://x/*y*/".into())));
}

#[test]
fn jsonc_scan_structure_and_values() {
    let src = br#"{/* pre */"keep": 1, "skip": 2}"#;
    let keep = Demand::path(vec![Step::Key("keep".into())]);
    let strict = scan(
        src,
        &req(core::slice::from_ref(&keep), Strictness::Strict, Dialect::Jsonc),
    )
    .expect("strict");
    let v = common::mat_value_dialect(&strict.answers[0], Dialect::Jsonc).expect("materialize");
    assert_eq!(number(&v), "1");

    let lazy = scan(
        src,
        &req(core::slice::from_ref(&keep), Strictness::Lazy, Dialect::Jsonc),
    )
    .expect("lazy");
    assert_eq!(
        common::mat_value_dialect(&lazy.answers[0], Dialect::Jsonc).expect("mat"),
        v
    );

    // Structural Locate skips comments and an undemanded leading-zero number.
    let unread = br#"{"keep":1,/*c*/"bad":01}"#;
    let structural = scan(
        unread,
        &req(core::slice::from_ref(&keep), Strictness::Structural, Dialect::Jsonc),
    );
    assert!(structural.is_ok(), "{structural:?}");
}

#[test]
fn jsonc_edit_set_and_delete() {
    let src = br#"{"a":/* keep */1,"b":[2,3,],"c":4}"#;
    let set = Edit::Set {
        path: vec![Step::Key("b".into()), Step::Index(0)],
        value: Value::Number(structury::Number::parse("9").unwrap()),
    };
    let delete = Edit::Delete {
        path: vec![Step::Key("a".into())],
    };
    let out = edit(src, &[set, delete], EditOptions::new(Dialect::Jsonc)).expect("edit");
    validate(&out, Dialect::Jsonc).expect("output valid");
    let value = common::parsed_dialect(&out, Dialect::Jsonc).expect("parse");
    assert_eq!(
        value.member("b").and_then(|b| b.element(0)),
        Some(&Value::Number(structury::Number::parse("9").unwrap()))
    );
    assert!(value.member("a").is_none(), "a removed");
}

fn span(start: usize, end: usize) -> ByteRange {
    ByteRange::try_new(start, end).expect("ordered")
}

#[test]
fn facts_roles_and_owners() {
    let src = b"// lead\n[ 1/* inner */, 2 ] // trail\n";
    let d = facts_doc(src, Strictness::Strict, Dialect::Jsonc);
    assert!(d.is_fully_validated());
    assert_eq!(d.facts().len(), 4, "root lead, interstitial pair, trailer lead");
    assert_eq!(d.fact_bytes(&d.facts()[0]), b"// lead");

    let array = FactOwner::node(span(8, 27));
    let array_facts: Vec<_> = d.facts().iter().filter(|fact| fact.owner() == array).collect();
    assert_eq!(array_facts.len(), 2, "root's lead and its trailer");
    assert!(array_facts.iter().all(|fact| fact.role() == FactRole::CommentLead));

    // The interstitial comment is two records: a foot of `1` and a lead of `2`.
    let one = FactOwner::node(span(10, 11));
    let one_fact = d.facts().iter().find(|fact| fact.owner() == one).expect("foot");
    assert_eq!(d.facts().iter().filter(|fact| fact.owner() == one).count(), 1);
    assert_eq!(one_fact.role(), FactRole::CommentFoot);
    assert_eq!(d.fact_bytes(one_fact), b"/* inner */");

    let two = FactOwner::node(span(24, 25));
    let two_fact = d.facts().iter().find(|fact| fact.owner() == two).expect("lead");
    assert_eq!(d.facts().iter().filter(|fact| fact.owner() == two).count(), 1);
    assert_eq!(two_fact.role(), FactRole::CommentLead);
    assert_eq!(d.fact_bytes(two_fact), b"/* inner */");
}

#[test]
fn facts_inline_in_empty_container() {
    let d = facts_doc(b"[/* only */]", Strictness::Strict, Dialect::Jsonc);
    assert_eq!(d.facts().len(), 1);
    assert_eq!(d.facts()[0].role(), FactRole::CommentInline);
}

#[test]
fn facts_off_by_default() {
    let rfc = doc(b"[1,2]", Strictness::Strict, Dialect::Rfc8259);
    assert!(rfc.facts().is_empty());
    let jsonc = doc(b"[1, /* c */ 2]", Strictness::Strict, Dialect::Jsonc);
    assert!(jsonc.facts().is_empty(), "facts: false collects nothing");
}

#[test]
fn query_facts_keep_no_glyphs() {
    let d = facts_doc(b"[1, /* c */ 2]", Strictness::Structural, Dialect::Jsonc);
    assert!(!d.facts().is_empty());
    for fact in d.facts() {
        assert!(fact.source_span().is_none(), "query scan keeps canonical spelling only");
    }
}

#[test]
fn json5_single_quoted_strings() {
    let src = br#"{'a':'it\'s',"b":"c\"d"}"#;
    validate(src, Dialect::Json5).expect("valid");
    let value = common::parsed_dialect(src, Dialect::Json5).expect("parse");
    assert_eq!(value.member("a"), Some(&Value::Str("it's".into())));
    assert_eq!(value.member("b"), Some(&Value::Str("c\"d".into())));
}

#[test]
fn json5_unquoted_keys() {
    let src = br"{a:1,$b:2,_c:3,d4:4}";
    validate(src, Dialect::Json5).expect("valid");
    let value = common::parsed_dialect(src, Dialect::Json5).expect("parse");
    assert_eq!(number(value.member("$b").expect("$b")), "2");
    assert_eq!(number(value.member("d4").expect("d4")), "4");

    // A demand path matches a bare identifier key, and a project reads it.
    let path = Demand::path(vec![Step::Key("$b".into())]);
    let result = scan(
        src,
        &req(core::slice::from_ref(&path), Strictness::Strict, Dialect::Json5),
    )
    .expect("scan");
    let got = common::mat_value_dialect(&result.answers[0], Dialect::Json5).expect("materialize");
    assert_eq!(number(&got), "2");
    let project = Demand::Project {
        path: structury::Path::root(),
        fields: vec!["d4".into(), "$b".into()],
    };
    let result = scan(
        src,
        &req(core::slice::from_ref(&project), Strictness::Structural, Dialect::Json5),
    )
    .expect("scan");
    match &result.answers[0] {
        Answer::Columns(batch) => assert_eq!(batch.rows(), 1),
        other => panic!("{other:?}"),
    }
}

#[test]
fn json5_hex_numbers() {
    let src = br"[0x1F, -0X10, +0xff]";
    validate(src, Dialect::Json5).expect("valid");
    match common::parsed_dialect(src, Dialect::Json5).expect("parse") {
        Value::Array(items) => {
            assert_eq!(items[0], Value::Number(structury::Number::parse("31").unwrap()));
            assert_eq!(items[1], Value::Number(structury::Number::parse("-16").unwrap()));
            assert_eq!(items[2], Value::Number(structury::Number::parse("255").unwrap()));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn json5_point_and_sign_numbers() {
    let src = b"[.5, 5., +1, -0.25, 1e+2]";
    validate(src, Dialect::Json5).expect("valid");
    match common::parsed_dialect(src, Dialect::Json5).expect("parse") {
        Value::Array(items) => {
            assert_eq!(number(&items[0]), ".5");
            assert_eq!(number(&items[1]), "5.");
            assert_eq!(number(&items[2]), "1");
            assert_eq!(number(&items[3]), "-0.25");
            assert_eq!(number(&items[4]), "1e+2");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn json5_non_finite_numbers() {
    let src = b"[Infinity, -Infinity, +Infinity, NaN]";
    validate(src, Dialect::Json5).expect("valid");
    match common::parsed_dialect(src, Dialect::Json5).expect("parse") {
        Value::Array(items) => {
            assert_eq!(number(&items[0]), "Infinity");
            assert_eq!(number(&items[1]), "-Infinity");
            assert_eq!(number(&items[2]), "Infinity");
            assert_eq!(number(&items[3]), "NaN");
        }
        other => panic!("{other:?}"),
    }

    // A demand path materializes the non-finite value with its spelling.
    let path = Demand::path(vec![Step::Index(1)]);
    let result = scan(
        src,
        &req(core::slice::from_ref(&path), Strictness::Strict, Dialect::Json5),
    )
    .expect("scan");
    let got = common::mat_value_dialect(&result.answers[0], Dialect::Json5).expect("materialize");
    assert_eq!(number(&got), "-Infinity");
}

#[test]
fn json5_string_escapes() {
    let src = b"[\"\\x41\", '\\x42', \"\\v\", \"\\0\", '\\'', 'a\\\nb']";
    validate(src, Dialect::Json5).expect("valid");
    match common::parsed_dialect(src, Dialect::Json5).expect("parse") {
        Value::Array(items) => {
            assert_eq!(items[0], Value::Str("A".into()));
            assert_eq!(items[1], Value::Str("B".into()));
            assert_eq!(items[2], Value::Str("\u{000b}".into()));
            assert_eq!(items[3], Value::Str("\0".into()));
            assert_eq!(items[4], Value::Str("'".into()));
            assert_eq!(items[5], Value::Str("ab".into()));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn json5_line_continuations() {
    let lf = b"'a\\\nb'";
    let cr = b"'a\\\rb'";
    let crlf = b"'a\\\r\nb'";
    let ls = b"'a\\\xe2\x80\xa8b'";
    let ps = b"'a\\\xe2\x80\xa9b'";
    let dq = b"\"a\\\r\nb\"";
    for src in [&lf[..], &cr[..], &crlf[..], &ls[..], &ps[..], &dq[..]] {
        validate(src, Dialect::Json5).expect("valid");
        assert_eq!(
            common::parsed_dialect(src, Dialect::Json5).expect("parse"),
            Value::Str("ab".into()),
            "{src:?}"
        );
    }
}

#[test]
fn json5_malformed_escapes_are_rejected() {
    assert!(validate(br"['\x4']", Dialect::Json5).is_err(), "short hex");
    assert!(validate(br"['\xzz']", Dialect::Json5).is_err(), "bad hex");
    assert!(validate(br"['\01']", Dialect::Json5).is_err(), "NUL before digit");
}

#[test]
fn json5_non_finite_encode_requires_json5() {
    use structury_json::{EncodeOptions, Source, encode};
    let value = common::parsed_dialect(b"[Infinity, -Infinity, NaN]", Dialect::Json5).expect("parse");
    let mut out = Vec::new();
    encode(
        Source::Value(&value),
        &EncodeOptions::compact().with_dialect(Dialect::Json5),
        &mut out,
    )
    .expect("json5 encode");
    assert_eq!(out, b"[Infinity,-Infinity,NaN]");

    let mut rfc = Vec::new();
    assert!(
        encode(Source::Value(&value), &EncodeOptions::compact(), &mut rfc).is_err(),
        "RFC encode refuses non-finite"
    );
    let mut jsonc = Vec::new();
    assert!(
        encode(
            Source::Value(&value),
            &EncodeOptions::compact().with_dialect(Dialect::Jsonc),
            &mut jsonc
        )
        .is_err(),
        "JSONC encode refuses non-finite"
    );
}

#[test]
fn json5_extensions_are_rejected_by_rfc_and_jsonc() {
    let cases: &[&[u8]] = &[
        br"'\x41'",
        br#""\x41""#,
        br#""\v""#,
        br#""\0""#,
        b"'a\\\nb'",
        b"Infinity",
        b"-Infinity",
        b"+Infinity",
        b"NaN",
        b".5",
        b"5.",
        b"+1",
        b"0x1F",
    ];
    for src in cases {
        assert!(validate(src, Dialect::Json5).is_ok(), "json5 accepts {src:?}");
        assert!(validate(src, Dialect::Rfc8259).is_err(), "rfc rejects {src:?}");
        assert!(validate(src, Dialect::Jsonc).is_err(), "jsonc rejects {src:?}");
    }
}

#[test]
fn json5_scan_materialize_tape_agree() {
    let demand = Demand::Whole;
    for src in [
        b"[.5, 5., 0x1F, +1, Infinity, -Infinity, NaN, 'x\\x41', \"y\\v\", {k:'v'}]".as_slice(),
        br"{a:.5, b:0x10, c:'x', d:[1,2,],}".as_slice(),
    ] {
        let result = scan(
            src,
            &req(core::slice::from_ref(&demand), Strictness::Strict, Dialect::Json5),
        )
        .expect("scan");
        let value = common::mat_value_dialect(&result.answers[0], Dialect::Json5).expect("materialize");
        assert_eq!(value, common::parsed_dialect(src, Dialect::Json5).expect("parse"));
        let document = common::mat_owned_dialect(&result.answers[0], Dialect::Json5).expect("tape");
        assert_eq!(document.to_value(), value);
    }
}

#[test]
fn json5_remaining_gaps_are_rejected() {
    // Leading zeros stay refused (RFC rule kept for JSON5).
    assert!(validate(b"01", Dialect::Json5).is_err());
    // Unicode identifier keys are not implemented.
    assert!(validate("{\u{00e9}:1}".as_bytes(), Dialect::Json5).is_err());
}

#[test]
fn json5_comments_and_trailing_commas() {
    let src = br"// c
{a: [1, 2,], /* tail */}";
    validate(src, Dialect::Json5).expect("valid");
    let value = common::parsed_dialect(src, Dialect::Json5).expect("parse");
    match value.member("a") {
        Some(Value::Array(items)) => assert_eq!(items.len(), 2),
        other => panic!("{other:?}"),
    }
}

fn project_over_rows(src: &[u8], dialect: Dialect, fields: &[&str]) -> Value {
    let demand = Demand::Project {
        path: structury::Path {
            steps: vec![Step::Key("rows".into())],
        },
        fields: fields.iter().map(|field| (*field).into()).collect(),
    };
    let result = scan(
        src,
        &req(core::slice::from_ref(&demand), Strictness::Structural, dialect),
    )
    .expect("scan");
    common::mat_value_dialect(&result.answers[0], dialect).expect("materialize")
}

fn dom_rows_project(src: &[u8], dialect: Dialect, fields: &[&str]) -> Value {
    let tree = common::parsed_dialect(src, dialect).expect("parse");
    let rows = match tree.member("rows") {
        Some(Value::Array(items)) => items,
        other => panic!("rows: {other:?}"),
    };
    Value::Array(
        rows.iter()
            .map(|row| {
                Value::Object(
                    fields
                        .iter()
                        .filter_map(|field| {
                            row.member(field)
                                .map(|value| (structury::CompactStr::from(*field), value.clone()))
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}

/// A rotated JSONC array with comments and trailing commas exercises the
/// content-keyed head index under a commenting dialect (quoted keys only), and
/// the projected rows must equal the parsed DOM.
#[test]
fn jsonc_rotated_rows_project_matches_dom() {
    let src = br#"{"rows":[
        {"id":1,"name":"a","v":{"x":1}},
        /* rot */ {"name":"b","id":2},
        {"id":3,"v":{"x":3},"name":"c",},
        {"name":"d","v":{"x":4},"id":4},
    ]}"#;
    for fields in [&["name"][..], &["id", "v"][..]] {
        assert_eq!(
            project_over_rows(src, Dialect::Jsonc, fields),
            dom_rows_project(src, Dialect::Jsonc, fields),
            "fields={fields:?}"
        );
    }
}

/// JSON5 mixes quoted and bare keys, so a row can fall back to the general key
/// path while a quoted row still rides the head index; the projection must stay
/// byte-identical to the parsed DOM.
#[test]
fn json5_rotated_rows_project_matches_dom() {
    let src = br#"{"rows":[
        {"id":1,name:'a',"v":{x:1}},
        {name:"b",'id':2},
        {id:3,"v":{x:3},name:'c',},
        {'name':"d",v:{x:4},id:4},
    ]}"#;
    for fields in [&["name"][..], &["id", "v"][..]] {
        assert_eq!(
            project_over_rows(src, Dialect::Json5, fields),
            dom_rows_project(src, Dialect::Json5, fields),
            "fields={fields:?}"
        );
    }
}

/// Fused facts over comment trivia: last-before-close, trailing, invalid UTF-8
/// bytes, duplicate owners, and CRLF inside `//`. Whole always answers a Document.
#[test]
fn fused_facts_comment_trivia() {
    for (src, strictness) in [
        (&b"{\"a\":1 /* last */,}"[..], structury::Strictness::Structural),
        (&b"{\"a\":1, /* last */}"[..], structury::Strictness::Structural),
        (&b"{\"a\":1} // tail"[..], structury::Strictness::Structural),
        (
            &b"/* a */{\"a\":/* b */1}/* c */"[..],
            structury::Strictness::Structural,
        ),
        (&b"{\"a\":1} // \xff\xfe bad"[..], structury::Strictness::Structural),
        (
            &b"{\"a\":1 /* first */,\"a\":2 /* second */}"[..],
            structury::Strictness::Strict,
        ),
        (&b"{\r\n // a\r\n \"x\":1\r\n}"[..], structury::Strictness::Structural),
    ] {
        let request = ScanRequest::new(JsonInput::Text, &[Demand::Whole])
            .with_strictness(strictness)
            .with_dialect(Dialect::Jsonc)
            .with_facts(true);
        let result = scan(src, &request).expect("fused facts scan");
        assert!(matches!(result.answers[0], structury::Answer::Document(_)), "{src:?}");
    }
}

/// The fused facts of the *whole* document must not change when a sparse sibling
/// demand is added.
#[test]
fn fused_facts_are_stable_with_a_sibling_demand() {
    let src: &[u8] = b"/* l */{\"id\":1, \"x\": /* m */ 2 /* f */, \"y\":3}// t";
    let doc_facts = |demands: &[Demand]| -> Vec<(String, String, structury::ByteRange)> {
        let request = ScanRequest::new(JsonInput::Text, demands)
            .with_dialect(Dialect::Jsonc)
            .with_facts(true);
        let r = scan(src, &request).unwrap();
        let doc = r
            .answers
            .iter()
            .find_map(|a| match a {
                structury::Answer::Document(d) => Some(d),
                _ => None,
            })
            .expect("a document");
        doc.facts()
            .iter()
            .map(|f| (format!("{:?}", f.role()), f.text().to_owned(), f.owner().span()))
            .collect()
    };
    let alone = doc_facts(&[Demand::Whole]);
    for sibling in [
        project_root(&["id"]),
        path_key("id", None::<Demand>),
        Demand::Oracle(structury::Oracle::Count),
        filter_root(gt("x", "0"), &["x"]),
        Demand::Slice {
            range: Range { start: None, end: None },
            nested: None,
        },
    ] {
        let with = doc_facts(&[Demand::Whole, sibling.clone()]);
        println!("sibling {sibling:?}: {} facts vs {} alone", with.len(), alone.len());
        assert_eq!(alone, with, "a sibling {sibling:?} changed the whole document's facts");
    }
}

#[test]
fn fused_facts_are_order_independent_across_three_demands() {
    let src: &[u8] = b"/* lead */ {\"a\": 1, \"b\": 2} // tail";
    let keyed = path_key("a", None::<Demand>);
    let proj = project_root(&["b"]);

    let run = |demands: &[Demand]| -> Vec<(String, structury::ByteRange)> {
        let request = mk_req(demands, Dialect::Jsonc, Strictness::Structural, true);
        let r = scan(src, &request).unwrap();
        let whole_at = demands.iter().position(|d| matches!(d, Demand::Whole)).unwrap();
        let doc = match &r.answers[whole_at] {
            Answer::Document(d) => d,
            other => panic!("Whole answer is {other:?}"),
        };
        doc.facts()
            .iter()
            .map(|f| (f.text().to_owned(), f.owner().span()))
            .collect()
    };

    let variants: Vec<(&str, Vec<Demand>)> = vec![
        ("whole,keyed,proj", vec![Demand::Whole, keyed.clone(), proj.clone()]),
        ("whole,proj,keyed", vec![Demand::Whole, proj.clone(), keyed.clone()]),
        ("keyed,whole,proj", vec![keyed.clone(), Demand::Whole, proj.clone()]),
        ("keyed,proj,whole", vec![keyed.clone(), proj.clone(), Demand::Whole]),
        ("proj,whole,keyed", vec![proj.clone(), Demand::Whole, keyed.clone()]),
        ("proj,keyed,whole", vec![proj.clone(), keyed.clone(), Demand::Whole]),
        ("whole,keyed,whole", vec![Demand::Whole, keyed.clone(), Demand::Whole]),
        ("keyed,whole,whole", vec![keyed.clone(), Demand::Whole, Demand::Whole]),
        ("whole,whole,keyed", vec![Demand::Whole, Demand::Whole, keyed.clone()]),
    ];
    let mut reference: Option<Vec<(String, structury::ByteRange)>> = None;
    for (name, demands) in &variants {
        let got = run(demands);
        println!("{name:20} {got:?}");
        match &reference {
            None => reference = Some(got),
            Some(want) => assert_eq!(*want, got, "{name}: fused-facts owner depends on demand order"),
        }
    }
}

#[test]
fn fused_facts_with_a_collector_sibling_are_order_independent() {
    let src: &[u8] = b"/* lead */ {\"a\": 1} // tail";
    let keyed = path_key("a", None::<Demand>);
    let run = |demands: &[Demand]| -> Vec<(String, structury::ByteRange)> {
        let request = mk_req(demands, Dialect::Jsonc, Strictness::Structural, true);
        let r = scan(src, &request).unwrap();
        let whole_at = demands.iter().position(|d| matches!(d, Demand::Whole)).unwrap();
        let Answer::Document(d) = &r.answers[whole_at] else {
            panic!("not a doc");
        };
        d.facts()
            .iter()
            .map(|f| (f.text().to_owned(), f.owner().span()))
            .collect()
    };
    let a = run(&[Demand::Whole, keyed.clone()]);
    let b = run(&[keyed.clone(), Demand::Whole]);
    println!("whole-first {a:?}");
    println!("keyed-first {b:?}");
    assert_eq!(a, b, "owner must be the Whole root in both orders");
}

#[test]
fn fused_facts_with_a_spent_whole_sibling_are_order_independent() {
    let src: &[u8] = b"/* lead */ {\"a\": 1} // tail";
    let spent_whole = spent_path(Demand::Whole);
    let keyed = path_key("a", None::<Demand>);
    let run = |demands: &[Demand]| -> Vec<(String, structury::ByteRange)> {
        let request = mk_req(demands, Dialect::Jsonc, Strictness::Structural, true);
        let r = scan(src, &request).unwrap();
        let whole_at = demands.iter().position(|d| matches!(d, Demand::Whole)).unwrap();
        let Answer::Document(d) = &r.answers[whole_at] else {
            panic!("not a doc");
        };
        d.facts()
            .iter()
            .map(|f| (f.text().to_owned(), f.owner().span()))
            .collect()
    };
    let a = run(&[spent_whole.clone(), keyed.clone(), Demand::Whole]);
    let b = run(&[keyed.clone(), Demand::Whole, spent_whole.clone()]);
    println!("spent-whole first {a:?}");
    println!("spent-whole last  {b:?}");
    assert_eq!(a, b, "spent-Path Whole sibling must not change the owner");
}
