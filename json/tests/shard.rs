//! Shard doors: plan + per-range scan + stitch equals a serial scan, admitted
//! shapes fold byte-identically, and refused shapes plan one range. The codec
//! starts no thread.

mod common;

use crate::common::*;

use structury::{
    Answer, ByteRange, Control, Demand, ErrorClass, Oracle, OracleAnswer, Path, Predicate, Range, Shard, Step,
    Strictness, stitch,
};
use structury_json::{Dialect, FIRST_SHARD_BYTES, JsonInput, Plan, ScanRequest, scan};

#[test]
fn duplicate_array_path_shards_the_last_member() {
    let cases: &[(&[u8], &[&str])] = &[
        (br#"{"a":[1,2],"a":[3,4,5]}"#, &["a"]),
        (br#"{"a":[1,2],"a":[3,4,5],"a":[6,7]}"#, &["a"]),
        (br#"{"a":5,"a":[3,4,5]}"#, &["a"]),
        (br#"{"a":[1,2],"a":5}"#, &["a"]),
        (br#"{"a":{"b":[1,2]},"a":{"b":[3,4,5]}}"#, &["a", "b"]),
        (br#"{"x":{"b":[1]},"x":{"b":[2,3]},"x":{"b":[4,5,6]}}"#, &["x", "b"]),
        (br#"{"a":[1,2],"\u0061":[3,4,5]}"#, &["a"]),
    ];
    for (src, keys) in cases {
        let steps = keys.iter().map(|key| Step::Key((*key).into())).collect();
        let demand = Demand::Path {
            steps,
            nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
        };
        assert_plan_matches_serial(src, &text_req(&demand));
    }
}

/// Plan + per-range scan + stitch observes what one serial scan observes,
/// answer for answer and error for error.
fn assert_plan_matches_serial(src: &[u8], req: &ScanRequest<'_>) {
    let serial = scan(src, req);
    let plan = Plan::build(src, req).expect("plan");
    let parts: Vec<_> = plan.ranges(1).iter().map(|range| plan.scan(src, *range)).collect();
    let first_error = parts.iter().find_map(|part| part.as_ref().err());
    match (serial, first_error) {
        (Ok(serial), None) => {
            let scanned: Vec<_> = parts.into_iter().map(|part| part.expect("part")).collect();
            assert_eq!(
                common::observe_all(&stitch(scanned).answers),
                common::observe_all(&serial.answers),
                "{src:?}"
            );
        }
        (Err(serial), Some(error)) => assert_eq!(serial.code(), error.code(), "{src:?}"),
        _ => panic!("plan and serial disagree on {src:?}"),
    }
}

/// The host re-entry resolves the same shadow: a unique path plans from the
/// summaries, and a duplicate path falls back to the eager walk's answer.
#[test]
fn host_ranges_resolve_a_shadowed_path() {
    let demand = Demand::Path {
        steps: vec![Step::Key("a".into())],
        nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
    };
    let req = text_req(&demand);
    for src in [
        br#"{"a":[1,2,3,4,5,6,7,8,9,10]}"#.as_slice(),
        br#"{"a":[1,2],"a":[3,4,5]}"#,
    ] {
        let serial = scan(src, &req).expect("serial");
        let start = Plan::array_start(src, &req).expect("resolve").expect("[");
        let region = ByteRange::try_new(start + 1, src.len()).expect("region");
        let summaries = [Plan::cut(src, region)];
        let parts: Vec<_> = Plan::host_ranges(src, &req, &summaries, 1)
            .expect("host ranges")
            .iter()
            .map(|range| common::scan_range(src, *range, &req).expect("part"))
            .collect();
        assert_eq!(
            common::observe_all(&stitch(parts).answers),
            common::observe_all(&serial.answers),
            "{src:?}"
        );
    }
}

/// The retained plan declines a shadowed path into the serial whole range.
#[test]
fn from_summaries_falls_back_on_a_shadowed_path() {
    let demand = Demand::Path {
        steps: vec![Step::Key("a".into())],
        nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
    };
    let req = text_req(&demand);
    let src = br#"{"a":[1,2],"a":[3,4,5]}"#;
    let serial = scan(src, &req).expect("serial");
    let start = Plan::array_start(src, &req).expect("resolve").expect("[");
    let cut = Plan::cut(src, ByteRange::try_new(start, src.len()).expect("range"));
    let plan = Plan::from_summaries(src, start, &[cut], &req);
    let ranges = plan.ranges(64);
    assert_eq!(ranges.len(), 1, "a shadowed path is one whole range");
    let out = plan.scan(src, ranges[0]).expect("scan");
    assert_eq!(common::observe_all(&out.answers), common::observe_all(&serial.answers));
}

/// The fast bound never enters the body: a malformed body still yields the
/// start, while planning raises the lexer's error from the serial scan.
#[test]
fn array_start_bounds_the_body_without_scanning_it() {
    let demand = Demand::Project {
        path: Path::key("users"),
        fields: vec!["id".into()],
    };
    let req = text_req(&demand);
    let src = br#"{"users":[1, /2]}"#;
    let start = Plan::array_start(src, &req).expect("resolve").expect("[");
    assert_eq!(start, 9, "the byte after `:`");
    assert_eq!(plan_ranges(src, &req).unwrap_err().code(), "invalid-comment");
}

#[test]
fn stream_key_paths_keep_virtual_array_semantics_in_plans() {
    let demands = [
        Demand::Path {
            steps: vec![Step::Key("a".into())],
            nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
        },
        Demand::Project {
            path: Path::key("a"),
            fields: vec!["id".into()],
        },
    ];
    for input in [JsonInput::Adjacent, JsonInput::Ndjson, JsonInput::JsonSeq] {
        let src = if input == JsonInput::JsonSeq {
            b"\x1e{\"a\":[1,2]}\n\x1e{\"a\":[3,4,5]}\n".as_slice()
        } else {
            b" {\"a\":[1,2]}\n{\"a\":[3,4,5]} \n".as_slice()
        };
        let req = common::req(input, &demands, structury_json::Dialect::Rfc8259);
        let serial = scan(src, &req).unwrap();
        let plan = Plan::build(src, &req).unwrap();
        let parts = plan
            .ranges(1)
            .iter()
            .map(|range| plan.scan(src, *range).unwrap())
            .collect();
        assert_eq!(
            common::observe_all(&stitch(parts).answers),
            common::observe_all(&serial.answers)
        );
    }
}

#[test]
fn nested_array_shards_and_matches_serial() {
    let src = common::users_array(40_000);
    let demand = Demand::Project {
        path: Path::key("users"),
        fields: vec!["id".into()],
    };
    let req = text_req(&demand);
    let serial = scan(&src, &req).expect("serial");
    let ranges = plan_ranges(&src, &req).expect("plan");
    assert!(ranges.len() > 1, "large nested array must shard: {}", ranges.len());
    let parts: Vec<_> = ranges
        .iter()
        .map(|r| common::scan_range(&src, *r, &req).expect("shard"))
        .collect();
    let stitched = stitch(parts);
    let a = common::mat_value(&serial.answers[0]).expect("serial mat");
    let b = common::mat_value(&stitched.answers[0]).expect("stitch mat");
    assert_eq!(a, b, "nested-array shards must stitch to the serial answer");
}

#[test]
fn shard_equals_serial_on_root_array() {
    let src = br#"[{"id":1},{"id":2},{"id":3},{"id":4}]"#;
    let demand = Demand::Collection {
        fields: Some(vec!["id".into()]),
        nested: None,
    };
    let req = text_req(&demand);
    let serial = scan(src, &req).expect("serial");
    let ranges = plan_ranges(src, &req).expect("plan");
    assert!(!ranges.is_empty());
    let mut parts = Vec::new();
    for range in ranges {
        parts.push(common::scan_range(src, range, &req).expect("shard"));
    }
    let stitched = stitch(parts);
    assert_eq!(stitched.answers.len(), serial.answers.len());
    let a = common::mat_value(&serial.answers[0]).expect("serial mat");
    let b = common::mat_value(&stitched.answers[0]).expect("stitch mat");
    assert_eq!(a, b);
}

#[test]
fn stitch_two_element_windows_equals_serial() {
    let src = br#"[{"id":1},{"id":2},{"id":3},{"id":4}]"#;
    let demand = Demand::Collection {
        fields: Some(vec!["id".into()]),
        nested: None,
    };
    let req = text_req(&demand);
    let serial = scan(src, &req).expect("serial");
    let left = structury::ByteRange::try_new(1, 18).expect("left");
    let right = structury::ByteRange::try_new(19, 36).expect("right");
    let stitched = stitch(vec![
        common::scan_range(src, left, &req).expect("left"),
        common::scan_range(src, right, &req).expect("right"),
    ]);
    let a = common::mat_value(&serial.answers[0]).expect("serial mat");
    let b = common::mat_value(&stitched.answers[0]).expect("stitch mat");
    assert_eq!(a, b);
}

#[test]
fn plan_is_serial_for_oracle() {
    let src = br"[1,2,3]";
    let serial = [
        Demand::Oracle(structury::Oracle::Kind),
        Demand::Oracle(structury::Oracle::DescendCount),
        Demand::Oracle(structury::Oracle::MemberNames),
        Demand::Oracle(structury::Oracle::StringByteLength),
    ];
    for demand in serial {
        let req = text_req(&demand);
        let ranges = plan_ranges(src, &req).expect("plan");
        assert_eq!(ranges.len(), 1, "{demand:?}");
        assert_eq!(ranges[0].start(), 0);
        assert_eq!(ranges[0].end(), src.len());
    }
    // A `Path` spine whose leaf is not an element count observes the located
    // node once, like a scalar path. `DescendCount` counts the array node
    // itself, so per-range sums would be off by one.
    let array = common::users_array(40_000);
    for oracle in [Oracle::Kind, Oracle::DescendCount] {
        let path = Demand::Path {
            steps: vec![Step::Key("users".into())],
            nested: Some(Box::new(Demand::Oracle(oracle.clone()))),
        };
        let req = text_req(&path);
        assert_eq!(plan_ranges(&array, &req).expect("plan").len(), 1, "{oracle:?}");
    }
}

#[test]
fn sharded_count_equals_serial() {
    let nested = Demand::Path {
        steps: vec![Step::Key("users".into())],
        nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
    };
    let mut root = Vec::from(&b"["[..]);
    for i in 0..40_000u64 {
        if i > 0 {
            root.push(b',');
        }
        root.extend_from_slice(format!("{{\"id\":{i},\"name\":\"row{i}\"}}").as_bytes());
    }
    root.push(b']');
    let mut stream = Vec::new();
    for i in 0..25_000u64 {
        stream.extend_from_slice(format!("{{\"id\":{i},\"name\":\"row{i}\"}}\n").as_bytes());
    }
    for (label, src, demand, input, want) in [
        (
            "nested",
            common::users_array(40_000),
            nested,
            JsonInput::Text,
            40_000_u64,
        ),
        ("root", root, Demand::Oracle(Oracle::Count), JsonInput::Text, 40_000),
        (
            "stream",
            stream,
            Demand::Oracle(Oracle::Count),
            JsonInput::Ndjson,
            25_000,
        ),
    ] {
        for strictness in [Strictness::Structural, Strictness::Strict] {
            let req = common::req_with(
                input,
                core::slice::from_ref(&demand),
                strictness,
                structury_json::Dialect::Rfc8259,
            );
            let serial = scan(&src, &req).expect("serial");
            let ranges = plan_ranges(&src, &req).expect("plan");
            assert!(ranges.len() > 1, "{label}: count must shard: {}", ranges.len());
            let parts: Vec<_> = ranges
                .iter()
                .map(|r| common::scan_range(&src, *r, &req).expect("shard"))
                .collect();
            let (Answer::Oracle(got), Answer::Oracle(want_serial)) = (&stitch(parts).answers[0], &serial.answers[0])
            else {
                panic!("count answers are oracle answers")
            };
            assert_eq!(got, want_serial, "{label} {strictness:?}");
            assert!(
                matches!(got, OracleAnswer::Count(n) if *n == want),
                "{label} {strictness:?}: {got:?}"
            );
        }
    }
}

#[test]
fn sharded_count_rejects_what_serial_rejects() {
    let demand = Demand::Oracle(Oracle::Count);
    let mut leading_zero = Vec::from(&b"["[..]);
    leading_zero.push(b'"');
    leading_zero.extend(core::iter::repeat_n(b'x', FIRST_SHARD_BYTES));
    leading_zero.extend_from_slice(b"\",01]");
    for (label, src, strictness) in [
        ("trailing comma", two_pack_array(b",", b"2,]"), Strictness::Structural),
        ("leading zero", leading_zero, Strictness::Strict),
    ] {
        let mut req = text_req(&demand);
        req.strictness = strictness;
        assert!(scan(&src, &req).is_err(), "serial must refuse {label}");
        let ranges = plan_ranges(&src, &req).expect("plan");
        assert!(ranges.len() > 1, "must shard: {}", ranges.len());
        let sharded: Result<Vec<_>, _> = ranges.iter().map(|r| common::scan_range(&src, *r, &req)).collect();
        assert!(sharded.is_err(), "sharded count must refuse {label}");
    }
}

#[test]
fn mixed_concat_and_sum_stitch_by_kind() {
    let src = common::users_array(40_000);
    let demands = [
        Demand::Project {
            path: Path::key("users"),
            fields: vec!["id".into()],
        },
        Demand::Path {
            steps: vec![Step::Key("users".into())],
            nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
        },
    ];
    let req = common::req_with(
        JsonInput::Text,
        &demands,
        Strictness::Structural,
        structury_json::Dialect::Rfc8259,
    );
    let serial = scan(&src, &req).expect("serial");
    let ranges = plan_ranges(&src, &req).expect("plan");
    assert!(ranges.len() > 1, "must shard: {}", ranges.len());
    let parts: Vec<_> = ranges
        .iter()
        .map(|r| common::scan_range(&src, *r, &req).expect("shard"))
        .collect();
    let stitched = stitch(parts);
    let (Answer::Columns(got), Answer::Columns(want)) = (&stitched.answers[0], &serial.answers[0]) else {
        panic!("project is a column batch")
    };
    assert_eq!(got.rows(), want.rows());
    let (Answer::Oracle(got), Answer::Oracle(want)) = (&stitched.answers[1], &serial.answers[1]) else {
        panic!("count is an oracle answer")
    };
    assert_eq!(got, want);
}

/// A comment dialect now shards: the cut scan skips `//`, `/* */`, and JSON5
/// `'…'` strings, so the array elements are located from bytes alone.
#[test]
fn comment_dialect_shards_and_matches_serial() {
    for (dialect, src) in [
        (structury_json::Dialect::Jsonc, jsonc_array(20_000)),
        (structury_json::Dialect::Json5, json5_array(20_000)),
    ] {
        let demand = Demand::Project {
            path: Path::key("users"),
            fields: vec!["id".into()],
        };
        let mut req = text_req(&demand);
        req.dialect = dialect;
        let serial =
            common::mat_value_dialect(&scan(&src, &req).expect("serial").answers[0], dialect).expect("serial mat");
        let ranges = plan_ranges(&src, &req).expect("plan");
        assert!(ranges.len() > 1, "{dialect:?} must shard: {}", ranges.len());
        let parts: Vec<_> = ranges
            .iter()
            .map(|r| common::scan_range(&src, *r, &req).expect("shard"))
            .collect();
        let got = common::mat_value_dialect(&stitch(parts).answers[0], dialect).expect("stitch mat");
        assert_eq!(got, serial, "{dialect:?} shards must stitch to the serial answer");
    }
}

/// JSONC array with a comment before the root key, comment trivia between rows
/// (including `] , " [`), and string values that look like comments.
fn jsonc_array(rows: usize) -> Vec<u8> {
    let mut src = Vec::new();
    src.extend_from_slice(b"{\n  // leading ] , \" [\n  /* block ] , \" { } */\n  \"users\": [\n");
    for i in 0..rows {
        if i > 0 {
            src.extend_from_slice(b",\n");
        }
        if i % 3 == 0 {
            src.extend_from_slice(b"    // row ] , \" /* not an opener\n");
        }
        src.extend_from_slice(
            format!(
                "    {{\"id\":{i},\"name\":\"a,b]c[{{\\\"d\\\\e\",\"note\":\"x/*not*/y\",\"score\":{}}}",
                i % 100
            )
            .as_bytes(),
        );
    }
    src.extend_from_slice(b"\n  ] /* trailing ] , */\n}\n");
    src
}

/// JSON5 array with a bare root key, a line comment, a block comment, hex
/// numbers, and single- and double-quoted strings holding structural bytes.
fn json5_array(rows: usize) -> Vec<u8> {
    let mut src = Vec::new();
    src.extend_from_slice(b"// leading ] , \"\n{users:[\n");
    for i in 0..rows {
        if i > 0 {
            src.extend_from_slice(b",\n");
        }
        if i % 3 == 0 {
            src.extend_from_slice(b"  /* row ] , \" */ ");
        }
        src.extend_from_slice(
            format!(
                "{{id:{i},name:'a,b]c[{{d \"e\" //f',note:\"it's // fine\",score:0x{:x}}}",
                i % 100
            )
            .as_bytes(),
        );
    }
    src.extend_from_slice(b"\n]/* trailing ] , */}\n");
    src
}

fn text_req(demand: &Demand) -> ScanRequest<'_> {
    common::req_with(
        JsonInput::Text,
        core::slice::from_ref(demand),
        Strictness::Structural,
        structury_json::Dialect::Rfc8259,
    )
}

fn two_pack_array(between: &[u8], last: &[u8]) -> Vec<u8> {
    let mut src = Vec::from(&b"[\""[..]);
    src.extend(core::iter::repeat_n(b'x', FIRST_SHARD_BYTES));
    src.push(b'"');
    src.extend_from_slice(between);
    src.extend_from_slice(last);
    src
}

#[test]
fn sharded_rfc_refusals_match_serial() {
    for (separator, tail, label) in [
        (&b","[..], &b"2,]"[..], "trailing comma"),
        (&b",,"[..], &b"2]"[..], "double comma on a pack boundary"),
    ] {
        let src = two_pack_array(separator, tail);
        let demand = common::project_root(&["id"]);
        let req = text_req(&demand);
        assert!(scan(&src, &req).is_err(), "serial must refuse RFC {label}");
        let ranges = plan_ranges(&src, &req).expect("plan");
        assert!(ranges.len() > 1, "must shard: {}", ranges.len());
        let sharded: Result<Vec<_>, _> = ranges.iter().map(|r| common::scan_range(&src, *r, &req)).collect();
        assert!(sharded.is_err(), "sharded must refuse RFC {label}");
    }
}

/// The serial planner keeps the lexer's refusal on malformed input. The
/// serial cut drives one `scan_block`, so an unterminated comment or string, or
/// a `/` that starts no comment, must be raised by `element_ranges` rather than
/// absorbed into the ranges.
#[test]
fn malformed_array_plan_keeps_the_lexer_error() {
    let demand = Demand::Project {
        path: Path::key("users"),
        fields: vec!["id".into()],
    };
    let req = text_req(&demand);
    for (src, code) in [
        (&b"{\"users\":[1, /2]}"[..], "invalid-comment"),
        (b"{\"users\":[1, /* 2]}", "unterminated-comment"),
        (b"{\"users\":[\"abc]}", "unterminated-string"),
    ] {
        let err = plan_ranges(src, &req).expect_err("planner must refuse");
        assert_eq!(err.code(), code, "plan_ranges {src:?}");
        let err = Plan::build(src, &req).expect_err("index must refuse");
        assert_eq!(err.code(), code, "Plan::build {src:?}");
    }
}

#[test]
fn sharded_stream_applies_the_absent_row_law_to_a_scalar_record() {
    // A scalar record under a flat projection is an absent row, exactly as the
    // serial stream fold has it; the per-value window route must match.
    let demands = [Demand::Project {
        path: Path::root(),
        fields: vec!["a".into()],
    }];
    let src = b"7\n{\"a\":1}\n";
    let req = ScanRequest::new(JsonInput::Ndjson, &demands);
    let serial = scan(src, &req).expect("serial");
    let plan = Plan::build(src, &req).expect("plan");
    // A tiny stream is one planned morsel; scan the two record ranges directly
    // to exercise the per-value window route.
    let first = ByteRange::try_new(0, 1).expect("range");
    let second = ByteRange::try_new(2, 9).expect("range");
    let parts = vec![
        plan.scan(src, first).expect("first"),
        plan.scan(src, second).expect("second"),
    ];
    let sharded = stitch(parts);
    assert_eq!(
        common::observe_all(&sharded.answers),
        common::observe_all(&serial.answers)
    );
}

#[test]
fn from_summaries_declines_a_serial_demand() {
    let src = br#"{"rows":[1,2,3]}"#;
    let demand = Demand::Path {
        steps: vec![Step::Key("rows".into())],
        nested: Some(Box::new(Demand::Oracle(Oracle::DescendCount))),
    };
    let req = text_req(&demand);
    let serial = scan(src, &req).expect("serial");
    let start = src.iter().position(|&b| b == b'[').expect("[");
    let cut = Plan::cut(src, ByteRange::try_new(start, src.len()).expect("range"));
    let plan = Plan::from_summaries(src, start, &[cut], &req);
    let ranges = plan.ranges(64);
    assert_eq!(ranges.len(), 1, "a serial demand is one whole range");
    let out = plan.scan(src, ranges[0]).expect("scan");
    assert_eq!(common::observe_all(&out.answers), common::observe_all(&serial.answers));
}

#[test]
fn from_summaries_falls_back_when_the_tail_is_invalid() {
    let src = b"[1,2] trailing";
    let demand = Demand::Oracle(Oracle::Count);
    let req = text_req(&demand);
    let cut = Plan::cut(src, ByteRange::try_new(0, src.len()).expect("range"));
    let plan = Plan::from_summaries(src, 0, &[cut], &req);
    let ranges = plan.ranges(64);
    assert_eq!(ranges.len(), 1, "the invalid tail forces one whole range");
    assert!(
        plan.scan(src, ranges[0]).is_err(),
        "the whole-range scan validates the tail"
    );
    assert!(Plan::build(src, &req).is_err());
}

#[test]
fn scan_controlled_stops_a_part() {
    fn one() -> u64 {
        1
    }

    let src = b"[1,2,3,4,5]";
    let demand = Demand::Oracle(Oracle::Count);
    let req = text_req(&demand);
    let plan = Plan::build(src, &req).expect("plan");
    let control = Control::new(u64::MAX, Some(1), one);
    let ranges = plan.ranges(1);
    assert!(ranges.len() > 1, "must shard: {}", ranges.len());
    let error = plan.scan_controlled(src, ranges[1], &control).expect_err("stop");
    assert_eq!(error.class(), ErrorClass::Control);
}

fn numeric(spelling: &str) -> structury::Value {
    structury::Value::Number(structury::Number::parse(spelling).expect("a number spelling"))
}

fn eq_a_one() -> structury::Predicate {
    structury::Predicate::Eq {
        field: "a".into(),
        value: numeric("1"),
    }
}

/// A non-object element under a flat row view follows the element law in the
/// plan route: one absent row for `Project`, nothing for `Filter`. The per-value
/// route walks each element as a root, so an array element used to answer per
/// nested element instead of once.
#[test]
fn non_object_elements_follow_the_element_law_in_windows() {
    fn zero() -> u64 {
        0
    }

    let cases: &[(&[u8], JsonInput, Demand)] = &[
        (
            br#"{"rows":[{"a":1},[]]}"#,
            JsonInput::Text,
            Demand::Project {
                path: Path::key("rows"),
                fields: vec!["a".into()],
            },
        ),
        (
            br#"{"rows":[[1,2],[3],[]]}"#,
            JsonInput::Text,
            Demand::Project {
                path: Path::key("rows"),
                fields: vec!["a".into()],
            },
        ),
        (
            br#"{"rows":[{"a":1},[2,3],{"a":2}]}"#,
            JsonInput::Text,
            Demand::Filter {
                path: Path::key("rows"),
                predicate: eq_a_one(),
                project: vec!["a".into()],
            },
        ),
        (
            b"{\"a\":1}\n[1,2,3]\n",
            JsonInput::Ndjson,
            Demand::Project {
                path: Path::root(),
                fields: vec!["a".into()],
            },
        ),
        (
            b"{\"a\":1}\n[1,2,3]\n",
            JsonInput::Ndjson,
            Demand::Filter {
                path: Path::root(),
                predicate: eq_a_one(),
                project: vec!["a".into()],
            },
        ),
    ];

    for (src, input, demand) in cases {
        let demands = [demand.clone()];
        let req = ScanRequest::new(*input, &demands);
        let serial = scan(src, &req).expect("serial");
        let plan = Plan::build(src, &req).expect("plan");
        let parts: Vec<_> = plan
            .ranges(1)
            .iter()
            .map(|range| plan.scan(src, *range).expect("part"))
            .collect();
        assert_eq!(
            common::observe_all(&stitch(parts).answers),
            common::observe_all(&serial.answers),
            "uncontrolled {src:?}"
        );

        let control = Control::new(u64::MAX, None, zero);
        let parts: Vec<_> = plan
            .ranges(1)
            .iter()
            .map(|range| plan.scan_controlled(src, *range, &control).expect("part"))
            .collect();
        assert_eq!(
            common::observe_all(&stitch(parts).answers),
            common::observe_all(&serial.answers),
            "controlled {src:?}"
        );
    }
}

/// A flat filter that matches nothing answers the empty batch, not `Missing`,
/// on the element-run route too.
#[test]
fn a_filter_that_matches_nothing_answers_an_empty_batch() {
    let src = br#"{"rows":[1,2,3]}"#;
    let demand = Demand::Filter {
        path: Path::key("rows"),
        predicate: eq_a_one(),
        project: Vec::new(),
    };
    let req = text_req(&demand);
    let serial = scan(src, &req).expect("serial");
    let plan = Plan::build(src, &req).expect("plan");
    let parts: Vec<_> = plan
        .ranges(1)
        .iter()
        .map(|range| plan.scan(src, *range).expect("part"))
        .collect();
    let sharded = stitch(parts);
    assert!(
        matches!(&sharded.answers[0], Answer::Columns(columns) if columns.rows() == 0),
        "an empty batch, not Missing: {:?}",
        sharded.answers[0]
    );
    assert_eq!(
        common::observe_all(&sharded.answers),
        common::observe_all(&serial.answers)
    );
}

/// An empty plan range answers one empty batch per demand, and a range past the
/// source is a shape refusal rather than a panic.
#[test]
fn degenerate_plan_ranges_are_answers_or_refusals() {
    fn zero() -> u64 {
        0
    }

    let src = br#"{"rows":[1,2,3]}"#;
    let demand = Demand::Filter {
        path: Path::key("rows"),
        predicate: eq_a_one(),
        project: Vec::new(),
    };
    let req = text_req(&demand);
    let plan = Plan::build(src, &req).expect("plan");

    let empty = ByteRange::try_new(0, 0).expect("ordered");
    let out = plan
        .scan_controlled(src, empty, &Control::new(u64::MAX, None, zero))
        .expect("an empty range still answers");
    assert_eq!(out.answers.len(), 1, "one answer per demand");
    assert!(matches!(&out.answers[0], Answer::Columns(columns) if columns.rows() == 0));

    let past = ByteRange::try_new(src.len() + 1, src.len() + 4).expect("ordered");
    let error = plan.scan(src, past).expect_err("past the source");
    assert_eq!(error.class(), ErrorClass::Shape);
}

fn mk(demands: &[Demand], dialect: Dialect) -> ScanRequest<'_> {
    ScanRequest::new(JsonInput::Text, demands).with_dialect(dialect)
}

#[derive(Debug, PartialEq, Eq)]
enum Door {
    Ok(String),
    Err(String),
}

fn serial_answer(src: &[u8], demands: &[Demand], dialect: Dialect) -> Door {
    match scan(src, &mk(demands, dialect)) {
        Ok(result) => Door::Ok(format!("{:?}", observe_all(&result.answers))),
        Err(error) => Door::Err(format!("{error:?}")),
    }
}

fn plan_answer(src: &[u8], demands: &[Demand], dialect: Dialect) -> (usize, Door) {
    let request = mk(demands, dialect);
    let built = match Plan::build(src, &request) {
        Ok(plan) => plan,
        Err(error) => return (0, Door::Err(format!("{error:?}"))),
    };
    let ranges = built.ranges(1);
    let mut parts = Vec::new();
    for range in &ranges {
        match common::scan_range(src, *range, &request) {
            Ok(part) => parts.push(part),
            Err(error) => return (ranges.len(), Door::Err(format!("{error:?}"))),
        }
    }
    (
        ranges.len(),
        Door::Ok(format!("{:?}", observe_all(&stitch(parts).answers))),
    )
}

/// Every top-level `Demand` variant, including all seven `Oracle` arms.
fn leaves() -> Vec<(&'static str, Demand)> {
    vec![
        ("whole", Demand::Whole),
        (
            "path[]",
            Demand::Path {
                steps: vec![],
                nested: None,
            },
        ),
        (
            "path[a]",
            Demand::Path {
                steps: vec![Step::Key("a".into())],
                nested: None,
            },
        ),
        (
            "proj[]",
            Demand::Project {
                path: Path::root(),
                fields: vec!["x".into()],
            },
        ),
        (
            "proj[a]",
            Demand::Project {
                path: Path::key("a"),
                fields: vec!["x".into()],
            },
        ),
        (
            "filt[]",
            Demand::Filter {
                path: Path::root(),
                predicate: Predicate::Gt {
                    field: "x".into(),
                    value: num("0"),
                },
                project: vec!["x".into()],
            },
        ),
        ("oracle-count", Demand::Oracle(Oracle::Count)),
        ("oracle-descend", Demand::Oracle(Oracle::DescendCount)),
        ("oracle-kind", Demand::Oracle(Oracle::Kind)),
        ("oracle-haskey", Demand::Oracle(Oracle::HasKey { key: "a".into() })),
        ("oracle-names", Demand::Oracle(Oracle::MemberNames)),
        ("oracle-mcount", Demand::Oracle(Oracle::MemberCount)),
        ("oracle-sblen", Demand::Oracle(Oracle::StringByteLength)),
    ]
}

/// A named wrapper that applies a nested demand.
type Wrapper = (&'static str, fn(Demand) -> Demand);

/// Wrappers that apply a nested demand: spent spines, keyed/indexed spines,
/// collections, slices. `fn` pointers so the sequence can be rebuilt per depth.
fn wrappers() -> Vec<Wrapper> {
    vec![
        ("spent[]", |d| Demand::Path {
            steps: vec![],
            nested: Some(Box::new(d)),
        }),
        ("path[a]", |d| Demand::Path {
            steps: vec![Step::Key("a".into())],
            nested: Some(Box::new(d)),
        }),
        ("index0", |d| Demand::Path {
            steps: vec![Step::Index(0)],
            nested: Some(Box::new(d)),
        }),
        ("index-1", |d| Demand::Path {
            steps: vec![Step::Index(-1)],
            nested: Some(Box::new(d)),
        }),
        ("coll", |d| Demand::Collection {
            fields: None,
            nested: Some(Box::new(d)),
        }),
        ("collkeys", |d| Demand::Collection {
            fields: Some(vec!["x".into()]),
            nested: Some(Box::new(d)),
        }),
        ("slice", |d| Demand::Slice {
            range: Range { start: None, end: None },
            nested: Some(Box::new(d)),
        }),
        ("slice02", |d| Demand::Slice {
            range: Range {
                start: Some(0),
                end: Some(2),
            },
            nested: Some(Box::new(d)),
        }),
    ]
}

/// Every demand built from a leaf under `depth` wrappers, depth 0..=3.
fn generated() -> Vec<(String, Demand)> {
    let mut out = Vec::new();
    for (leaf_name, leaf) in leaves() {
        for depth in 0..=3 {
            grow(&mut out, leaf_name, &leaf, depth);
        }
    }
    out
}

fn grow(out: &mut Vec<(String, Demand)>, name: &str, demand: &Demand, depth: usize) {
    if depth == 0 {
        out.push((name.to_string(), demand.clone()));
        return;
    }
    for (wrapper_name, wrapper) in wrappers() {
        grow(
            out,
            &format!("{wrapper_name}({name})"),
            &wrapper(demand.clone()),
            depth - 1,
        );
    }
}

/// Sources with a root array of >= 2 elements, an array under a key, a root of
/// arrays, and a non-array root.
const SRC_OBJECTS: &[u8] = br#"[{"a":[{"x":1},{"x":2}],"b":[{"x":3}],"n":1},{"a":[{"x":4}],"b":[{"x":5},{"x":6}],"n":0},{"a":[],"b":[{"x":7}],"n":2}]"#;
const SRC_ARRAYS: &[u8] = br#"[[{"x":1},{"x":2}],5,[{"x":3}],{"a":[{"x":4}]}]"#;
const SRC_OBJECT_ROOT: &[u8] = br#"{"a":[{"x":1},{"x":2}],"b":[{"x":3}]}"#;

#[derive(Debug, Clone, Copy)]
enum Source {
    Objects,
    Arrays,
    ObjectRoot,
}

impl Source {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Objects => SRC_OBJECTS,
            Self::Arrays => SRC_ARRAYS,
            Self::ObjectRoot => SRC_OBJECT_ROOT,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Objects => "objects",
            Self::Arrays => "arrays",
            Self::ObjectRoot => "object-root",
        }
    }
}

/// A shape the planner admits (>1 morsel) must fold byte-for-byte to serial;
/// a shape it refuses must plan one range. Any other row is a divergence.
struct Divergence {
    source: &'static str,
    dialect: &'static str,
    demand: String,
    shard: String,
    eligible: bool,
    morsels: usize,
    serial: Door,
    plan: Door,
}

#[test]
fn generated_depth3_matrix_is_closed() {
    let shapes = generated();
    println!("generated {} demand shapes", shapes.len());
    let mut divergences: Vec<Divergence> = Vec::new();
    let mut admitted = 0usize;
    let mut refused = 0usize;
    let mut both_err_different = 0usize;
    let mut ok_vs_err = 0usize;
    for source in [Source::Objects, Source::Arrays, Source::ObjectRoot] {
        let src = source.bytes();
        for dialect in [Dialect::Rfc8259, Dialect::Jsonc, Dialect::Json5] {
            for (name, demand) in &shapes {
                let demand = core::slice::from_ref(demand);
                let (parts, plan_door) = plan_answer(src, demand, dialect);
                let serial_door = serial_answer(src, demand, dialect);
                let eligible = structury::Drive::eligible(demand);
                let shard = format!("{:?}", demand[0].shard());
                if parts > 1 {
                    admitted += 1;
                } else {
                    refused += 1;
                }
                let agreed = plan_door == serial_door;
                if !agreed {
                    match (&serial_door, &plan_door) {
                        (Door::Err(_), Door::Err(_)) => both_err_different += 1,
                        (Door::Err(_), Door::Ok(_)) | (Door::Ok(_), Door::Err(_)) => ok_vs_err += 1,
                        (Door::Ok(_), Door::Ok(_)) => {}
                    }
                }
                let serial_shard = if parts > 1 { !agreed } else { false };
                if serial_shard || (parts == 0 && matches!(serial_door, Door::Ok(_))) {
                    divergences.push(Divergence {
                        source: source.name(),
                        dialect: match dialect {
                            Dialect::Rfc8259 => "rfc",
                            Dialect::Jsonc => "jsonc",
                            Dialect::Json5 => "json5",
                            _ => "other",
                        },
                        demand: name.clone(),
                        shard,
                        eligible,
                        morsels: parts,
                        serial: serial_door,
                        plan: plan_door,
                    });
                }
            }
        }
    }
    println!("admitted morsel rows: {admitted}, refused one-range rows: {refused}");
    let mut by_shard: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut by_head: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for d in &divergences {
        *by_shard.entry(d.shard.clone()).or_default() += 1;
        *by_head
            .entry(d.demand.split('(').next().unwrap_or("").to_string())
            .or_default() += 1;
        names.insert(d.demand.clone());
    }
    println!(
        "divergences: {} ({} unique demand spellings)",
        divergences.len(),
        names.len()
    );
    println!("  both-doors-error-differently: {both_err_different}, one-ok-one-err: {ok_vs_err}");
    println!("  by shard: {by_shard:?}");
    println!("  by top-level head: {by_head:?}");
    for d in divergences.iter().take(60) {
        println!(
            "DIVERGE [{}/{}] {}\n    shard={} eligible={} morsels={}\n    serial={:?}\n    plan  ={:?}",
            d.source, d.dialect, d.demand, d.shard, d.eligible, d.morsels, d.serial, d.plan
        );
    }
    assert!(
        divergences.is_empty(),
        "{} generated shapes admit a plan whose fold differs from serial",
        divergences.len()
    );
}

/// Spent-path edge shapes: the fold kind, the morsel count, and the two doors.
/// Every row the law calls `Serial` must plan one range; every parallel row must
/// fold to serial.
#[test]
fn spent_path_normalisation_edges() {
    let src = SRC_OBJECTS;
    let cases: Vec<(&str, Demand)> = vec![
        // A spent spine with no nested demand: not the normalising arm.
        (
            "spent-no-nested",
            Demand::Path {
                steps: vec![],
                nested: None,
            },
        ),
        ("spent-of-spent-no-nested", spent_path(spent_no_nested())),
        // `Collection{spent-not-nested}` is a bare whole-element row.
        ("coll-spent-no-nested", collection(None, Some(spent_no_nested()))),
        // `Path{[]}` whose nested is itself `Path{[]}`.
        ("spent-spent-project", spent_path(spent_path(project_root(&["x"])))),
        (
            "coll-spent-spent-project",
            collection(None, Some(spent_path(spent_path(project_root(&["x"]))))),
        ),
        // `Path{[] -> Collection{fields:Some, nested:Some(_)}}`.
        (
            "spent-coll-keys-project",
            spent_path(collection(Some(&["x"]), Some(project_root(&["x"])))),
        ),
        ("spent-coll-keys-none", spent_path(collection(Some(&["x"]), None))),
        // `Path{[] -> Slice}`.
        ("spent-slice", spent_path(slice(None, None))),
        (
            "spent-slice-project",
            spent_path(slice_nested(None, None, project_root(&["x"]))),
        ),
        (
            "coll-spent-slice-project",
            collection(None, Some(spent_path(slice_nested(None, None, project_root(&["x"]))))),
        ),
        // `Slice{nested: Some(Serial leaf)}`.
        ("slice-oracle", slice_nested(None, None, Demand::Oracle(Oracle::Count))),
        (
            "coll-slice-oracle",
            collection(None, Some(slice_nested(None, None, Demand::Oracle(Oracle::Count)))),
        ),
        // `Collection{fields:Some, nested:Some(Serial leaf)}`.
        ("coll-keys-slice", collection(Some(&["x"]), Some(slice(None, None)))),
        (
            "coll-keys-oracle",
            collection(Some(&["x"]), Some(Demand::Oracle(Oracle::Kind))),
        ),
        // `Oracle` nested inside `Slice` vs inside `Collection`.
        (
            "slice-oracle-kind",
            slice_nested(None, None, Demand::Oracle(Oracle::Kind)),
        ),
        ("coll-oracle-kind", collection(None, Some(Demand::Oracle(Oracle::Kind)))),
    ];
    let mut bad = Vec::new();
    for (name, demand) in &cases {
        let one = core::slice::from_ref(demand);
        let parts = morsels(src, one, Dialect::Rfc8259);
        let serial_door = serial_answer(src, one, Dialect::Rfc8259);
        let (plan_parts, plan_door) = plan_answer(src, one, Dialect::Rfc8259);
        let shard = demand.shard();
        let eligible = structury::Drive::eligible(one);
        println!(
            "{name:28} shard={shard:?} eligible={eligible} morsels={parts}/{plan_parts} serial={serial_door:?} plan={plan_door:?}"
        );
        // The law: a Serial demand is never admitted to a multi-morsel plan.
        if !shard.is_parallel() {
            if parts > 1 {
                bad.push(format!("{name}: Serial law admitted {parts} morsels"));
            }
        } else if plan_door != serial_door {
            bad.push(format!("{name}: parallel fold differs from serial"));
        }
    }
    assert!(bad.is_empty(), "spent-path edges:\n  {}", bad.join("\n  "));
}

fn spent_no_nested() -> Demand {
    Demand::Path {
        steps: vec![],
        nested: None,
    }
}

/// A direct nested `Collection{Collection{...}}` is over arrays and non-arrays: the plan and serial doors must agree.
#[test]
fn nested_collection_over_non_array_element() {
    let cases: &[(&str, &[u8], Demand)] = &[
        (
            "coll-coll-proj over objects",
            br#"[{"x":1},{"x":2}]"#,
            collection(None, Some(collection(None, Some(project_root(&["x"]))))),
        ),
        (
            "coll-coll-proj over arrays",
            br#"[[{"x":1}],[{"x":2},{"x":3}]]"#,
            collection(None, Some(collection(None, Some(project_root(&["x"]))))),
        ),
        (
            "coll-coll-proj-over-spent over objects",
            br#"[{"x":1},{"x":2}]"#,
            collection(None, Some(collection(None, Some(spent_path(project_root(&["x"])))))),
        ),
        (
            "coll-spent-coll-proj over objects",
            br#"[{"x":1},{"x":2}]"#,
            collection(None, Some(spent_path(collection(None, Some(project_root(&["x"])))))),
        ),
        (
            "coll-coll-none over arrays",
            br"[[1,2],[3]]",
            collection(None, Some(collection(None, None))),
        ),
    ];
    let mut bad = Vec::new();
    for (name, src, demand) in cases {
        let one = core::slice::from_ref(demand);
        let parts = morsels(src, one, Dialect::Rfc8259);
        let serial_door = serial_answer(src, one, Dialect::Rfc8259);
        let (plan_parts, plan_door) = plan_answer(src, one, Dialect::Rfc8259);
        println!(
            "{name:44} shard={:?} eligible={} morsels={parts}/{plan_parts}\n    serial={serial_door:?}\n    plan  ={plan_door:?}",
            demand.shard(),
            structury::Drive::eligible(one)
        );
        if plan_door != serial_door {
            bad.push(format!("{name}: serial={serial_door:?} plan={plan_door:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "nested Collection over a non-array element:\n  {}",
        bad.join("\n  ")
    );
}

fn big_objects(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * 16);
    out.push(b'[');
    for i in 0..n {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(format!(r#"{{"id":{i},"n":{}}}"#, i % 5).as_bytes());
    }
    out.push(b']');
    out
}

/// The admitted direct spelling keeps its parallelism; the spent spelling is
/// refused (`Serial`, one range), so no admitted lane loses work.
#[test]
fn direct_collection_spelling_keeps_parallelism() {
    let src = big_objects(4000);
    let direct = collection(None, Some(project_root(&["id"])));
    let spent_direct = collection(None, Some(spent_path(project_root(&["id"]))));
    let direct_parts = morsels(&src, core::slice::from_ref(&direct), Dialect::Rfc8259);
    let spent_parts = morsels(&src, core::slice::from_ref(&spent_direct), Dialect::Rfc8259);
    println!("Collection{{Project}} parts={direct_parts}");
    println!("Collection{{spent Path{{[]}}->Project}} parts={spent_parts}");
    assert!(direct_parts >= 4000, "expected >=4000 morsels, got {direct_parts}");
    assert_eq!(
        core::slice::from_ref(&direct).first().unwrap().shard(),
        structury::Shard::Concat
    );
    assert_eq!(
        spent_direct.shard(),
        structury::Shard::Serial,
        "the spent spine is not admitted to the plan"
    );
    assert_eq!(spent_parts, 1, "the refused spent spelling plans one range");

    // A keyed spine to a nested array is a correctly shardable shape the plan
    // declines; record its morsel count.
    let keyed = Demand::Path {
        steps: vec![Step::Key("rows".into())],
        nested: Some(Box::new(project_root(&["id"]))),
    };
    let keyed_src = {
        let mut s = Vec::new();
        s.extend_from_slice(br#"{"rows":"#);
        s.extend_from_slice(&big_objects(4000));
        s.extend_from_slice(br"}");
        s
    };
    println!(
        "Path{{[rows]->Project}} shard={:?} eligible={} parts={}",
        keyed.shard(),
        structury::Drive::eligible(core::slice::from_ref(&keyed)),
        morsels(&keyed_src, core::slice::from_ref(&keyed), Dialect::Rfc8259)
    );
}

/// `Project{path:[rows]}` and
/// `Path{[rows]→Project{[]}}` answer identically at the serial door, and the
/// spine spelling *would* fold correctly over the canonical plan's ranges, but
/// the plan refuses it (`Serial`, one range). The canonical spelling keeps the
/// parallel path; this is the parallelism cost of the admitted set, not a
/// correctness gap.
#[test]
fn refused_path_spine_would_fold_correctly() {
    let body = big_objects(4000);
    let mut src = Vec::new();
    src.extend_from_slice(br#"{"rows":"#);
    src.extend_from_slice(&body);
    src.extend_from_slice(br"}");

    let canonical = Demand::Project {
        path: Path::key("rows"),
        fields: vec!["id".into()],
    };
    let spine = Demand::Path {
        steps: vec![Step::Key("rows".into())],
        nested: Some(Box::new(project_root(&["id"]))),
    };
    let canonical_parts = morsels(&src, core::slice::from_ref(&canonical), Dialect::Rfc8259);
    let spine_parts = morsels(&src, core::slice::from_ref(&spine), Dialect::Rfc8259);
    // Both spellings answer the same at the serial door.
    assert_eq!(
        serial_answer(&src, core::slice::from_ref(&canonical), Dialect::Rfc8259),
        serial_answer(&src, core::slice::from_ref(&spine), Dialect::Rfc8259),
        "the two spellings must answer the same"
    );
    println!("canonical Project{{path:[rows]}} parts={canonical_parts}");
    println!(
        "spine Path{{[rows]->Project}}   parts={spine_parts} shard={:?}",
        spine.shard()
    );
    // The spine shape's own per-morsel mapping folds correctly against the
    // canonical plan's ranges; the plan refuses it because its
    // cutting/normalisation is not proven, so it plans one range.
    let canonical_plan = Plan::build(&src, &mk(core::slice::from_ref(&canonical), Dialect::Rfc8259)).expect("plan");
    let pairs: Vec<_> = canonical_plan
        .ranges(1)
        .iter()
        .map(|range| {
            common::scan_range(&src, *range, &mk(core::slice::from_ref(&spine), Dialect::Rfc8259)).expect("scan")
        })
        .collect();
    let folded = format!("{:?}", observe_all(&stitch(pairs).answers));
    let serial_spine = match serial_answer(&src, core::slice::from_ref(&spine), Dialect::Rfc8259) {
        Door::Ok(text) => text,
        Door::Err(text) => panic!("serial spine errored: {text}"),
    };
    assert_eq!(
        folded, serial_spine,
        "the refused shape would fold correctly if sharded"
    );
    assert_eq!(spine.shard(), Shard::Serial, "the spine spelling is not admitted");
    assert_eq!(spine_parts, 1, "a refused shape plans one whole range");
    assert!(
        canonical_parts >= 4000,
        "the canonical spelling keeps the parallel path"
    );
}

/// Shapes the plan refuses: each plans one whole range and the serial door
/// answers alone.
#[test]
fn refused_shapes_plan_one_range() {
    let cases: &[(&str, &[u8], Demand)] = &[
        (
            "nested Collection collapse",
            br#"[{"x":1},{"x":2}]"#,
            collection(None, Some(collection(None, Some(project_root(&["x"]))))),
        ),
        (
            "Collection path absent row",
            br#"[{"a":[]},{"a":[]}]"#,
            collection(
                None,
                Some(Demand::Project {
                    path: Path::key("a"),
                    fields: vec!["x".into()],
                }),
            ),
        ),
        (
            "spent path spine Count",
            br#"[10,{"a":1}]"#,
            Demand::Path {
                steps: vec![],
                nested: Some(Box::new(Demand::Path {
                    steps: vec![Step::Index(-1)],
                    nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
                })),
            },
        ),
    ];
    for (name, src, demand) in cases {
        let one = core::slice::from_ref(demand);
        let (parts, plan_door) = plan_answer(src, one, Dialect::Rfc8259);
        let serial_door = serial_answer(src, one, Dialect::Rfc8259);
        assert_eq!(demand.shard(), Shard::Serial, "{name} is not admitted");
        assert_eq!(parts, 1, "{name} plans one whole range");
        assert_eq!(plan_door, serial_door, "{name}: the serial door is the only answer");
    }
}

/// A `Collection` applied to a non-array element, and
/// `Collection{fields:Some, nested:Some}`: what each door does.
#[test]
fn unadmitted_shapes_agree_across_doors() {
    let cases: &[(&str, &[u8], Demand)] = &[
        (
            "collection over non-array root",
            br#"{"a":1}"#,
            collection(None, Some(project_root(&["x"]))),
        ),
        (
            "collection over root array of scalars",
            br"[1,2,3]",
            collection(None, Some(project_root(&["x"]))),
        ),
        (
            "keys+nested=Some project",
            br#"[{"x":1,"y":2},{"x":3}]"#,
            collection(Some(&["y"]), Some(project_root(&["x"]))),
        ),
        (
            "keys+nested=Some slice",
            br#"[{"x":1,"y":2},{"x":3}]"#,
            collection(Some(&["y"]), Some(slice(None, None))),
        ),
        (
            "keys+nested=Some coll",
            br#"[{"x":1,"y":2},{"x":3}]"#,
            collection(Some(&["y"]), Some(collection(None, Some(project_root(&["x"]))))),
        ),
    ];
    for (name, src, demand) in cases {
        let one = core::slice::from_ref(demand);
        let parts = morsels(src, one, Dialect::Rfc8259);
        let serial_door = serial_answer(src, one, Dialect::Rfc8259);
        let (plan_parts, plan_door) = plan_answer(src, one, Dialect::Rfc8259);
        println!(
            "{name:40} shard={:?} eligible={} morsels={parts}/{plan_parts}\n    serial={serial_door:?}\n    plan  ={plan_door:?}",
            demand.shard(),
            structury::Drive::eligible(one)
        );
        if plan_parts > 1 {
            assert_eq!(plan_door, serial_door, "{name}: fold differs from serial");
        }
    }
}

#[test]
fn slice_reachability_every_path() {
    let text = br#"[{"id":1,"n":1},{"id":2,"n":0},{"id":3,"n":2},{"id":4,"n":0}]"#;
    let root_slice = slice_nested(None, None, project_root(&["id"]));
    let cases: Vec<(&str, Demand)> = vec![
        ("slice", root_slice.clone()),
        (
            "path-[rows]-slice",
            Demand::Path {
                steps: vec![Step::Key("rows".into())],
                nested: Some(Box::new(root_slice.clone())),
            },
        ),
        ("collection-slice", collection(None, Some(root_slice.clone()))),
        (
            "collection-keys-slice",
            collection(Some(&["id"]), Some(root_slice.clone())),
        ),
        (
            "collection-collection-slice",
            collection(None, Some(collection(None, Some(root_slice.clone())))),
        ),
        ("spent-path-slice", spent_path(root_slice.clone())),
        (
            "path-collection-slice",
            Demand::Path {
                steps: vec![Step::Key("rows".into())],
                nested: Some(Box::new(collection(None, Some(root_slice.clone())))),
            },
        ),
        ("slice-slice", slice_nested(Some(0), Some(2), root_slice.clone())),
        (
            "collection-spent-path-slice",
            collection(None, Some(spent_path(root_slice.clone()))),
        ),
    ];
    for (name, demand) in &cases {
        let shard = demand.shard();
        let m = morsels(text, core::slice::from_ref(demand), Dialect::Rfc8259);
        let drive = structury::Drive::eligible(core::slice::from_ref(demand));
        let tag = sharded(text, core::slice::from_ref(demand), Dialect::Rfc8259);
        println!("{name:34} shard={shard:?} drive_eligible={drive} morsels={m} tag={tag}");
        assert!(!drive, "{name}: a Serial Slice shape is eligible");
        assert_eq!(m, 1, "{name}: a Slice reached a multi-morsel plan");
        assert_eq!(tag, "AGREE", "{name}: a Slice shape diverges");
    }
}

#[test]
fn slice_nested_in_collection_matches_serial_plan() {
    // Root array of arrays, each one object; a nested Slice windows the inner array.
    let src = big_arrays_of_objects(4000);
    let inner = slice_nested(Some(0), Some(1), project_root(&["id"]));
    let demand = collection(None, Some(inner));
    let tag = sharded(&src, core::slice::from_ref(&demand), Dialect::Rfc8259);
    println!("collection{{slice(0,1)}} over arrays: {tag}");
    assert_eq!(tag, "AGREE", "Collection{{nested:Slice}} serial vs plan");

    // Root array of *objects*: a nested Slice on an object element is a
    // TypeMismatch the serial row law drops, so the two doors disagree.
    let objs = big_objects(4);
    let tag = sharded(&objs, core::slice::from_ref(&demand), Dialect::Rfc8259);
    println!("collection{{slice(0,1)}} over objects: {tag}");
}

#[test]
fn slice_on_a_stream_is_serial_only() {
    let src = b"{\"n\":1}\n{\"n\":0}\n{\"n\":2}\n{\"n\":3}\n";
    let demand = slice_nested(Some(1), Some(3), filter_root(gt("n", "0"), &["n"]));
    let request = ScanRequest::new(JsonInput::Ndjson, core::slice::from_ref(&demand));
    let serial = scan(src, &request).map(|r| format!("{:?}", observe_all(&r.answers)));
    let plan_ranges = Plan::build(src, &request).map_or(0, |p| p.ranges(1).len());
    println!("ndjson slice plan ranges={plan_ranges} serial={serial:?}");
    assert_eq!(plan_ranges, 1, "a Slice over a stream must not shard");
}

/// A `Slice` is `Shard::Serial` (its window is global), so a multi-morsel plan
/// delegates to the serial walk and answers the window, not a per-morsel fold.
#[test]
fn slice_demand_is_serial_in_a_sharded_plan() {
    let mut base = Vec::new();
    base.push(b'[');
    for i in 0..120_000u32 {
        if i > 0 {
            base.push(b',');
        }
        base.extend_from_slice(format!(r#"{{"id":{i}}}"#).as_bytes());
    }
    base.push(b']');
    let demands: Vec<(&str, Demand)> = vec![
        (
            "slice(0,3)",
            Demand::Slice {
                range: Range {
                    start: Some(0),
                    end: Some(3),
                },
                nested: None,
            },
        ),
        (
            "slice(0,3)+id",
            Demand::Slice {
                range: Range {
                    start: Some(0),
                    end: Some(3),
                },
                nested: Some(Box::new(project_root(&["id"]))),
            },
        ),
    ];
    for (name, demand) in demands {
        let tag = sharded(&base, core::slice::from_ref(&demand), Dialect::Rfc8259);
        println!("{name}: {tag}");
        assert_eq!(
            tag, "AGREE",
            "{name}: the sharded Slice answer must equal the serial window"
        );
    }
    // The same over a comment dialect (the `parts > 1` comment cut).
    let mut jsonc = Vec::new();
    jsonc.extend_from_slice(b"[\n");
    for i in 0..120_000u32 {
        jsonc.extend_from_slice(b"  ");
        jsonc.extend_from_slice(format!(r#"{{"id":{i}}}"#).as_bytes());
        if i + 1 < 120_000 {
            jsonc.push(b',');
        }
        jsonc.extend_from_slice(b" /* c */\n");
    }
    jsonc.push(b']');
    let slice = Demand::Slice {
        range: Range {
            start: Some(0),
            end: Some(3),
        },
        nested: None,
    };
    println!(
        "jsonc slice(0,3): {}",
        sharded(&jsonc, core::slice::from_ref(&slice), Dialect::Jsonc)
    );
    assert_eq!(
        sharded(&jsonc, core::slice::from_ref(&slice), Dialect::Jsonc),
        "AGREE",
        "jsonc slice(0,3): the sharded Slice answer must equal the serial window"
    );
}

/// The same, but big enough that the plan actually splits into >= 2 morsels
/// (`pack_elements` returns one full range below the target).
#[test]
fn large_shard_door_refuses_what_serial_refuses() {
    let mut base = Vec::new();
    base.push(b'[');
    for i in 0..120_000u32 {
        if i > 0 {
            base.push(b',');
        }
        base.extend_from_slice(format!(r#"{{"id":{i}}}"#).as_bytes());
    }
    base.push(b']');
    println!("base len {}", base.len());
    let rows = project_root(&["id"]);
    let cases: Vec<(&str, Vec<u8>, Demand)> = vec![
        ("valid large", base.clone(), rows.clone()),
        (
            "trailing junk",
            {
                let mut v = base.clone();
                v.extend_from_slice(b" junk");
                v
            },
            rows.clone(),
        ),
        (
            "trailing second value",
            {
                let mut v = base.clone();
                v.extend_from_slice(b" 1");
                v
            },
            rows.clone(),
        ),
    ];
    for (name, src, demand) in cases {
        let tag = sharded(&src, core::slice::from_ref(&demand), Dialect::Rfc8259);
        println!("{name}: {tag}");
        assert!(
            !tag.starts_with("DIVERGE"),
            "{name}: the sharded door accepted input the serial door refuses: {tag}"
        );
    }
}

fn big_arrays_of_objects(n: usize) -> Vec<u8> {
    let mut base = Vec::new();
    base.push(b'[');
    for i in 0..n {
        if i > 0 {
            base.push(b',');
        }
        base.extend_from_slice(format!(r#"[{{"id":{i}}}]"#).as_bytes());
    }
    base.push(b']');
    base
}
