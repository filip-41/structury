//! Scan request surface: demand kinds, arrangements, multi-demand, strictness,
//! nesting limits, windows, and error classes.

mod common;

use crate::common::*;

use std::fmt::Write as _;

use structury::{
    Answer, Demand, ErrorClass, Oracle, OracleAnswer, Path, Predicate, Range, Step, Strictness, Value, stitch,
};
use structury_json::{Dialect, EncodeOptions, JsonInput, Plan, ScanRequest, Source, encode, scan, validate};

fn req(demands: &[Demand], strictness: Strictness) -> ScanRequest<'_> {
    common::req_with(JsonInput::Text, demands, strictness, Dialect::Rfc8259)
}

/// The standard request over a non-text arrangement.
fn input_req(input: JsonInput, demands: &[Demand]) -> ScanRequest<'_> {
    common::req(input, demands, Dialect::Rfc8259)
}
fn marks<'a>(bytes: &'a [u8], demands: &[Demand], strictness: Strictness) -> Vec<Answer<'a>> {
    scan(bytes, &req(demands, strictness)).expect("scan").answers
}

#[test]
fn duplicate_count_is_last_wins_with_or_without_siblings() {
    let src = br#"{"a":1,"\u0061":2,"b":3}"#;
    for strictness in [Strictness::Lazy, Strictness::Structural, Strictness::Strict] {
        for demands in [
            vec![Demand::Oracle(Oracle::Count)],
            vec![Demand::Oracle(Oracle::Count), Demand::Oracle(Oracle::MemberNames)],
        ] {
            let got = marks(src, &demands, strictness);
            assert!(matches!(got[0], Answer::Oracle(OracleAnswer::Count(2))), "{got:?}");
        }
    }
}

#[test]
fn descend_count_counts_every_node() {
    let src = br#"{"a":[1,2,{"b":true}],"c":null}"#;
    let demand = Demand::Oracle(Oracle::DescendCount);
    // The counter runs on both `Check` arms, so every strictness dial agrees.
    for strictness in [Strictness::Lazy, Strictness::Structural, Strictness::Strict] {
        let got = marks(src, core::slice::from_ref(&demand), strictness);
        assert!(
            matches!(got[0], Answer::Oracle(OracleAnswer::Count(7))),
            "root + array + 1 + 2 + object + true + null under {strictness:?}: {:?}",
            got[0]
        );
    }
}

#[test]
fn duplicate_member_replaces_nested_path_and_projection_answers() {
    for strictness in [Strictness::Lazy, Strictness::Structural, Strictness::Strict] {
        let demands = [
            Demand::path(vec![Step::Key("a".into()), Step::Key("b".into())]),
            Demand::Project {
                path: Path {
                    steps: vec![Step::Key("a".into()), Step::Key("b".into())],
                },
                fields: vec!["id".into()],
            },
        ];
        let got = marks(br#"{"a":{"b":[{"id":1}]},"a":{}}"#, &demands, strictness);
        assert!(got.iter().all(|answer| matches!(answer, Answer::Missing)), "{got:?}");
        let got = marks(br#"{"a":{"b":[{"id":1}]},"a":{"b":[{"id":2}]}}"#, &demands, strictness);
        assert_eq!(common::mat_value(&got[1]).unwrap(), common::json_value(r#"[{"id":2}]"#));
    }
}

#[test]
fn stream_slice_selects_records_before_filtering() {
    let text = br#"[{"a":false,"id":0},{"a":true,"id":1},{"a":true,"id":2}]"#;
    let records = [r#"{"a":false,"id":0}"#, r#"{"a":true,"id":1}"#, r#"{"a":true,"id":2}"#];
    for range in [
        Range {
            start: Some(1),
            end: Some(2),
        },
        Range {
            start: Some(-2),
            end: Some(-1),
        },
        Range {
            start: Some(3),
            end: None,
        },
    ] {
        let demands = [Demand::Slice {
            range,
            nested: Some(Box::new(Demand::Filter {
                path: Path::root(),
                predicate: Predicate::Eq {
                    field: "a".into(),
                    value: Value::Bool(true),
                },
                project: vec!["id".into()],
            })),
        }];
        let expected = common::mat_value(&marks(text, &demands, Strictness::Strict)[0]).unwrap();
        for input in [JsonInput::Adjacent, JsonInput::Ndjson, JsonInput::JsonSeq] {
            let src = if input == JsonInput::JsonSeq {
                let mut framed = String::new();
                for row in &records {
                    let _ = writeln!(framed, "\u{1e}{row}");
                }
                framed
            } else {
                records.join("\n")
            };
            let got = scan(src.as_bytes(), &input_req(input, &demands)).unwrap();
            assert_eq!(
                common::mat_value(&got.answers[0]).unwrap(),
                expected,
                "{input:?} {range:?}"
            );
        }
    }
}

#[test]
fn marks_len_equals_demands_len() {
    let demands = [
        Demand::Whole,
        Demand::Oracle(Oracle::Kind),
        Demand::path(vec![Step::Key("a".into())]),
    ];
    let got = marks(br#"{"a":1,"b":2}"#, &demands, Strictness::Structural);
    assert_eq!(got.len(), demands.len());
}

#[test]
fn two_path_demands_match_two_separate_scans() {
    let src = br#"{"a":1,"b":2}"#;
    let a = Demand::path(vec![Step::Key("a".into())]);
    let b = Demand::path(vec![Step::Key("b".into())]);
    let together = marks(src, &[a.clone(), b.clone()], Strictness::Structural);
    let only_a = marks(src, &[a], Strictness::Structural);
    let only_b = marks(src, &[b], Strictness::Structural);
    let ma = common::mat_value(&together[0]).expect("a");
    let mb = common::mat_value(&together[1]).expect("b");
    assert_eq!(ma, common::mat_value(&only_a[0]).expect("a2"));
    assert_eq!(mb, common::mat_value(&only_b[0]).expect("b2"));
}

#[test]
fn whole_structural_value_checks_demanded_children() {
    let src = br#"{"a":01}"#;
    let demand = Demand::Whole;
    assert!(
        scan(src, &req(&[demand], Strictness::Structural)).is_err(),
        "Whole is demanded: Structural must value-check 01"
    );
    let demand = Demand::Whole;
    let ok = scan(br#"{"a":1}"#, &req(&[demand], Strictness::Structural)).expect("valid");
    let Answer::Document(doc) = &ok.answers[0] else {
        panic!("document");
    };
    assert!(
        !doc.is_fully_validated(),
        "write gate stays false unless Strict validated"
    );
    let demands = [Demand::Whole, Demand::Oracle(Oracle::Count)];
    assert!(
        scan(src, &req(&demands, Strictness::Structural)).is_err(),
        "Count must not skip Whole's value check of 01"
    );
}

#[test]
fn structural_accepts_undemanded_sibling_leading_zero() {
    let src = br#"{"keep":1,"bad":01}"#;
    let demand = Demand::path(vec![Step::Key("keep".into())]);
    let got = scan(src, &req(&[demand], Strictness::Structural));
    assert!(got.is_ok(), "Structural skip must not value-check unread 01: {got:?}");
}

#[test]
fn strict_rejects_undemanded_sibling_leading_zero() {
    let src = br#"{"keep":1,"bad":01}"#;
    let demand = Demand::path(vec![Step::Key("keep".into())]);
    let got = scan(src, &req(&[demand], Strictness::Strict));
    assert!(got.is_err(), "Strict must value-check the whole document");
}

/// A `Filter` reads its predicate and projection fields, so Structural must
/// value-check them even on a row the predicate drops, while the row's other
/// members stay unread. A row-span pre-check in front of the member loop must
/// not cross either edge: a `Locate` skip would accept a malformed projected
/// value, and a `Values` skip would refuse a well-formed row whose unread
/// sibling is malformed.
#[test]
fn filter_row_value_checks_only_its_demanded_fields() {
    let filter = || Demand::Filter {
        path: Path::key("users"),
        predicate: Predicate::Eq {
            field: "active".into(),
            value: Value::Bool(true),
        },
        project: vec!["id".into()],
    };
    let projected = br#"{"users":[{"id":01,"active":false}]}"#;
    assert!(
        scan(projected, &req(&[filter()], Strictness::Structural)).is_err(),
        "the projected id is demanded even on a dropped row"
    );
    let unread = br#"{"users":[{"id":1,"active":false,"pad":01}]}"#;
    assert!(
        scan(unread, &req(&[filter()], Strictness::Structural)).is_ok(),
        "an unread sibling stays Locate on a dropped row"
    );
}

#[test]
fn lazy_errors_only_on_materialize_of_bad_demanded_value() {
    let src = br#"{"keep":1,"bad":01}"#;
    let keep = Demand::path(vec![Step::Key("keep".into())]);
    let bad = Demand::path(vec![Step::Key("bad".into())]);
    let result = scan(src, &req(&[keep, bad], Strictness::Lazy)).expect("Lazy scan locates");
    assert_eq!(result.answers.len(), 2);
    assert!(common::mat_value(&result.answers[0]).is_ok());
    assert!(common::mat_value(&result.answers[1]).is_err());
}

#[test]
fn missing_is_missing_not_null() {
    let src = br#"{"a":1}"#;
    let demand = Demand::path(vec![Step::Key("nope".into())]);
    let got = marks(src, &[demand], Strictness::Structural);
    assert!(matches!(got[0], Answer::Missing), "got {:?}", got[0]);
}

#[test]
fn validate_is_strict_whole() {
    assert!(validate(br#"{"a":1}"#, Dialect::Rfc8259).is_ok());
    assert!(validate(br#"{"a":01}"#, Dialect::Rfc8259).is_err());
}

#[test]
fn oracle_count_type_keys_has() {
    let src = br#"{"b":1,"a":2}"#;
    let demands = [
        Demand::Oracle(Oracle::Count),
        Demand::Oracle(Oracle::Kind),
        Demand::Oracle(Oracle::MemberNames),
        Demand::Oracle(Oracle::HasKey { key: "a".into() }),
        Demand::Oracle(Oracle::HasKey { key: "z".into() }),
    ];
    let got = marks(src, &demands, Strictness::Structural);
    assert!(matches!(got[0], Answer::Oracle(OracleAnswer::Count(2))));
    assert!(matches!(
        got[1],
        Answer::Oracle(OracleAnswer::Kind(structury::ValueKind::Object))
    ));
    match &got[2] {
        Answer::Oracle(OracleAnswer::MemberNames(ks)) => assert_eq!(ks, &["b".to_string(), "a".to_string()]),
        other => panic!("{other:?}"),
    }
    assert!(matches!(got[3], Answer::Oracle(OracleAnswer::HasKey(true))));
    assert!(matches!(got[4], Answer::Oracle(OracleAnswer::HasKey(false))));
}

#[test]
fn collection_and_project_and_filter() {
    let src = br#"{"users":[{"id":1,"score":10,"active":true},{"id":2,"score":3,"active":false}]}"#;
    let project = Demand::Project {
        path: structury::Path {
            steps: vec![Step::Key("users".into())],
        },
        fields: vec!["id".into()],
    };
    let filter = Demand::Filter {
        path: structury::Path {
            steps: vec![Step::Key("users".into())],
        },
        predicate: Predicate::Eq {
            field: "active".into(),
            value: Value::Bool(true),
        },
        project: vec!["id".into()],
    };
    let slice = Demand::Path {
        steps: vec![Step::Key("users".into())],
        nested: Some(Box::new(Demand::Slice {
            range: Range {
                start: Some(0),
                end: Some(1),
            },
            nested: None,
        })),
    };
    let got = marks(src, &[project, filter, slice], Strictness::Structural);
    assert_eq!(got.len(), 3);
    match &got[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 2),
        other => panic!("{other:?}"),
    }
    match &got[1] {
        Answer::Columns(c) => assert_eq!(c.rows(), 1),
        other => panic!("{other:?}"),
    }
    match &got[2] {
        Answer::Columns(c) => assert_eq!(c.rows(), 1),
        other => panic!("{other:?}"),
    }
}

#[test]
fn adjacent_ndjson_json_seq() {
    let adjacent = b"1 2 3";
    let req = input_req(JsonInput::Adjacent, &[Demand::Whole]);
    let r = scan(adjacent, &req).expect("adjacent");
    match &r.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 3),
        other => panic!("{other:?}"),
    }

    let nd = b"1\n2\n3\n";
    let req = input_req(JsonInput::Ndjson, &[Demand::Whole]);
    let r = scan(nd, &req).expect("ndjson");
    match &r.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 3),
        other => panic!("{other:?}"),
    }

    let seq = b"\x1e1\n\x1e2\n";
    let req = input_req(JsonInput::JsonSeq, &[Demand::Whole]);
    let r = scan(seq, &req).expect("json-seq");
    match &r.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 2),
        other => panic!("{other:?}"),
    }

    let records = b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
    let collection = Demand::Collection {
        fields: Some(vec!["id".into()]),
        nested: None,
    };
    let req = input_req(JsonInput::Ndjson, core::slice::from_ref(&collection));
    let r = scan(records, &req).expect("ndjson collection");
    match &r.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 3, "Collection on records is a virtual array"),
        other => panic!("{other:?}"),
    }
    let project = Demand::Project {
        path: structury::Path::root(),
        fields: vec!["id".into()],
    };
    let req = input_req(JsonInput::Ndjson, core::slice::from_ref(&project));
    let r = scan(records, &req).expect("ndjson project");
    match &r.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 3, "Project root fields on records is a virtual array"),
        other => panic!("{other:?}"),
    }
    let first = Demand::path(vec![Step::Index(0)]);
    let req = input_req(JsonInput::Ndjson, core::slice::from_ref(&first));
    let r = scan(records, &req).expect("ndjson path index");
    let v = common::mat_value(&r.answers[0]).expect("first record"); // Path [0] is the record, not a column of records
    match v.member("id") {
        Some(Value::Number(n)) => assert_eq!(n.spelling(), "1"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn last_index_path() {
    let src = br#"{"users":[{"id":1},{"id":2}]}"#;
    let demand = Demand::path(vec![Step::Key("users".into()), Step::Index(-1), Step::Key("id".into())]);
    let got = marks(src, &[demand], Strictness::Structural);
    let v = common::mat_value(&got[0]).expect("id");
    match v {
        Value::Number(n) => assert_eq!(n.spelling(), "2"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stream_oracles_are_a_virtual_array() {
    let src = b"{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n";
    let keys_len = Demand::Oracle(Oracle::MemberCount);
    let req = input_req(JsonInput::Ndjson, core::slice::from_ref(&keys_len));
    let r = scan(src, &req).expect("stream keys-length");
    assert!(
        matches!(r.answers[0], Answer::Oracle(OracleAnswer::Count(3))),
        "{:?}",
        r.answers[0]
    );

    let keys = Demand::Oracle(Oracle::MemberNames);
    let req = input_req(JsonInput::Ndjson, core::slice::from_ref(&keys));
    let r = scan(src, &req).expect("stream keys");
    assert!(
        matches!(
            r.answers[0],
            Answer::TypeMismatch {
                actual: structury::ValueKind::Array
            }
        ),
        "{:?}",
        r.answers[0]
    );
}

#[test]
fn stream_slice_applies_nested_demand() {
    let src = b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
    let demand = Demand::Slice {
        range: Range {
            start: Some(0),
            end: Some(2),
        },
        nested: Some(Box::new(Demand::Project {
            path: Path::root(),
            fields: vec!["id".into()],
        })),
    };
    let req = input_req(JsonInput::Ndjson, core::slice::from_ref(&demand));
    let r = scan(src, &req).expect("stream slice");
    match &r.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 2),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stream_compiled_plan_mixes_every_and_index_scoped_demands() {
    // The compiled `StreamPlan` folds index-independent demands into one shared
    // slice and index-scoped demands into per-record extras; two `Index` steps
    // resolving to the same record must group onto that record's walk.
    let src = b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
    let every = Demand::Project {
        path: Path::root(),
        fields: vec!["id".into()],
    };
    let second_id = Demand::path(vec![Step::Index(1), Step::Key("id".into())]);
    let second_missing = Demand::path(vec![Step::Index(-2), Step::Key("ghost".into())]);
    let last_id = Demand::path(vec![Step::Index(-1), Step::Key("id".into())]);
    let demands = [every, second_id, second_missing, last_id];
    let req = input_req(JsonInput::Ndjson, &demands);
    let r = scan(src, &req).expect("mixed stream");
    assert_eq!(r.answers.len(), demands.len());
    match &r.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 3, "every-record projection spans all records"),
        other => panic!("projection: {other:?}"),
    }
    let num = |s: &str| Value::Number(structury::Number::parse(s).expect("number"));
    assert_eq!(
        common::mat_value(&r.answers[1]).expect("second id"),
        num("2"),
        "Index(1) reaches the second record"
    );
    assert!(
        matches!(r.answers[2], Answer::Missing),
        "Index(-2) on the same record still applies its own missing key: {:?}",
        r.answers[2]
    );
    assert_eq!(
        common::mat_value(&r.answers[3]).expect("last id"),
        num("3"),
        "Index(-1) reaches the last record"
    );
}

fn stream_marks<'a>(bytes: &'a [u8], demand: &Demand) -> Vec<Answer<'a>> {
    let req = input_req(JsonInput::Ndjson, core::slice::from_ref(demand));
    scan(bytes, &req).expect("stream scan").answers
}

/// Scan `records` as NDJSON and as the equivalent text array, and require the
/// one demand's materialized answer to be identical. The persistent stream
/// walker reuses one accumulator per demand across records, so a stale span, a
/// retained row or a lost duplicate-key win would diverge here.
fn stream_matches_array(records: &[&[u8]], demand: &Demand) {
    let mut ndjson = Vec::new();
    let mut array = Vec::from(&b"["[..]);
    for (i, record) in records.iter().enumerate() {
        if i > 0 {
            ndjson.push(b'\n');
            array.push(b',');
        }
        ndjson.extend_from_slice(record);
        array.extend_from_slice(record);
    }
    ndjson.push(b'\n');
    array.push(b']');
    let from_stream = stream_marks(&ndjson, demand);
    let from_array = marks(&array, core::slice::from_ref(demand), Strictness::Structural);
    assert_eq!(
        common::mat_value(&from_stream[0]).expect("stream"),
        common::mat_value(&from_array[0]).expect("array"),
    );
}

#[test]
fn stream_projection_matches_equivalent_array() {
    stream_matches_array(
        &[
            br#"{"id":1,"v":{"x":1}}"#,
            br#"{"id":2}"#,
            br#"{"v":{"x":3},"id":4}"#,
            br#"{"id":5,"id":6,"v":null}"#,
            br#"{"id":7,"extra":true}"#,
        ],
        &Demand::Project {
            path: Path::root(),
            fields: vec!["id".into(), "v".into()],
        },
    );
}

/// A non-object record contributes no projection row, but the persistent
/// accumulator must survive it (it is overwritten to a type mismatch and
/// rebuilt) without losing the rows already accumulated.
/// The ruling: a projection is one row per element, and a
/// non-object record is an absent row. This test used to sanction *skip* for the
/// stream/root form; the parity check passed while both forms skipped. It now
/// pins the absolute answer, so the stream and the text array cannot agree on
/// the wrong law.
#[test]
fn stream_projection_counts_a_row_per_non_object_record() {
    let demand = Demand::Project {
        path: Path::root(),
        fields: vec!["id".into()],
    };
    stream_matches_array(&[&b"1"[..], br#"{"id":2}"#, &b"\"x\""[..], br#"{"id":3}"#], &demand);
    let src = b"1\n{\"id\":2}\n\"x\"\n{\"id\":3}\n";
    let result = scan(src, &input_req(JsonInput::Ndjson, core::slice::from_ref(&demand))).expect("scan");
    let Answer::Columns(columns) = &result.answers[0] else {
        panic!("root Project answers columns: {:?}", result.answers[0]);
    };
    assert_eq!(columns.rows(), 4, "one row per record, absent for a non-object");
    let id = |text: &str| {
        Value::Object(vec![(
            "id".into(),
            Value::Number(structury::Number::parse(text).expect("number")),
        )])
    };
    assert_eq!(
        common::mat_value(&result.answers[0]).expect("materialize"),
        Value::Array(vec![
            Value::Object(Vec::new()),
            id("2"),
            Value::Object(Vec::new()),
            id("3"),
        ])
    );
}

/// A mixed stream with one `Whole` and one root `Project` demand: the `Whole`
/// slot folds the per-record documents into a `$` column while the projection
/// accumulates one row per record.
#[test]
fn stream_whole_and_project_fold_together() {
    let src = b"{\"a\":1}\n{\"b\":2}\n";
    let demands = [
        Demand::Whole,
        Demand::Project {
            path: Path::root(),
            fields: vec!["a".into()],
        },
    ];
    let req = input_req(JsonInput::Ndjson, &demands);
    let r = scan(src, &req).expect("mixed stream");
    match &r.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 2, "one `$` row per record"),
        other => panic!("whole slot: {other:?}"),
    }
    match &r.answers[1] {
        Answer::Columns(c) => assert_eq!(c.rows(), 2, "one row per record"),
        other => panic!("project slot: {other:?}"),
    }
}

#[test]
fn stream_filter_matches_equivalent_array() {
    stream_matches_array(
        &[
            br#"{"id":1,"active":true}"#,
            br#"{"id":2,"active":false}"#,
            br#"{"id":3}"#,
            br#"{"id":4,"active":true,"extra":1}"#,
        ],
        &Demand::Filter {
            path: Path::root(),
            predicate: Predicate::Eq {
                field: "active".into(),
                value: Value::Bool(true),
            },
            project: vec!["id".into()],
        },
    );
}

/// A stream is a virtual array: an empty stream answers the empty batch with
/// the demand's fields, exactly as the same demand does on an empty text array —
/// never `Missing`, and never dependent on a sibling demand.
#[test]
fn empty_stream_projection_is_an_empty_batch() {
    let demand = Demand::Project {
        path: Path::root(),
        fields: vec!["id".into()],
    };
    let got = stream_marks(b"", &demand);
    match &got[0] {
        Answer::Columns(columns) => {
            assert_eq!(columns.rows(), 0);
            assert_eq!(columns.fields().len(), 1);
            assert_eq!(columns.fields()[0], "id");
        }
        other => panic!("empty stream must be an empty batch, got {other:?}"),
    }
    assert_eq!(
        common::mat_value(&got[0]).expect("materialize"),
        Value::Array(Vec::new())
    );
}

#[test]
fn collection_nested_path_keeps_every_row() {
    let src = br#"{"rows":[{"a":{"b":1}},{"a":{"b":2}},{"a":{"b":3}}]}"#;
    let demand = Demand::Path {
        steps: vec![Step::Key("rows".into())],
        nested: Some(Box::new(Demand::Collection {
            fields: None,
            nested: Some(Box::new(Demand::Path {
                steps: vec![Step::Key("a".into()), Step::Key("b".into())],
                nested: None,
            })),
        })),
    };
    let got = marks(src, &[demand], Strictness::Structural);
    match &got[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 3, "a nested Path must append per element, not overwrite"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn slice_window_bounds_pick_the_rows() {
    let src = br#"{"rows":[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5}]}"#;
    for (range, want) in [
        (
            Range {
                start: Some(0),
                end: Some(2),
            },
            2,
        ),
        (Range { start: None, end: None }, 5),
        (
            Range {
                start: Some(-2),
                end: None,
            },
            2,
        ),
        (
            Range {
                start: None,
                end: Some(3),
            },
            3,
        ),
    ] {
        let demand = Demand::Path {
            steps: vec![Step::Key("rows".into())],
            nested: Some(Box::new(Demand::Slice {
                range,
                nested: Some(Box::new(Demand::Project {
                    path: Path::root(),
                    fields: vec!["id".into()],
                })),
            })),
        };
        let got = marks(src, &[demand], Strictness::Structural);
        match &got[0] {
            Answer::Columns(c) => assert_eq!(c.rows(), want, "range {range:?}"),
            other => panic!("{range:?} -> {other:?}"),
        }
    }
}

#[test]
fn bounded_slice_stops_before_the_tail() {
    let src = br#"{"rows":[{"id":1},{"id":2},{"id":3},{"id":4}],"tail":}"#;
    let demand = Demand::Path {
        steps: vec![Step::Key("rows".into())],
        nested: Some(Box::new(Demand::Slice {
            range: Range {
                start: Some(0),
                end: Some(2),
            },
            nested: Some(Box::new(Demand::Project {
                path: Path::root(),
                fields: vec!["id".into()],
            })),
        })),
    };
    let got = marks(src, &[demand], Strictness::Structural);
    match &got[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 2, "the window is answered without reading the tail"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn filter_covers_every_predicate_operator() {
    let src = br#"{"rows":[{"id":1,"n":5},{"id":2,"n":10},{"id":3,"n":15}]}"#;
    let num = |text: &str| Value::Number(structury::Number::parse(text).expect("number"));
    let eq = |field: &str, value: Value| Predicate::Eq {
        field: field.into(),
        value,
    };
    let rows = |predicate: Predicate| -> usize {
        let demand = Demand::Filter {
            path: Path {
                steps: vec![Step::Key("rows".into())],
            },
            predicate,
            project: Vec::new(),
        };
        match &marks(src, &[demand], Strictness::Structural)[0] {
            Answer::Columns(c) => c.rows(),
            other => panic!("{other:?}"),
        }
    };

    assert_eq!(
        rows(Predicate::Ne {
            field: "id".into(),
            value: num("2")
        }),
        2
    );
    assert_eq!(
        rows(Predicate::Lt {
            field: "n".into(),
            value: num("10")
        }),
        1
    );
    assert_eq!(
        rows(Predicate::Ge {
            field: "n".into(),
            value: num("10")
        }),
        2
    );
    assert_eq!(
        rows(Predicate::Le {
            field: "n".into(),
            value: num("5")
        }),
        1
    );
    assert_eq!(
        rows(Predicate::And(
            Box::new(eq("id", num("1"))),
            Box::new(Predicate::Lt {
                field: "n".into(),
                value: num("10")
            }),
        )),
        1
    );
    assert_eq!(
        rows(Predicate::Or(
            Box::new(eq("id", num("2"))),
            Box::new(Predicate::Ge {
                field: "n".into(),
                value: num("15")
            }),
        )),
        2
    );
    assert_eq!(rows(Predicate::Not(Box::new(eq("id", num("1"))))), 2);
}

#[test]
fn numeric_filter_orders_signed_zero_and_over_i128_spellings() {
    let src = br#"{"rows":[{"n":0},{"n":-0},{"n":100},{"n":1.5},{"n":170141183460469231731687303715884105728}]}"#;
    let gt = |text: &str| -> usize {
        let demand = Demand::Filter {
            path: Path {
                steps: vec![Step::Key("rows".into())],
            },
            predicate: Predicate::Gt {
                field: "n".into(),
                value: Value::Number(structury::Number::parse(text).expect("number")),
            },
            project: Vec::new(),
        };
        match &marks(src, &[demand], Strictness::Structural)[0] {
            Answer::Columns(c) => c.rows(),
            other => panic!("{other:?}"),
        }
    };
    // `-0` orders below `0`; a magnitude past `i128` still compares exactly.
    assert_eq!(gt("0"), 3, "100, 1.5, and the wide integer are above zero; -0 is not");
    assert_eq!(gt("-0"), 4, "everything but the -0 row is above -0");
    assert_eq!(gt("1.5"), 2, "only the wide integer is above 1.5");
    assert_eq!(
        gt("170141183460469231731687303715884105727"),
        1,
        "only the wider integer is above i128::MAX"
    );
}

#[test]
fn scan_each_visits_text_once_and_streams_per_record() {
    let text_visits = core::cell::Cell::new(0usize);
    structury_json::scan_each(br#"{"a":1}"#, &req(&[Demand::Whole], Strictness::Structural), |mark| {
        assert!(matches!(mark, Answer::Document(_)));
        text_visits.set(text_visits.get() + 1);
    })
    .expect("scan_each text");
    assert_eq!(text_visits.get(), 1);

    let ndjson = input_req(JsonInput::Ndjson, &[Demand::Whole]);
    let mut seen = 0usize;
    structury_json::scan_each(b"1\n2\n3\n", &ndjson, |_| seen += 1).expect("scan_each stream");
    assert_eq!(seen, 3);

    let truncated = input_req(JsonInput::Ndjson, &[Demand::Whole]);
    let mut kept = 0usize;
    structury_json::scan_each(b"{\"id\":1}\n{\"id\":", &truncated, |_| kept += 1).expect("truncated tail recovers");
    assert_eq!(kept, 1);
}

#[test]
fn empty_ndjson_count_is_zero() {
    let demand = Demand::Oracle(Oracle::Count);
    let req = input_req(JsonInput::Ndjson, core::slice::from_ref(&demand));
    for src in [b"".as_slice(), b"\n\n".as_slice()] {
        let got = scan(src, &req).expect("empty stream");
        assert!(
            matches!(got.answers[0], Answer::Oracle(OracleAnswer::Count(0))),
            "{src:?} {:?}",
            got.answers[0]
        );
    }
}

#[test]
fn lazy_count_on_string_locates_without_value_check() {
    let src = br#""\uDC00""#;
    let demand = Demand::Oracle(Oracle::Count);
    assert!(
        scan(src, &req(core::slice::from_ref(&demand), Strictness::Lazy)).is_ok(),
        "Lazy Count locates; unpaired surrogate waits for materialize"
    );
    assert!(scan(src, &req(core::slice::from_ref(&demand), Strictness::Strict)).is_err());
}

#[test]
fn parse_strips_utf8_bom() {
    let mut src = b"\xEF\xBB\xBF".to_vec();
    src.extend_from_slice(br#"{"a":1}"#);
    common::parsed(&src).expect("BOM is stripped on parse as on scan");
}

#[test]
fn filter_gt_zero_keeps_a_fraction() {
    let src = br#"{"rows":[{"n":0.5}]}"#;
    let demand = Demand::Filter {
        path: Path {
            steps: vec![Step::Key("rows".into())],
        },
        predicate: Predicate::Gt {
            field: "n".into(),
            value: Value::Number(structury::Number::parse("0").expect("0")),
        },
        project: Vec::new(),
    };
    match &marks(src, &[demand], Strictness::Structural)[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 1, "0.5 > 0"),
        other => panic!("{other:?}"),
    }
}

/// The span walk compares demanded values without materializing one, so a
/// container-valued `Eq`/`Ne` operand is refused as a shape error instead of
/// silently never matching.
#[test]
fn filter_container_operand_is_a_shape_refusal() {
    let filter = Demand::Filter {
        path: Path::root(),
        predicate: Predicate::Eq {
            field: "a".into(),
            value: Value::Array(vec![Value::Null]),
        },
        project: Vec::new(),
    };
    let error = scan(
        br#"[{"a":[null]}]"#,
        &req(core::slice::from_ref(&filter), Strictness::Structural),
    )
    .expect_err("a container operand cannot be served from spans");
    assert_eq!(error.class(), structury::ErrorClass::Shape);
    assert_eq!(error.code(), "predicate-operand");
    assert!(
        Plan::build(
            br#"[{"a":[null]}]"#,
            &req(core::slice::from_ref(&filter), Strictness::Structural)
        )
        .is_err(),
        "the plan door refuses it too"
    );

    let nested = Demand::Path {
        steps: vec![Step::Key("rows".into())],
        nested: Some(Box::new(Demand::Filter {
            path: Path::root(),
            predicate: Predicate::Ne {
                field: "a".into(),
                value: Value::Object(Vec::new()),
            },
            project: Vec::new(),
        })),
    };
    assert!(
        scan(
            br#"{"rows":[{"a":{}}]}"#,
            &req(core::slice::from_ref(&nested), Strictness::Structural)
        )
        .is_err(),
        "a nested filter is refused too"
    );

    let scalar = Demand::Filter {
        path: Path::root(),
        predicate: Predicate::Eq {
            field: "a".into(),
            value: Value::Bool(true),
        },
        project: Vec::new(),
    };
    assert!(
        scan(
            br#"[{"a":true}]"#,
            &req(core::slice::from_ref(&scalar), Strictness::Structural)
        )
        .is_ok(),
        "scalar operands still serve"
    );
}

#[test]
fn scan_each_with_issues_reports_the_recovered_tail() {
    let req = input_req(JsonInput::Ndjson, &[Demand::Whole]);
    let mut kept = 0usize;
    let issues =
        structury_json::scan_each_with_issues(b"{\"id\":1}\n{\"id\":", &req, |_| kept += 1).expect("recovered tail");
    assert_eq!(kept, 1);
    assert_eq!(issues.len(), 1, "the dropped tail is reported, not silent");
}

#[test]
fn scan_each_with_issues_is_empty_on_clean_input() {
    let req = input_req(JsonInput::Ndjson, &[Demand::Whole]);
    let mut seen = 0usize;
    let issues = structury_json::scan_each_with_issues(b"1\n2\n3\n", &req, |_| seen += 1).expect("clean input");
    assert_eq!(seen, 3);
    assert!(issues.is_empty());
}

fn obs_text_with(src: &[u8], demands: &[Demand], strictness: Strictness) -> String {
    let request = ScanRequest::new(JsonInput::Text, demands).with_strictness(strictness);
    let result = scan(src, &request).expect("scan");
    format!("{:?}", observe_all(&result.answers))
}

#[test]
fn nine_path_demands_into_one_child_all_return() {
    let src = br#"{"child":{"a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7,"h":8,"i":9}}"#;
    let demands: Vec<Demand> = (b'a'..=b'i')
        .map(|k| Demand::path(vec![Step::Key("child".into()), Step::Key(char::from(k).to_string())]))
        .collect();
    let result = scan(src, &req(&demands, Strictness::Structural)).expect("scan");
    assert_eq!(result.answers.len(), 9, "marks.len() == N");
    for (i, mark) in result.answers.iter().enumerate() {
        let v = common::mat_value(mark).expect("mat");
        match v {
            Value::Number(n) => assert_eq!(n.spelling(), (i + 1).to_string(), "demand {i}"),
            other => panic!("demand {i}: {other:?}"),
        }
    }
}

#[test]
fn escaped_and_unicode_keys_in_project_has_keys_path() {
    let src = br#"{"a\nb":1,"\u0062":2}"#;
    let project = Demand::Project {
        path: Path::root(),
        fields: vec!["a\nb".into(), "b".into()],
    };
    let has_nl = Demand::Oracle(Oracle::HasKey { key: "a\nb".into() });
    let has_b = Demand::Oracle(Oracle::HasKey { key: "b".into() });
    let keys = Demand::Oracle(Oracle::MemberNames);
    let path_nl = Demand::path(vec![Step::Key("a\nb".into())]);
    let result = scan(
        src,
        &req(&[project, has_nl, has_b, keys, path_nl], Strictness::Structural),
    )
    .expect("scan");
    match &result.answers[0] {
        Answer::Columns(c) => {
            assert_eq!(c.rows(), 1);
            assert!(
                !matches!(c.cells()[0], structury::ColumnCell::Absent),
                "escaped a\\nb present"
            );
            assert!(
                !matches!(c.cells()[1], structury::ColumnCell::Absent),
                "decoded b present"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(result.answers[1], Answer::Oracle(OracleAnswer::HasKey(true))));
    assert!(matches!(result.answers[2], Answer::Oracle(OracleAnswer::HasKey(true))));
    match &result.answers[3] {
        Answer::Oracle(OracleAnswer::MemberNames(ks)) => {
            assert!(ks.iter().any(|k| k == "a\nb"), "{ks:?}");
            assert!(ks.iter().any(|k| k == "b"), "{ks:?}");
        }
        other => panic!("{other:?}"),
    }
    let v = common::mat_value(&result.answers[4]).expect("path");
    match v {
        Value::Number(n) => assert_eq!(n.spelling(), "1"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn unpaired_low_surrogate_is_refused_on_strict_and_materialize() {
    let src = b"\"\\uDC00\"";
    let demand = Demand::Whole;
    assert!(
        scan(src, &req(core::slice::from_ref(&demand), Strictness::Strict)).is_err(),
        "Strict skip must refuse unpaired low surrogate"
    );
    let lazy = scan(src, &req(core::slice::from_ref(&demand), Strictness::Lazy)).expect("Lazy locates");
    assert!(
        common::mat_value(&lazy.answers[0]).is_err(),
        "materialize must refuse unpaired low surrogate"
    );
}

#[test]
fn nesting_past_bound_is_json_limit() {
    let depth = usize::try_from(structury_json::MAX_NESTING).expect("u32") + 1;
    let mut src = Vec::new();
    src.extend(std::iter::repeat_n(b'[', depth));
    src.extend(std::iter::repeat_n(b']', depth));
    let demand = Demand::Whole;
    let err = scan(&src, &req(&[demand], Strictness::Strict)).expect_err("over-nested");
    assert_eq!(err.class(), ErrorClass::Limit, "{err}");
}

#[test]
fn request_max_nesting_is_honored() {
    let src = b"[[[[[1]]]]]";
    let demand = Demand::Whole;
    let mut request = req(core::slice::from_ref(&demand), Strictness::Strict);
    request.max_nesting = 4;
    let err = scan(src, &request).expect_err("depth 5 over a 4 bound");
    assert_eq!(err.class(), ErrorClass::Limit, "{err}");
    request.max_nesting = 8;
    assert!(scan(src, &request).is_ok(), "depth 5 under an 8 bound");
}

#[test]
fn filter_eq_uses_numeric_value_and_decoded_string() {
    let src = br#"[{"n":1.50,"s":"a\"b"},{"n":2,"s":"x"}]"#;
    let num = Demand::Filter {
        path: Path::root(),
        predicate: Predicate::Eq {
            field: "n".into(),
            value: Value::Number(structury::Number::parse("1.5").expect("1.5")),
        },
        project: vec!["n".into()],
    };
    let s = Demand::Filter {
        path: Path::root(),
        predicate: Predicate::Eq {
            field: "s".into(),
            value: Value::Str(r#"a"b"#.into()),
        },
        project: vec!["s".into()],
    };
    let result = scan(src, &req(&[num, s], Strictness::Structural)).expect("scan");
    match &result.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 1, "1.50 equals 1.5"),
        other => panic!("{other:?}"),
    }
    match &result.answers[1] {
        Answer::Columns(c) => assert_eq!(c.rows(), 1, "decoded a\\\"b matches a\"b"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stitch_ndjson_whole_equals_serial() {
    // Two manual windows, not plan_scan_shard: FIRST_SHARD_BYTES is 256KiB, so a
    // tiny buffer is one full-range scan_shard (identity with serial). Whole on
    // each record is a Document; stitch must concatenate those, not keep shard 0.
    let src = b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
    assert_eq!(src.len(), 27);
    let demand = Demand::Whole;
    let req = common::req_with(
        JsonInput::Ndjson,
        core::slice::from_ref(&demand),
        Strictness::Structural,
        structury_json::Dialect::Rfc8259,
    );
    let serial = scan(src, &req).expect("serial");
    let left = structury::ByteRange::try_new(0, 9).expect("left");
    let right = structury::ByteRange::try_new(9, 27).expect("right");
    assert_ne!(left.end(), src.len(), "left is not the full buffer");
    assert_ne!(right.start(), 0, "right is not the full buffer");
    let stitched = stitch(vec![
        common::scan_range(src, left, &req).expect("left"),
        common::scan_range(src, right, &req).expect("right"),
    ]);
    match &stitched.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 3, "Whole shards concatenate, they do not keep shard 0"),
        other => panic!("expected concatenated columns, got {other:?}"),
    }
    let a = common::mat_value(&serial.answers[0]).expect("serial mat");
    let b = common::mat_value(&stitched.answers[0]).expect("stitch mat");
    assert_eq!(a, b);
}

#[test]
fn count_with_path_keeps_fill_oracle_count() {
    let src = br#"{"a":1,"b":2}"#;
    let demands = [Demand::Oracle(Oracle::Count), Demand::path(vec![Step::Key("a".into())])];
    let result = scan(src, &req(&demands, Strictness::Structural)).expect("scan");
    match &result.answers[0] {
        Answer::Oracle(OracleAnswer::Count(n)) => {
            assert_eq!(*n, 2, "Count+Path must not overwrite Count with members.len()==0");
        }
        other => panic!("{other:?}"),
    }
    let v = common::mat_value(&result.answers[1]).expect("path a");
    match v {
        Value::Number(n) => assert_eq!(n.spelling(), "1"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn users_object_scan_compact_edit_leaves_untouched_members() {
    let src = br#"{"users":[{"id":1,"name":"a","pad":"keep"}]}"#;
    let demand = Demand::Whole;
    let result = scan(src, &req(core::slice::from_ref(&demand), Strictness::Strict)).expect("scan");
    assert_eq!(result.answers.len(), 1);
    let Answer::Document(doc) = &result.answers[0] else {
        panic!("document");
    };
    let mut compact = Vec::new();
    encode(Source::Document(doc), &EncodeOptions::compact(), &mut compact).expect("encode");
    assert_eq!(compact, src, "already-compact source is memcpy byte-exact");
    let out = structury_json::edit(
        src,
        &[structury_json::Edit::Set {
            path: vec![Step::Key("users".into()), Step::Index(0), Step::Key("name".into())],
            value: Value::Str("x".into()),
        }],
        structury_json::EditOptions::default(),
    )
    .expect("edit");
    let text = core::str::from_utf8(&out).expect("utf8");
    assert!(text.contains(r#""pad":"keep""#), "untouched pad stays verbatim: {text}");
    assert!(text.contains(r#""name":"x""#), "name spliced: {text}");
}

#[test]
fn truncated_ndjson_tail_is_an_issue() {
    let src = b"{\"id\":1}\n{\"id\":";
    let demand = Demand::Whole;
    let req = common::req_with(
        JsonInput::Ndjson,
        core::slice::from_ref(&demand),
        Strictness::Structural,
        structury_json::Dialect::Rfc8259,
    );
    let result = scan(src, &req).expect("truncated tail recovers");
    assert!(!result.issues.is_empty(), "truncated last record is Issue, not fatal");
    match &result.answers[0] {
        Answer::Columns(c) => assert_eq!(c.rows(), 1, "complete first record is kept"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn mixed_whole_on_scalar_keeps_comment_facts() {
    let src = b"1 // keep";
    let demands = [
        Demand::Whole,
        Demand::path(Vec::new()),
        Demand::path(vec![Step::Key("x".into())]),
    ];
    let mut req = common::req_with(
        JsonInput::Text,
        &demands,
        Strictness::Strict,
        structury_json::Dialect::Jsonc,
    );
    req.facts = true;
    let result = scan(src, &req).expect("scan");
    assert!(
        matches!(result.answers[2], Answer::TypeMismatch { .. }),
        "{:?}",
        result.answers[2]
    );
    for (i, mark) in result.answers.iter().enumerate().take(2) {
        match mark {
            Answer::Document(d) => {
                assert!(
                    d.facts()
                        .iter()
                        .any(|f| d.fact_bytes(f).windows(4).any(|w| w == b"keep")),
                    "Whole answer {i} must keep the comment fact: {:?}",
                    d.facts().iter().map(|f| d.fact_bytes(f)).collect::<Vec<_>>(),
                );
            }
            other => panic!("answer {i}: {other:?}"),
        }
    }
}

#[test]
fn lazy_escaped_key_matches() {
    let src = br#"{"a\nb":1}"#;
    let project = Demand::Project {
        path: Path::root(),
        fields: vec!["a\nb".into()],
    };
    let result = scan(src, &req(core::slice::from_ref(&project), Strictness::Lazy)).expect("scan");
    match &result.answers[0] {
        Answer::Columns(c) => {
            assert!(
                !matches!(c.cells()[0], structury::ColumnCell::Absent),
                "escaped key matched under Lazy"
            );
        }
        other => panic!("{other:?}"),
    }
    let path = Demand::path(vec![Step::Key("a\nb".into())]);
    let located = scan(src, &req(core::slice::from_ref(&path), Strictness::Lazy)).expect("scan");
    assert!(
        !matches!(located.answers[0], Answer::Missing),
        "{:?}",
        located.answers[0]
    );
}

#[test]
fn deep_nesting_parse_is_limit_not_overflow() {
    let depth = usize::try_from(structury_json::MAX_NESTING).expect("u32") + 8;
    let mut src = Vec::new();
    src.extend(std::iter::repeat_n(b'[', depth));
    src.extend(std::iter::repeat_n(b']', depth));
    let err = common::parsed(&src).expect_err("over-nested parse is refused");
    assert_eq!(err.class(), ErrorClass::Limit, "{err}");
}

/// A `Slice` window selects elements, then the nested filter applies, so a
/// filtered nested keeps one output row per matching element in the window.
#[test]
fn slice_window_over_a_filtered_nested() {
    let src: &[u8] = br#"[{"n":1},{"n":0},{"n":2}]"#;
    let nested = filter_root(gt("n", "0"), &["n"]);
    let cases = [
        (None, None, r#"[{"n":1},{"n":2}]"#),
        (Some(0), Some(2), r#"[{"n":1}]"#),
        (Some(2), Some(3), r#"[{"n":2}]"#),
        (Some(1), Some(3), r#"[{"n":2}]"#),
    ];
    for (start, end, want) in cases {
        let got = mat(&answer_of(src, &text(&[slice_nested(start, end, nested.clone())])));
        println!("slice({start:?},{end:?}) filtered: {got:?} want {want}");
        assert_eq!(
            got,
            json_value(want),
            "Slice{{{start:?}..{end:?}}} over a filtered nested produced {got:?}, want {want}"
        );
    }
}

/// A windowed filtered nested walks each element in element position: an array
/// element is one value, never a map of its inner elements.
#[test]
fn slice_window_filtered_array_elements() {
    let src: &[u8] = br#"[[{"n":1}],[{"n":0}]]"#;
    let got = mat(&answer_of(
        src,
        &text(&[slice_nested(Some(0), Some(2), filter_root(gt("n", "0"), &["n"]))]),
    ));
    println!("slice(0,2) filtered array elements: {got:?}");
    assert_eq!(got, json_value(r"[]"), "array elements must not map as rows");
}

/// The window law must not depend on the early-stop flag (Structural vs Strict).
#[test]
fn slice_window_filtered_strict_agrees() {
    let src: &[u8] = br#"[{"n":1},{"n":0},{"n":2}]"#;
    let d = slice_nested(Some(2), Some(3), filter_root(gt("n", "0"), &["n"]));
    let structural = obs_text(src, &d);
    let strict = obs_text_with(src, &[d], structury::Strictness::Strict);
    println!("filtered slice structural: {structural}");
    println!("filtered slice strict    : {strict}");
    assert_eq!(structural, strict, "the slice window differs by strictness");
}

/// A `Slice` window over a projected nested is an element window (control).
#[test]
fn slice_window_over_projected_nested() {
    let src: &[u8] = br#"[{"d":1},{"d":2},{"d":3}]"#;
    let got = mat(&answer_of(
        src,
        &text(&[slice_nested(Some(1), Some(3), project_root(&["d"]))]),
    ));
    println!("slice(1,3) projected: {got:?}");
    assert_eq!(
        got,
        json_value(r#"[{"d":2},{"d":3}]"#),
        "slice window over a projection misaligned: {got:?}"
    );
}

fn value_of(src: &[u8]) -> Value {
    json_value(core::str::from_utf8(src).expect("utf8"))
}

fn ref_filtered_slice(tree: &Value, start: Option<i64>, end: Option<i64>, pred: &Predicate, project: &[&str]) -> Value {
    let Value::Array(items) = tree else {
        panic!("not an array");
    };
    let window = Range { start, end }.window(items.len());
    Value::Array(
        items[window]
            .iter()
            .filter(|e| holds(e, pred))
            .map(|e| {
                if project.is_empty() {
                    e.clone()
                } else {
                    project_row(e, project)
                }
            })
            .collect(),
    )
}

/// The element-window law for `Slice` of `Slice`: window the outer elements,
/// then apply the inner window to each and concatenate its cells.
fn ref_slice_of_slice(src: &[u8], o0: Option<i64>, o1: Option<i64>, i0: Option<i64>, i1: Option<i64>) -> Value {
    let tree = value_of(src);
    let Value::Array(items) = &tree else { panic!("array") };
    let ow = Range { start: o0, end: o1 }.window(items.len());
    let mut want = Vec::new();
    for e in &items[ow] {
        if let Value::Array(inner_items) = e {
            let iw = Range { start: i0, end: i1 }.window(inner_items.len());
            want.extend(inner_items[iw].to_vec());
        } else {
            want.push(e.clone());
        }
    }
    Value::Array(want)
}

#[test]
#[allow(clippy::too_many_lines)]
fn validate_tail_matrix() {
    let root = project_root(&["id"]);
    let rows = project_key("rows", &["id"]);
    let two = r#"[{"id":1},{"id":2}]"#;
    let obj = r#"{"rows":[{"id":1},{"id":2}]}"#;

    let mk = |s: String| s.into_bytes();
    let cases: Vec<(&str, Vec<u8>, Demand, Dialect)> = vec![
        ("valid array", mk(two.into()), root.clone(), Dialect::Rfc8259),
        (
            "valid + spaces",
            mk(format!("{two}   ")),
            root.clone(),
            Dialect::Rfc8259,
        ),
        (
            "trailing junk",
            mk(format!("{two} junk")),
            root.clone(),
            Dialect::Rfc8259,
        ),
        ("trailing value", mk(format!("{two} 1")), root.clone(), Dialect::Rfc8259),
        ("extra ]", mk(format!("{two}]")), root.clone(), Dialect::Rfc8259),
        (
            "trailing comma",
            mk(r#"[{"id":1},{"id":2},]"#.into()),
            root.clone(),
            Dialect::Rfc8259,
        ),
        (
            "array no closer",
            mk(r#"[{"id":1},{"id":2}"#.to_string()),
            root.clone(),
            Dialect::Rfc8259,
        ),
        (
            "jsonc trailing comment",
            mk(format!("{two} /*c*/")),
            root.clone(),
            Dialect::Jsonc,
        ),
        (
            "jsonc comment + junk",
            mk(format!("{two} /*c*/ junk")),
            root.clone(),
            Dialect::Jsonc,
        ),
        (
            "jsonc line comment",
            mk(format!("{two} // c")),
            root.clone(),
            Dialect::Jsonc,
        ),
        (
            "crlf trailing",
            mk(format!("{two}\r\n")),
            root.clone(),
            Dialect::Rfc8259,
        ),
        (
            "bom + junk",
            {
                let mut v = vec![0xEF, 0xBB, 0xBF];
                v.extend_from_slice(format!("{two} junk").as_bytes());
                v
            },
            root.clone(),
            Dialect::Rfc8259,
        ),
        ("valid object", mk(obj.into()), rows.clone(), Dialect::Rfc8259),
        (
            "object trailing junk",
            mk(format!("{obj} junk")),
            rows.clone(),
            Dialect::Rfc8259,
        ),
        (
            "object missing brace",
            mk(r#"{"rows":[{"id":1},{"id":2}]"#.into()),
            rows.clone(),
            Dialect::Rfc8259,
        ),
        (
            "object trailing comma",
            mk(r#"{"rows":[{"id":1},{"id":2}],}"#.into()),
            rows.clone(),
            Dialect::Rfc8259,
        ),
        (
            "junk before close",
            mk(r#"{"rows":[{"id":1},{"id":2}] junk}"#.into()),
            rows.clone(),
            Dialect::Rfc8259,
        ),
        (
            "later member malformed",
            mk(r#"{"rows":[{"id":1},{"id":2}],"a":}"#.into()),
            rows.clone(),
            Dialect::Rfc8259,
        ),
        (
            "later member valid",
            mk(r#"{"rows":[{"id":1},{"id":2}],"a":1}"#.into()),
            rows.clone(),
            Dialect::Rfc8259,
        ),
        (
            "deep valid",
            mk(r#"{"a":{"rows":[{"id":1},{"id":2}]}}"#.into()),
            project_steps(&[Step::Key("a".into()), Step::Key("rows".into())], &["id"]),
            Dialect::Rfc8259,
        ),
        (
            "deep missing inner",
            mk(r#"{"a":{"rows":[{"id":1},{"id":2}]}"#.into()),
            project_steps(&[Step::Key("a".into()), Step::Key("rows".into())], &["id"]),
            Dialect::Rfc8259,
        ),
        (
            "deep trailing junk",
            mk(r#"{"a":{"rows":[{"id":1},{"id":2}]}} junk"#.into()),
            project_steps(&[Step::Key("a".into()), Step::Key("rows".into())], &["id"]),
            Dialect::Rfc8259,
        ),
        (
            "json5 trailing comma obj",
            mk(r#"{"rows":[{"id":1},{"id":2},],}"#.into()),
            rows.clone(),
            Dialect::Json5,
        ),
        (
            "index path valid",
            mk(r#"{"rows":[[{"id":1},{"id":2}]]}"#.into()),
            project_steps(&[Step::Key("rows".into()), Step::Index(0)], &["id"]),
            Dialect::Rfc8259,
        ),
        (
            "index path junk",
            mk(r#"{"rows":[[{"id":1},{"id":2}]]} junk"#.into()),
            project_steps(&[Step::Key("rows".into()), Step::Index(0)], &["id"]),
            Dialect::Rfc8259,
        ),
        (
            "index path missing outer",
            mk(r#"{"rows":[[{"id":1},{"id":2}]]"#.into()),
            project_steps(&[Step::Key("rows".into()), Step::Index(0)], &["id"]),
            Dialect::Rfc8259,
        ),
    ];
    for (name, src, demand, dialect) in cases {
        let tag = sharded(&src, core::slice::from_ref(&demand), dialect);
        println!("{name:28} {tag}");
        assert!(
            tag == "AGREE" || tag.starts_with("BOTH ERR (same)"),
            "{name}: the plan door disagrees with serial ({tag})"
        );
    }
}

#[test]
fn filtered_slice_windows_match_the_element_window() {
    let src = br#"[{"n":1},{"n":0},{"n":2},{"n":0},{"n":5}]"#;
    let tree = value_of(src);
    let pred = gt("n", "0");
    let windows: Vec<(Option<i64>, Option<i64>)> = vec![
        (None, None),
        (Some(0), Some(2)),
        (Some(2), Some(3)),
        (Some(1), Some(3)),
        (Some(0), Some(3)),
        (None, Some(1)),
        (Some(1), None),
        (Some(0), Some(0)),
        (Some(5), Some(7)),
        (Some(2), Some(2)),
        (Some(2), Some(1)),
        (Some(-1), None),
        (None, Some(-1)),
        (Some(-2), Some(-1)),
        (Some(-100), Some(1)),
    ];
    for (start, end) in windows {
        for project in [&["n"][..], &[][..]] {
            let demand = slice_nested(start, end, filter_root(pred.clone(), project));
            let got = mat(&answer_of(src, &text(&[demand])));
            let want = ref_filtered_slice(&tree, start, end, &pred, project);
            let tag = if got == want { "ok" } else { "MISMATCH" };
            println!("window({start:?},{end:?}) project={project:?} {tag} got={got:?} want={want:?}");
            assert_eq!(
                got, want,
                "filtered Slice window ({start:?},{end:?}) project {project:?}"
            );
        }
    }
}

#[test]
fn filtered_slice_empty_and_non_object() {
    let cases: Vec<(&[u8], Predicate, &[&str])> = vec![
        (br#"[{"a":1},{"b":2}]"#, gt("a", "100"), &["a"]),
        (br"[0,1,2]", gt("a", "0"), &["a"]),
        (br#"[{"n":1},5,{"n":2}]"#, gt("n", "0"), &["n"]),
        (br"[]", gt("n", "0"), &["n"]),
    ];
    for (src, pred, project) in cases {
        let tree = value_of(src);
        for (start, end) in [(None, None), (Some(0), Some(1)), (Some(1), Some(2)), (Some(0), Some(3))] {
            let demand = slice_nested(start, end, filter_root(pred.clone(), project));
            let got = mat(&answer_of(src, &text(&[demand])));
            let want = ref_filtered_slice(&tree, start, end, &pred, project);
            println!(
                "src={} window({start:?},{end:?}) got={got:?} want={want:?}",
                String::from_utf8_lossy(src)
            );
            assert_eq!(got, want, "empty/non-object filtered slice");
        }
    }
}

#[test]
fn filtered_slice_nested_predicates() {
    let src = br#"[{"a":1,"b":1},{"a":1,"b":0},{"a":0,"b":1},{"a":2,"b":2}]"#;
    let tree = value_of(src);
    let preds: Vec<(&str, Predicate)> = vec![
        ("and", Predicate::And(Box::new(gt("a", "0")), Box::new(gt("b", "0")))),
        ("or", Predicate::Or(Box::new(gt("a", "1")), Box::new(gt("b", "1")))),
        ("not", Predicate::Not(Box::new(gt("a", "0")))),
    ];
    for (name, pred) in preds {
        for (start, end) in [(None, None), (Some(1), Some(3)), (Some(2), Some(4))] {
            let demand = slice_nested(start, end, filter_root(pred.clone(), &["a"]));
            let got = mat(&answer_of(src, &text(&[demand])));
            let want = ref_filtered_slice(&tree, start, end, &pred, &["a"]);
            println!("{name} window({start:?},{end:?}) got={got:?} want={want:?}");
            assert_eq!(got, want, "nested predicate filtered slice");
        }
    }
}

#[test]
fn filtered_slice_with_a_path_nested_filter() {
    // Element window first, then each element's nested array is filtered.
    let src = br#"[{"a":[{"n":1},{"n":0}]},{"a":[{"n":2}]},{"a":[{"n":0}]}]"#;
    let pred = gt("n", "0");
    for (start, end) in [(None, None), (Some(0), Some(2)), (Some(1), Some(3)), (Some(2), Some(3))] {
        let nested = Demand::Filter {
            path: structury::Path::key("a"),
            predicate: pred.clone(),
            project: vec!["n".into()],
        };
        let got = mat(&answer_of(src, &text(&[slice_nested(start, end, nested)])));
        // Reference: window the *elements*, then for each, nav to `a` and filter.
        let tree = value_of(src);
        let Value::Array(items) = &tree else { panic!("array") };
        let w = Range { start, end }.window(items.len());
        let mut want = Vec::new();
        for e in &items[w] {
            if let Some(Value::Array(sub)) = nav(e, &structury::Path::key("a")) {
                want.extend(sub.iter().filter(|x| holds(x, &pred)).map(|x| project_row(x, &["n"])));
            }
        }
        let want = Value::Array(want);
        println!("path-filter window({start:?},{end:?}) got={got:?} want={want:?}");
        assert_eq!(got, want, "path-nested filtered slice");
    }
}

#[test]
fn slice_of_slice_is_element_windowed() {
    let src = br"[[0,1,2],[3,4,5],[6,7,8]]";
    let mut bad = Vec::new();
    for (o0, o1, i0, i1) in [
        (None, None, None, None),
        (Some(0), Some(2), Some(0), Some(1)),
        (Some(1), Some(3), Some(1), Some(3)),
    ] {
        let inner = Demand::Slice {
            range: Range { start: i0, end: i1 },
            nested: None,
        };
        let got = mat(&answer_of(src, &text(&[slice_nested(o0, o1, inner)])));
        let want = ref_slice_of_slice(src, o0, o1, i0, i1);
        if got != want {
            bad.push(format!(
                "outer({o0:?},{o1:?}) inner({i0:?},{i1:?}) got={got:?} want={want:?}"
            ));
        }
    }
    assert!(bad.is_empty(), "Slice of Slice:\n  {}", bad.join("\n  "));
}

#[test]
fn index_in_project_path_matches_the_element_law() {
    // `rows` is an array whose element 0 is the array of objects. A trailing
    // `Index` in a `Project` path selects that element; it is projected in
    // element position, so a non-object element is one absent row. This
    // is the DOM's `Project{path:[Index]}` law.
    let src = br#"{"rows":[[{"id":1},{"id":2}]]}"#;
    let demand = project_steps(&[Step::Key("rows".into()), Step::Index(0)], &["id"]);
    let got = mat(&answer_of(src, &text(core::slice::from_ref(&demand))));
    println!("index-path serial={got:?}");
    assert_eq!(got, json_value("[{}]"), "serial Project with an Index in the path");
    let tag = sharded(src, core::slice::from_ref(&demand), Dialect::Rfc8259);
    println!("index-path sharded: {tag}");
    assert_eq!(tag, "AGREE", "index-in-path serial vs plan: {tag}");
}

#[test]
fn slice_window_non_aligned_is_element_windowed() {
    let src = br#"[{"a":[{"n":1},{"n":2}]},{"a":[{"n":3}]},{"a":[{"n":4},{"n":5}]}]"#;
    let tree = value_of(src);
    let Value::Array(items) = &tree else { panic!("array") };
    let mut bad = Vec::new();
    for (start, end) in [(Some(1), Some(2)), (Some(0), Some(1)), (Some(2), Some(3))] {
        let nested = project_steps(&[Step::Key("a".into())], &["n"]);
        let got = mat(&answer_of(src, &text(&[slice_nested(start, end, nested)])));
        let w = Range { start, end }.window(items.len());
        let mut want = Vec::new();
        for e in &items[w] {
            if let Some(Value::Array(sub)) = nav(e, &structury::Path::key("a")) {
                want.extend(sub.iter().map(|x| project_row(x, &["n"])));
            }
        }
        let want = Value::Array(want);
        if got != want {
            bad.push(format!("window({start:?},{end:?}) got={got:?} want={want:?}"));
        }
    }
    assert!(bad.is_empty(), "non-aligned Projected Slice:\n  {}", bad.join("\n  "));

    // A nested `Collection` over array elements is element-windowed too: the
    // window selects element 1, then the nested collection maps it.
    let nested = collection(None, Some(project_root(&["n"])));
    assert_eq!(
        mat(&answer_of(
            br#"[[{"n":1}],[{"n":2}],[{"n":3}]]"#,
            &text(&[slice_nested(Some(1), Some(2), nested)])
        )),
        json_value(r#"[{"n":2}]"#),
        "windowed nested Collection over array elements"
    );
}

#[test]
fn filtered_slice_is_dialect_independent() {
    let src = br#"[{"n":1},{"n":0},{"n":2}]"#;
    let d = slice_nested(Some(2), Some(3), filter_root(gt("n", "0"), &["n"]));
    let rfc = obs(src, &d, Dialect::Rfc8259);
    let json5 = obs(src, &d, Dialect::Json5);
    let commented = obs(src, &d, Dialect::Jsonc);
    println!("rfc={rfc} json5={json5} jsonc={commented}");
    assert_eq!(rfc, json5, "Json5 filtered slice");
    assert_eq!(rfc, commented, "Jsonc filtered slice");
}

#[test]
fn slice_edge_windows_direct() {
    // Direct `Slice` with no nested demand: windows of an array of scalars.
    let src = br"[10,20,30]";
    let cases: Vec<(&str, Option<i64>, Option<i64>, &str)> = vec![
        ("all", None, None, "[10,20,30]"),
        ("0..2", Some(0), Some(2), "[10,20]"),
        ("1..3", Some(1), Some(3), "[20,30]"),
        ("2..3", Some(2), Some(3), "[30]"),
        ("empty 0..0", Some(0), Some(0), "[]"),
        ("empty 2..1", Some(2), Some(1), "[]"),
        ("past end", Some(5), Some(7), "[]"),
        ("neg start", Some(-1), None, "[30]"),
        ("neg end", None, Some(-1), "[10,20]"),
    ];
    for (name, start, end, want) in cases {
        let got = mat(&answer_of(
            src,
            &text(&[Demand::Slice {
                range: Range { start, end },
                nested: None,
            }]),
        ));
        println!("direct slice {name:10} got={got:?} want={want}");
        assert_eq!(got, json_value(want), "direct Slice {name}");
    }
    // Empty and one-element arrays do not split; the window is still exact.
    for (src2, start, end, want) in [
        (br"[]".as_slice(), None, None, "[]"),
        (br#"[{"id":0}]"#.as_slice(), Some(0), Some(1), r#"[{"id":0}]"#),
        (br#"[{"id":0}]"#.as_slice(), Some(1), Some(2), "[]"),
    ] {
        let d = Demand::Slice {
            range: Range { start, end },
            nested: Some(Box::new(project_root(&["id"]))),
        };
        let got = mat(&answer_of(src2, &text(&[d])));
        println!("edge slice got={got:?} want={want}");
        assert_eq!(got, json_value(want), "edge Slice");
    }
}

#[test]
fn object_envelope_eof_error_code_matches_serial() {
    let rows = project_key("rows", &["id"]);
    let deep = project_steps(&[Step::Key("a".into()), Step::Key("rows".into())], &["id"]);
    let cases: Vec<(&str, &[u8], Demand)> = vec![
        ("missing outer brace", br#"{"rows":[{"id":1},{"id":2}]"#, rows.clone()),
        ("missing inner brace", br#"{"a":{"rows":[{"id":1},{"id":2}]}"#, deep),
    ];
    for (name, src, demand) in cases {
        let request = mk_req(
            core::slice::from_ref(&demand),
            Dialect::Rfc8259,
            Strictness::Structural,
            false,
        );
        let serial = scan(src, &request).unwrap_err();
        let plan = Plan::build(src, &request).unwrap_err();
        println!("{name}: serial={:?} {}", serial.code(), plan.code());
        assert_eq!(serial.code(), plan.code(), "{name}: error code differs between doors");
        assert_eq!(
            serial.offset(),
            plan.offset(),
            "{name}: error offset differs between doors"
        );
    }
}

#[test]
fn validate_tail_accepts_valid_controls() {
    // No false positive: valid documents must not be rejected by `validate_tail`.
    let root = project_root(&["id"]);
    let rows = project_key("rows", &["id"]);
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(br#"[{"id":1},{"id":2}]"#);
    let cases: Vec<(&str, Vec<u8>, Demand, Dialect)> = vec![
        ("bom valid", bom, root.clone(), Dialect::Rfc8259),
        (
            "jsonc valid comment",
            br#"[{"id":1},{"id":2}] /*c*/"#.to_vec(),
            root.clone(),
            Dialect::Jsonc,
        ),
        (
            "jsonc valid line",
            br#"[{"id":1},{"id":2}] // c"#.to_vec(),
            root.clone(),
            Dialect::Jsonc,
        ),
        (
            "jsonc valid inner",
            br#"[{"id":1}/*x*/,{"id":2}] // c"#.to_vec(),
            root.clone(),
            Dialect::Jsonc,
        ),
        (
            "json5 valid trailing comma",
            br#"[{"id":1},{"id":2},]"#.to_vec(),
            root.clone(),
            Dialect::Json5,
        ),
        (
            "json5 valid comment",
            br#"[{"id":1},{"id":2}]/*c*/"#.to_vec(),
            root.clone(),
            Dialect::Json5,
        ),
        (
            "object jsonc trailing",
            br#"{"rows":[{"id":1},{"id":2}]} /*c*/"#.to_vec(),
            rows.clone(),
            Dialect::Jsonc,
        ),
        (
            "object json5 trailing comma",
            br#"{"rows":[{"id":1},{"id":2},],}"#.to_vec(),
            rows.clone(),
            Dialect::Json5,
        ),
    ];
    for (name, src, demand, dialect) in cases {
        let tag = sharded(&src, core::slice::from_ref(&demand), dialect);
        println!("{name:26} {tag}");
        assert!(
            tag == "AGREE" || tag.starts_with("BOTH ERR"),
            "{name}: valid document rejected by the plan ({tag})"
        );
        assert!(tag == "AGREE", "{name}: valid document must agree ({tag})");
    }
}

#[test]
fn filtered_slice_with_a_sibling_demand() {
    let src = br#"[{"n":1},{"n":0},{"n":2}]"#;
    let tree = value_of(src);
    let pred = gt("n", "0");
    let slice = slice_nested(Some(2), Some(3), filter_root(pred.clone(), &["n"]));
    let sibling = project_root(&["n"]);
    let r = scan(src, &text(&[slice, sibling])).unwrap();
    let got = mat(&r.answers[0]);
    let want = ref_filtered_slice(&tree, Some(2), Some(3), &pred, &["n"]);
    println!(
        "filtered slice + sibling: got={got:?} want={want:?} sibling={:?}",
        mat(&r.answers[1])
    );
    assert_eq!(got, want, "filtered Slice with a sibling demand");
}

/// The element-window reference: window the root elements, then apply the
/// nested demand to each selected element and concatenate.
fn ref_window(tree: &Value, range: Range, nested: &Demand) -> Value {
    let Value::Array(items) = tree else {
        panic!("window reference needs an array root");
    };
    let window = range.window(items.len());
    let mut out = Vec::new();
    for element in &items[window] {
        ref_apply(element, nested, &mut out);
    }
    Value::Array(out)
}

/// The `RowShape::Projected` law: project the located value's elements, and if
/// that yields no cell at all, contribute one absent row.
fn ref_projected(target: Option<&Value>, fields: &[String], out: &mut Vec<Value>) {
    let before = out.len();
    match target {
        Some(Value::Array(items)) => {
            for item in items {
                out.push(project_row(item, fields));
            }
        }
        Some(object @ Value::Object(_)) => out.push(project_row(object, fields)),
        Some(_) | None => out.push(Value::Object(Vec::new())),
    }
    if out.len() == before {
        out.push(Value::Object(Vec::new()));
    }
}

fn ref_apply(element: &Value, nested: &Demand, out: &mut Vec<Value>) {
    match nested {
        Demand::Whole => out.push(element.clone()),
        Demand::Project { path, fields } => ref_projected(nav(element, path), fields, out),
        Demand::Filter {
            path,
            predicate,
            project,
        } => {
            if let Some(object @ Value::Object(_)) = nav(element, path)
                && holds(object, predicate)
            {
                out.push(if project.is_empty() {
                    object.clone()
                } else {
                    project_row(object, project)
                });
            }
        }
        Demand::Collection { fields, nested } => {
            let Value::Array(items) = element else {
                return;
            };
            match nested.as_deref() {
                None => match fields {
                    Some(keys) => {
                        for item in items {
                            out.push(project_row(item, keys));
                        }
                    }
                    None => out.extend(items.iter().cloned()),
                },
                Some(Demand::Project { path, fields }) => {
                    // The element is mapped through the inner project; an empty
                    // element array contributes no outer row (the `Slice` row
                    // shape of a nested `Collection` is `Whole`, not `Projected`).
                    for item in items {
                        ref_projected(nav(item, path), fields, out);
                    }
                }
                Some(inner) => {
                    for item in items {
                        ref_apply(item, inner, out);
                    }
                }
            }
        }
        Demand::Slice { range, nested } => {
            let Value::Array(items) = element else {
                return;
            };
            let inner = range.window(items.len());
            for item in &items[inner] {
                match nested.as_deref() {
                    None => out.push(item.clone()),
                    Some(inner_demand) => ref_apply(item, inner_demand, out),
                }
            }
        }
        _ => {}
    }
}

#[test]
#[allow(clippy::items_after_statements, clippy::too_many_lines)]
fn slice_element_window_matrix() {
    let starts: &[Option<i64>] = &[
        None,
        Some(0),
        Some(1),
        Some(2),
        Some(3),
        Some(4),
        Some(10),
        Some(-1),
        Some(-2),
        Some(-5),
    ];
    let ends: &[Option<i64>] = &[
        None,
        Some(0),
        Some(1),
        Some(2),
        Some(3),
        Some(4),
        Some(10),
        Some(-1),
        Some(-2),
        Some(-5),
    ];

    struct Row {
        name: &'static str,
        src: &'static [u8],
        nested: Demand,
    }
    let rows: Vec<Row> = vec![
        Row {
            name: "project-flat",
            src: br#"[{"x":1,"n":1},{"x":2,"n":0},{"x":3,"n":2},{"x":4,"n":0}]"#,
            nested: project_root(&["x"]),
        },
        Row {
            name: "project-with-path",
            src: br#"[{"a":[{"x":1},{"x":2}]},{"a":[{"x":3}]},{"a":[]},{"b":1}]"#,
            nested: Demand::Project {
                path: Path::key("a"),
                fields: vec!["x".into()],
            },
        },
        Row {
            name: "filter-flat",
            src: br#"[{"x":1,"n":1},{"x":2,"n":0},{"x":3,"n":2},{"x":4,"n":0}]"#,
            nested: Demand::Filter {
                path: Path::root(),
                predicate: Predicate::Gt {
                    field: "n".into(),
                    value: num("0"),
                },
                project: vec!["x".into()],
            },
        },
        Row {
            name: "collection-projected",
            src: br#"[[{"x":1},{"x":2}],[{"x":3}],[]]"#,
            nested: collection(None, Some(project_root(&["x"]))),
        },
        Row {
            name: "slice-of-slice",
            src: br"[[1,2,3],[4,5,6],[7,8,9]]",
            nested: slice(Some(0), Some(2)),
        },
    ];

    let mut mismatches = Vec::new();
    let mut checks = 0usize;
    for row in &rows {
        let tree = parsed(row.src).expect("parse");
        for dialect in [Dialect::Rfc8259, Dialect::Jsonc, Dialect::Json5] {
            for &start in starts {
                for &end in ends {
                    let range = Range { start, end };
                    let demand = Demand::Slice {
                        range,
                        nested: Some(Box::new(row.nested.clone())),
                    };
                    let got = match scan(row.src, &common::text_dialect(core::slice::from_ref(&demand), dialect)) {
                        Ok(result) => mat_value(&result.answers[0]).expect("value"),
                        Err(error) => {
                            mismatches.push(format!(
                                "{} dialect={dialect:?} ({start:?},{end:?}) scan error {error:?}",
                                row.name
                            ));
                            continue;
                        }
                    };
                    let want = ref_window(&tree, range, &row.nested);
                    checks += 1;
                    if got != want {
                        mismatches.push(format!(
                            "{} dialect={dialect:?} ({start:?},{end:?}) got={got:?} want={want:?}",
                            row.name
                        ));
                    }
                }
            }
        }
    }
    println!("slice-window checks: {checks}, mismatches: {}", mismatches.len());
    for m in mismatches.iter().take(40) {
        println!("  MISMATCH {m}");
    }
    assert!(mismatches.is_empty(), "{} slice-window mismatches", mismatches.len());
}

/// An `Index` in a `Project`/`Filter` path forces the serial door; the answer
/// must be the DOM law, not merely `plan == serial`.
#[test]
fn index_in_path_is_dom_correct() {
    let mut bad = Vec::new();
    // Project path [rows, Index(i)]: select element i, then project.
    let src = br#"{"rows":[{"id":1,"n":1},{"id":2,"n":0},{"id":3,"n":2}]}"#;
    let tree = parsed(src).expect("parse");
    let arr = nav(&tree, &Path::key("rows")).expect("rows");
    for index in [-4i64, -3, -1, 0, 1, 2, 3, 9] {
        let demand = Demand::Project {
            path: Path {
                steps: vec![Step::Key("rows".into()), Step::Index(index)],
            },
            fields: vec!["id".into()],
        };
        let parts = morsels(src, core::slice::from_ref(&demand), Dialect::Rfc8259);
        let answer = common::answer_of(src, &text(core::slice::from_ref(&demand)));
        let selected = arr.element(index);
        match selected {
            Some(Value::Object(_)) => {
                let want = Value::Array(vec![project_row(selected.unwrap(), &["id"])]);
                let got = mat_value(&answer).expect("value");
                if got != want {
                    bad.push(format!("Index({index}) object got={got:?} want={want:?}"));
                }
            }
            Some(_) => {
                let got = mat_value(&answer).expect("value");
                let want = Value::Array(vec![Value::Object(Vec::new())]);
                if got != want {
                    bad.push(format!("Index({index}) non-object got={got:?} want={want:?}"));
                }
            }
            None => {
                if !matches!(answer, Answer::Missing) {
                    bad.push(format!("Index({index}) out-of-range got={answer:?} want Missing"));
                }
            }
        }
        if parts > 1 {
            bad.push(format!("Index({index}) reached {parts} morsels"));
        }
        println!("Project Index({index}) morsels={parts} answer={answer:?}");
    }

    // Filter path [rows, Index(i)].
    for index in [-4i64, -1, 0, 2, 3, 9] {
        let demand = Demand::Filter {
            path: Path {
                steps: vec![Step::Key("rows".into()), Step::Index(index)],
            },
            predicate: Predicate::Gt {
                field: "n".into(),
                value: num("0"),
            },
            project: vec!["id".into()],
        };
        let parts = morsels(src, core::slice::from_ref(&demand), Dialect::Rfc8259);
        let answer = common::answer_of(src, &text(core::slice::from_ref(&demand)));
        let selected = arr.element(index);
        match selected {
            Some(object @ Value::Object(_))
                if holds(
                    object,
                    &Predicate::Gt {
                        field: "n".into(),
                        value: num("0"),
                    },
                ) =>
            {
                let want = Value::Array(vec![project_row(object, &["id"])]);
                let got = mat_value(&answer).expect("value");
                if got != want {
                    bad.push(format!("Filter Index({index}) match got={got:?} want={want:?}"));
                }
            }
            Some(_) => {
                if !matches!(answer, Answer::Columns(_)) {
                    bad.push(format!(
                        "Filter Index({index}) no-match got={answer:?} want empty Columns"
                    ));
                }
            }
            None => {
                if !matches!(answer, Answer::Missing) {
                    bad.push(format!(
                        "Filter Index({index}) out-of-range got={answer:?} want Missing"
                    ));
                }
            }
        }
        println!("Filter Index({index}) morsels={parts} answer={answer:?}");
    }
    assert!(
        bad.is_empty(),
        "index in path serial correctness:\n  {}",
        bad.join("\n  ")
    );
}
