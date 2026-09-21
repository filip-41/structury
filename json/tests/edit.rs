//! Edit of invalid input fails; valid input is byte-verbatim outside the splice.
//! Fact spans follow the splice; the comment bytes they point at do not move.

mod common;

use common::key;

use structury::{ByteRange, Document, FactOwner, FactRole, Step, Value};
use structury_json::{Dialect, Edit, EditOptions, FactOp, edit, edit_document, validate};

fn spans(doc: &Document<'_>) -> Vec<(usize, usize)> {
    doc.facts()
        .iter()
        .map(|fact| {
            let span = fact.source_span().expect("edit-grade glyph");
            (span.start(), span.end())
        })
        .collect()
}

fn comments(doc: &Document<'_>) -> Vec<Vec<u8>> {
    doc.facts().iter().map(|fact| doc.fact_bytes(fact).to_vec()).collect()
}

fn number(text: &str) -> Value {
    Value::Number(structury::Number::parse(text).expect("number"))
}

#[test]
fn edit_refuses_invalid_input() {
    let src = br#"{"a":01}"#;
    let edits = [Edit::Set {
        path: vec![Step::Key("a".into())],
        value: Value::Bool(true),
    }];
    assert!(edit(src, &edits, EditOptions::default()).is_err());
}

#[test]
fn edit_insert_key_into_a_non_object_is_refused() {
    // A key insert assumes an object parent; the splice cannot be proven well
    // formed, so the written output is validated and refused.
    let edits = [Edit::Insert {
        path: vec![Step::Key("k".into())],
        value: Value::Bool(true),
    }];
    assert!(edit(b"[]", &edits, EditOptions::default()).is_err());
    assert!(edit(b"[1,2]", &edits, EditOptions::default()).is_err());
}

#[test]
fn edit_set_is_verbatim_outside_splice() {
    let src = br#"{"keep":"untouched","a":1}"#;
    let edits = [Edit::Set {
        path: vec![Step::Key("a".into())],
        value: Value::Number(structury::Number::parse("99").expect("99")),
    }];
    let out = edit(src, &edits, EditOptions::default()).expect("edit");
    let text = core::str::from_utf8(&out).expect("utf8");
    assert!(text.contains(r#""keep":"untouched""#), "{text}");
    assert!(text.contains(r#""a":99"#), "{text}");
    assert!(!text.contains(r#""a":1"#), "{text}");
}

#[test]
fn edit_insert_at_zero_prepends() {
    let src = b"[1,2,3]";
    let edits = [Edit::Insert {
        path: vec![Step::Index(0)],
        value: Value::Number(structury::Number::parse("9").expect("9")),
    }];
    let out = edit(src, &edits, EditOptions::default()).expect("insert");
    assert_eq!(out, b"[9,1,2,3]", "Insert at 0 prepends, it does not append");
}

#[test]
fn edit_insert_escapes_object_key() {
    let src = b"{}";
    let name = "a\\t\"\nb";
    let edits = [Edit::Insert {
        path: vec![Step::Key(name.into())],
        value: Value::Bool(true),
    }];
    let out = edit(src, &edits, EditOptions::default()).expect("insert");
    validate(&out, Dialect::Rfc8259).expect("Strict-valid");
    let parsed = common::parsed(&out).expect("parse");
    assert_eq!(parsed.member(name), Some(&Value::Bool(true)));
    assert_eq!(
        parsed.member("a\t\"\nb"),
        None,
        "decoded key equals the inserted name, not a tab-expanded different key"
    );
}

#[test]
fn edit_delete_drops_member() {
    let src = br#"{"a":1,"b":2}"#;
    let edits = [Edit::Delete {
        path: vec![Step::Key("a".into())],
    }];
    let out = edit(src, &edits, EditOptions::default()).expect("edit");
    let text = core::str::from_utf8(&out).expect("utf8");
    assert!(!text.contains(r#""a""#), "{text}");
    assert!(text.contains(r#""b":2"#), "{text}");
}

#[test]
fn edit_delete_key_is_last_wins() {
    let src = br#"{"a":1,"a":2}"#;
    let out = edit(
        src,
        &[Edit::Delete {
            path: vec![Step::Key("a".into())],
        }],
        EditOptions::default(),
    )
    .expect("delete last a");
    assert_eq!(out, br#"{"a":1}"#);
}

#[test]
fn edit_overlapping_sets_are_refused() {
    let src = br#"{"a":1}"#;
    let edits = [
        Edit::Set {
            path: vec![Step::Key("a".into())],
            value: number("2"),
        },
        Edit::Set {
            path: vec![Step::Key("a".into())],
            value: number("999"),
        },
    ];
    assert!(edit(src, &edits, EditOptions::default()).is_err());
}

#[test]
fn edit_delete_of_absent_path_is_a_noop() {
    let src = br#"{"a":1,"b":2}"#;
    let absent_key = [Edit::Delete {
        path: vec![Step::Key("missing".into())],
    }];
    assert_eq!(edit(src, &absent_key, EditOptions::default()).expect("noop"), src);

    let absent_index = [Edit::Delete {
        path: vec![Step::Index(9)],
    }];
    let arr: &[u8] = b"[1,2,3]";
    assert_eq!(edit(arr, &absent_index, EditOptions::default()).expect("noop"), arr);

    // Insert still errors when the parent is absent.
    let insert_absent = [Edit::Insert {
        path: vec![Step::Key("missing".into()), Step::Key("x".into())],
        value: Value::Bool(true),
    }];
    assert!(edit(src, &insert_absent, EditOptions::default()).is_err());
}

#[test]
fn edit_document_shifts_facts_after_a_replacement() {
    let src = b"[1, /* mid */ 2] // tail";
    let edits = [Edit::Set {
        path: vec![Step::Index(0)],
        value: number("99"),
    }];
    let mut out = Vec::new();
    let doc = edit_document(src, &edits, EditOptions::new(Dialect::Jsonc), &mut out).expect("edit");
    assert_eq!(doc.source(), b"[99, /* mid */ 2] // tail", "byte output is unchanged");
    assert_eq!(
        doc.source(),
        edit(src, &edits, EditOptions::new(Dialect::Jsonc))
            .expect("edit")
            .as_slice()
    );
    assert!(doc.is_fully_validated());
    // `/* mid */` is interstitial (a foot of `1` and a lead of `2`), so it is
    // two records that both shift by +1; the trailer is one record.
    assert_eq!(
        spans(&doc),
        vec![(5, 14), (5, 14), (18, 25)],
        "all comments shift by +1"
    );
    assert_eq!(
        comments(&doc),
        vec![b"/* mid */".to_vec(), b"/* mid */".to_vec(), b"// tail".to_vec()]
    );
}

#[test]
fn edit_document_inserts_between_facts() {
    let src = b"/* left */ [ /* mid */ 1 ] /* right */";
    let edits = [Edit::Insert {
        path: vec![Step::Index(0)],
        value: number("2"),
    }];
    let mut out = Vec::new();
    let doc = edit_document(src, &edits, EditOptions::new(Dialect::Jsonc), &mut out).expect("insert");
    assert_eq!(doc.source(), b"/* left */ [2, /* mid */ 1 ] /* right */");
    assert_eq!(
        spans(&doc),
        vec![(0, 10), (15, 24), (29, 40)],
        "facts after the insertion point shift by +2"
    );
    assert_eq!(
        comments(&doc),
        vec![b"/* left */".to_vec(), b"/* mid */".to_vec(), b"/* right */".to_vec()]
    );
}

#[test]
fn edit_document_delete_removes_a_contained_fact() {
    let src = br#"{"a": 1, /* keep */ "c": 3, "b": /* gone */ 2}"#;
    let edits = [Edit::Delete {
        path: vec![Step::Key("b".into())],
    }];
    let mut out = Vec::new();
    let doc = edit_document(src, &edits, EditOptions::new(Dialect::Jsonc), &mut out).expect("delete");
    assert_eq!(doc.source(), br#"{"a": 1, /* keep */ "c": 3}"#);
    // `/* keep */` is interstitial, so it stays as two records; `/* gone */`
    // sits inside the removed member and is dropped with it.
    assert_eq!(
        spans(&doc),
        vec![(9, 19), (9, 19)],
        "only the comment before the splice stays"
    );
    assert_eq!(comments(&doc), vec![b"/* keep */".to_vec(), b"/* keep */".to_vec()]);
}

#[test]
fn edit_document_set_adjacent_to_a_comment_keeps_it() {
    let src = b"[/* left */1/* right */,2]";
    let edits = [Edit::Set {
        path: vec![Step::Index(0)],
        value: number("10"),
    }];
    let mut out = Vec::new();
    let doc = edit_document(src, &edits, EditOptions::new(Dialect::Jsonc), &mut out).expect("set");
    assert_eq!(doc.source(), b"[/* left */10/* right */,2]");
    // `/* right */` is interstitial: both of its records shift by +1.
    assert_eq!(
        spans(&doc),
        vec![(1, 11), (13, 24), (13, 24)],
        "the right comment shifts by +1"
    );
    assert_eq!(
        comments(&doc),
        vec![b"/* left */".to_vec(), b"/* right */".to_vec(), b"/* right */".to_vec()]
    );
}

fn fact_insert(path: Vec<Step>, role: FactRole, text: &str) -> Edit {
    Edit::Fact {
        path,
        op: FactOp::Insert {
            role,
            text: text.into(),
        },
    }
}

fn fact_replace(path: Vec<Step>, role: FactRole, text: &str) -> Edit {
    Edit::Fact {
        path,
        op: FactOp::Replace {
            role,
            text: text.into(),
        },
    }
}

fn fact_clear(path: Vec<Step>, role: FactRole) -> Edit {
    Edit::Fact {
        path,
        op: FactOp::Clear { role },
    }
}

#[test]
fn fact_insert_root_lead_precedes_the_document() {
    let out = edit(
        br#"{"a":1}"#,
        &[fact_insert(Vec::new(), FactRole::CommentLead, "x")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("insert");
    assert_eq!(out, b"// x\n{\"a\":1}");
}

#[test]
fn fact_insert_member_lead_lands_before_the_key() {
    let out = edit(
        br#"{"a":1}"#,
        &[fact_insert(key("a"), FactRole::CommentLead, "x")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("insert");
    assert_eq!(out, b"{// x\n\"a\":1}");
}

#[test]
fn fact_insert_inline_and_foot_land_after_the_value() {
    let inline = edit(
        br#"{"a":1}"#,
        &[fact_insert(key("a"), FactRole::CommentInline, "x")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("inline");
    assert_eq!(inline, b"{\"a\":1 // x\n}");
    let foot = edit(
        br#"{"a":1}"#,
        &[fact_insert(key("a"), FactRole::CommentFoot, "x")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("foot");
    assert_eq!(foot, b"{\"a\":1\n// x\n}");
}

#[test]
fn fact_insert_on_rfc8259_is_a_noop() {
    let src = br#"{"a":1}"#;
    let out = edit(
        src,
        &[fact_insert(Vec::new(), FactRole::CommentLead, "x")],
        EditOptions::default(),
    )
    .expect("noop");
    assert_eq!(out, src);
}

#[test]
fn fact_insert_multiline_lead_uses_canonical_lines() {
    let out = edit(
        br#"{"a":1}"#,
        &[fact_insert(key("a"), FactRole::CommentLead, "one\ntwo")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("insert");
    assert_eq!(out, b"{// one\n// two\n\"a\":1}");
}

#[test]
fn fact_insert_empty_text_is_a_noop() {
    let src = br#"{"a":1}"#;
    let out = edit(
        src,
        &[fact_insert(key("a"), FactRole::CommentLead, "")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("noop");
    assert_eq!(out, src);
}

#[test]
fn fact_replace_same_place_keeps_the_glyph_shape() {
    let src = b"/* hi */ 1";
    let out = edit(
        src,
        &[fact_replace(Vec::new(), FactRole::CommentLead, "yo")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("replace");
    assert_eq!(out, b"/* yo */ 1");
}

#[test]
fn fact_replace_line_keeps_line_shape() {
    let src = b"// hi\n1";
    let out = edit(
        src,
        &[fact_replace(Vec::new(), FactRole::CommentLead, "yo")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("replace");
    assert_eq!(out, b"// yo\n1");
}

#[test]
fn fact_replace_multiline_body_falls_back_to_canonical() {
    let src = b"/* hi */ 1";
    let out = edit(
        src,
        &[fact_replace(Vec::new(), FactRole::CommentLead, "a\nb")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("replace");
    assert_eq!(out, b"// a\n// b\n 1");
}

#[test]
fn fact_clear_drops_the_glyph() {
    let src = b"/* hi */ 1";
    let out = edit(
        src,
        &[fact_clear(Vec::new(), FactRole::CommentLead)],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("clear");
    assert_eq!(out, b" 1");
}

#[test]
fn fact_replace_absent_is_an_error_but_clear_is_a_noop() {
    let src = br#"{"a":1}"#;
    assert!(
        edit(
            src,
            &[fact_replace(key("a"), FactRole::CommentLead, "x")],
            EditOptions::new(Dialect::Jsonc)
        )
        .is_err(),
        "replace needs an existing fact"
    );
    assert_eq!(
        edit(
            src,
            &[fact_clear(key("a"), FactRole::CommentLead)],
            EditOptions::new(Dialect::Jsonc)
        )
        .expect("noop"),
        src
    );
}

#[test]
fn fact_replace_foot_is_a_noop() {
    // A foot comment shares the following node's lead glyph; the lead is the
    // handle, so replacing the foot leaves the bytes alone (next/ semantics).
    let src = b"{\"a\":1, // inter\n\"b\":2}";
    let out = edit(
        src,
        &[fact_replace(key("a"), FactRole::CommentFoot, "x")],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("noop");
    assert_eq!(out, src);
}

#[test]
fn fact_insert_survives_the_splice_remap_in_the_document() {
    let src = b"[/* keep */[1],2]";
    let edits = [fact_insert(vec![Step::Index(1)], FactRole::CommentLead, "new")];
    let mut out = Vec::new();
    let doc = edit_document(src, &edits, EditOptions::new(Dialect::Jsonc), &mut out).expect("insert");
    assert_eq!(doc.source(), b"[/* keep */[1],// new\n2]");
    assert_eq!(
        comments(&doc),
        vec![b"/* keep */".to_vec(), b"// new".to_vec()],
        "{out:?}"
    );
}

#[test]
fn fact_replace_rewrites_the_record_in_the_document() {
    let src = b"[/* hi */ 1, 2]";
    let edits = [fact_replace(vec![Step::Index(0)], FactRole::CommentLead, "yo")];
    let mut out = Vec::new();
    let doc = edit_document(src, &edits, EditOptions::new(Dialect::Jsonc), &mut out).expect("replace");
    assert_eq!(doc.source(), b"[/* yo */ 1, 2]");
    assert_eq!(comments(&doc), vec![b"/* yo */".to_vec()]);
}

#[test]
fn fact_clear_drops_the_record_in_the_document() {
    let src = b"/* hi */ [1]";
    let edits = [fact_clear(Vec::new(), FactRole::CommentLead)];
    let mut out = Vec::new();
    let doc = edit_document(src, &edits, EditOptions::new(Dialect::Jsonc), &mut out).expect("clear");
    assert_eq!(doc.source(), b" [1]");
    assert_eq!(comments(&doc), Vec::<Vec<u8>>::new());
}

#[test]
fn fact_glyphs_follow_multiple_splices() {
    let edits = [
        fact_insert(vec![Step::Index(0)], FactRole::CommentLead, "first"),
        fact_insert(vec![Step::Index(1)], FactRole::CommentLead, "second"),
    ];
    let mut out = Vec::new();
    let doc = edit_document(
        b"[1,2]",
        &edits,
        EditOptions::default().with_dialect(Dialect::Jsonc),
        &mut out,
    )
    .expect("insert comments");
    assert_eq!(doc.source(), b"[// first\n1,// second\n2]");
    assert_eq!(comments(&doc), vec![b"// first".to_vec(), b"// second".to_vec()]);
}

#[test]
fn container_fact_owners_resize_after_child_edits() {
    for (src, replacement, expected) in [
        (
            b"[1] /* tail */".as_slice(),
            Value::Bool(true),
            b"[true] /* tail */".as_slice(),
        ),
        (
            b"[true] /* tail */".as_slice(),
            number("1"),
            b"[1] /* tail */".as_slice(),
        ),
    ] {
        let mut out = Vec::new();
        let doc = edit_document(
            src,
            &[Edit::Set {
                path: vec![Step::Index(0)],
                value: replacement,
            }],
            EditOptions::default().with_dialect(Dialect::Jsonc),
            &mut out,
        )
        .expect("resize child");
        assert_eq!(doc.source(), expected);
        assert_eq!(doc.facts()[0].owner().span(), doc.root());
        assert_eq!(
            doc.facts()
                .iter()
                .filter(|fact| fact.owner() == FactOwner::node(doc.root()))
                .count(),
            1
        );
    }
}

#[test]
fn new_container_fact_owner_resizes_after_child_edit() {
    let mut out = Vec::new();
    let doc = edit_document(
        b"[[1]]",
        &[
            fact_insert(vec![Step::Index(0)], FactRole::CommentLead, "container"),
            Edit::Set {
                path: vec![Step::Index(0), Step::Index(0)],
                value: Value::Bool(true),
            },
        ],
        EditOptions::default().with_dialect(Dialect::Jsonc),
        &mut out,
    )
    .expect("comment and resize child");
    assert_eq!(doc.source(), b"[// container\n[true]]");
    assert_eq!(
        doc.facts()[0].owner().span(),
        ByteRange::try_new(14, 20).expect("ordered")
    );
}

#[test]
fn delete_with_wrong_container_step_is_a_noop() {
    for dialect in [Dialect::Rfc8259, Dialect::Jsonc, Dialect::Json5] {
        for (src, step) in [
            (br#"{"a":1,"b":2}"#.as_slice(), Step::Index(0)),
            (br#"{"a":1,"b":2}"#.as_slice(), Step::Index(-1)),
            (b"[1,2]".as_slice(), Step::Key("a".into())),
            (b"1".as_slice(), Step::Index(0)),
            (b"true".as_slice(), Step::Key("a".into())),
        ] {
            let out = edit(src, &[Edit::Delete { path: vec![step] }], EditOptions::new(dialect))
                .expect("absent path is inert");
            assert_eq!(out, src);
        }
    }
}

#[test]
fn fact_ops_shift_neighbouring_facts() {
    let src = b"[1 /* tail */]";
    let edits = [fact_insert(vec![Step::Index(0)], FactRole::CommentLead, "new")];
    let mut out = Vec::new();
    let doc = edit_document(src, &edits, EditOptions::new(Dialect::Jsonc), &mut out).expect("insert");
    assert_eq!(doc.source(), b"[// new\n1 /* tail */]");
    assert_eq!(comments(&doc), vec![b"// new".to_vec(), b"/* tail */".to_vec()]);
    assert_eq!(spans(&doc), vec![(1, 7), (10, 20)], "the tail comment shifts by +7");
}
