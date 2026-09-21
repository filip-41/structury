//! Differential: every scan mark equals the equivalent lookup on a materialized
//! tree or a table-driven DOM oracle, over text, streams, and mixed demands.

use structury::{
    Answer, ByteRange, ColumnCell, Columns, CompactStr, Demand, Document, Oracle, OracleAnswer, Path, Predicate, Step,
    Strictness, Value, stitch,
};
use structury_json::{Dialect, JsonInput, scan, scan_each};

mod common;

use common::{
    Divergences, Lcg, Obs, abc, and, check_stream, check_text, collection, dom_array, dom_filter, dom_project,
    element_kinds, eq, filter, filter_key, filter_root, ge, gt, json_value, lt, mat, mat_value, nav, ndjson,
    nested_filter, nested_projection, not, num, obj, observe, observe_all, one, or, path, path_index, path_key,
    plan_ranges, project, project_index, project_key, project_root, project_steps, req, req_with, rows, slice, text,
    text_array, text_dialect, users_array,
};

fn whole(bytes: &[u8]) -> Value {
    let demand = Demand::Whole;
    let request = req_with(
        JsonInput::Text,
        core::slice::from_ref(&demand),
        Strictness::Strict,
        Dialect::Rfc8259,
    );
    mat_value(&scan(bytes, &request).expect("whole").answers[0]).expect("dom")
}

fn scan_path(bytes: &[u8], steps: Vec<Step>) -> Option<Value> {
    let demand = Demand::path(steps);
    let result = scan(
        bytes,
        &req(JsonInput::Text, core::slice::from_ref(&demand), Dialect::Rfc8259),
    )
    .expect("path");
    match &result.answers[0] {
        Answer::Missing | Answer::TypeMismatch { .. } => None,
        mark => Some(mat_value(mark).expect("mat")),
    }
}

fn lookup(tree: &Value, steps: &[Step]) -> Option<Value> {
    nav(tree, &Path { steps: steps.to_vec() }).cloned()
}

fn scan_answer(bytes: &[u8], demand: &Demand) -> Value {
    mat_value(&common::answer_of(bytes, &text(core::slice::from_ref(demand)))).expect("materialize columns")
}

fn scan_project(bytes: &[u8], path: Path, fields: &[&str]) -> Value {
    scan_answer(
        bytes,
        &Demand::Project {
            path,
            fields: fields.iter().map(|field| (*field).into()).collect(),
        },
    )
}

fn scan_filter(bytes: &[u8], path: Path, predicate: Predicate, project: &[&str]) -> Value {
    scan_answer(
        bytes,
        &Demand::Filter {
            path,
            predicate,
            project: project.iter().map(|field| (*field).into()).collect(),
        },
    )
}

fn users() -> Path {
    Path::key("users")
}

#[test]
fn scan_vs_dom_object_and_array() {
    let samples: &[&[u8]] = &[
        br#"{"a":1,"b":{"c":[true,null,"x"]},"d":1.50}"#,
        br#"[1,{"k":-0},3]"#,
        br#"{"users":[{"id":1,"name":"a"},{"id":2,"name":"b"}]}"#,
        b"null",
        b"true",
        b"\"hi\"",
    ];
    let paths: &[Vec<Step>] = &[
        vec![],
        vec![Step::Key("a".into())],
        vec![Step::Key("b".into()), Step::Key("c".into()), Step::Index(0)],
        vec![Step::Key("b".into()), Step::Key("c".into()), Step::Index(-1)],
        vec![Step::Key("d".into())],
        vec![Step::Key("users".into()), Step::Index(0), Step::Key("id".into())],
        vec![Step::Key("users".into()), Step::Index(-1), Step::Key("name".into())],
        vec![Step::Index(0)],
        vec![Step::Index(-1)],
        vec![Step::Key("nope".into())],
    ];
    for bytes in samples {
        let tree = whole(bytes);
        for steps in paths {
            let from_scan = scan_path(bytes, steps.clone());
            let from_dom = if steps.is_empty() {
                Some(tree.clone())
            } else {
                lookup(&tree, steps)
            };
            assert_eq!(
                from_scan,
                from_dom,
                "src={} path={steps:?}",
                core::str::from_utf8(bytes).unwrap()
            );
        }
    }
}

/// A projection batch must materialize to the same rows the materialized tree
/// projects. Mixed shapes, duplicate keys, missing keys, and nested values all
/// exercise the head/slot reuse without misattributing a span.
#[test]
fn project_columns_match_dom_projection() {
    let src = br#"{"users":[
        {"id":1,"name":"a","profile":{"city":"x"}},
        {"id":2,"name":"b","extra":true},
        {"id":3,"profile":{"city":"z"}},
        {"id":4,"name":"d","name":"e"},
        {"id":5,"name":"f","profile":{"city":"w","zip":9}},
        {"id":6,"age":30},
        {"id":7,"name":"g","tags":["p","q"]}
    ]}"#;
    let tree = whole(src);
    for fields in [
        &["id"][..],
        &["name", "id"][..],
        &["profile"][..],
        &["name", "profile", "tags"][..],
        &["missing", "id"][..],
    ] {
        let got = scan_project(src, users(), fields);
        let want = dom_project(&tree, &users(), fields);
        assert_eq!(
            got,
            want,
            "fields={fields:?} src={}",
            core::str::from_utf8(src).unwrap()
        );
    }
}

/// Shapes that alternate every row defeat head/slot reuse and force the general
/// member loop; key order, duplicates, and absence must still answer.
#[test]
fn alternating_shapes_fall_back() {
    let src = br#"{"users":[
        {"a":1,"b":2},
        {"b":3,"a":4},
        {"a":5,"b":6},
        {"b":7,"a":8,"c":9},
        {"c":10,"b":11,"a":12}
    ]}"#;
    let tree = whole(src);
    let got = scan_project(src, users(), &["a", "b", "c"]);
    assert_eq!(got, dom_project(&tree, &users(), &["a", "b", "c"]));
}

/// Duplicate keys are last-wins, including when the duplicate is the projected key.
#[test]
fn duplicate_keys_are_last_wins() {
    let src = br#"{"users":[
        {"x":"first","x":2,"y":1},
        {"y":2,"x":"third","x":4},
        {"x":5,"y":3}
    ]}"#;
    let tree = whole(src);
    for fields in [&["x"][..], &["y", "x"][..]] {
        assert_eq!(
            scan_project(src, users(), fields),
            dom_project(&tree, &users(), fields),
            "fields={fields:?}"
        );
    }
}

/// A wide homogeneous run exercises reuse across many rows; a missing field and
/// a duplicate mid-run force a rebuild without disturbing the rest.
#[test]
fn homogeneous_run_with_shape_breaks() {
    let mut src = String::from("{\"users\":[");
    for i in 0..200 {
        if i > 0 {
            src.push(',');
        }
        match i {
            57 => src.push_str("{\"id\":57}"),
            120 => src.push_str("{\"id\":120,\"score\":9,\"score\":10}"),
            _ => {
                let score = i % 7;
                let _ = core::fmt::Write::write_fmt(
                    &mut src,
                    format_args!("{{\"id\":{i},\"name\":\"n{i}\",\"score\":{score}}}"),
                );
            }
        }
    }
    src.push_str("]}");
    let src = src.into_bytes();
    let tree = whole(&src);
    for fields in [&["id"][..], &["score", "id"][..], &["name", "score", "id"][..]] {
        assert_eq!(
            scan_project(&src, users(), fields),
            dom_project(&tree, &users(), fields),
            "fields={fields:?}"
        );
    }
}

#[test]
fn filter_columns_match_dom_filter() {
    let src = br#"{"users":[
        {"id":1,"active":true,"score":10},
        {"id":2,"active":false,"score":200},
        {"id":3,"score":300},
        {"id":4,"active":true,"score":5},
        {"id":5,"active":false,"score":50},
        {"id":6,"active":true,"active":false,"score":7}
    ]}"#;
    let tree = whole(src);
    let active = Predicate::Eq {
        field: "active".into(),
        value: Value::Bool(true),
    };
    assert_eq!(
        scan_filter(src, users(), active.clone(), &["id", "name"]),
        dom_filter(&tree, &users(), &active, &["id", "name"])
    );
    let high = Predicate::Gt {
        field: "score".into(),
        value: num("100"),
    };
    assert_eq!(
        scan_filter(src, users(), high.clone(), &["id", "score"]),
        dom_filter(&tree, &users(), &high, &["id", "score"])
    );
    let not_active = Predicate::Or(
        Box::new(Predicate::Not(Box::new(active.clone()))),
        Box::new(Predicate::Ge {
            field: "score".into(),
            value: num("300"),
        }),
    );
    assert_eq!(
        scan_filter(src, users(), not_active.clone(), &["id"]),
        dom_filter(&tree, &users(), &not_active, &["id"])
    );
}

/// The root record stream shares the row layout, so a root projection over it
/// must match the DOM too.
#[test]
fn root_record_projection_matches_dom() {
    let src = b"{\"id\":1,\"v\":{\"x\":1}}\n{\"id\":2}\n{\"v\":{\"x\":3},\"id\":4}\n";
    let tree = whole(b"[{\"id\":1,\"v\":{\"x\":1}},{\"id\":2},{\"v\":{\"x\":3},\"id\":4}]");
    let demand = Demand::Project {
        path: Path::root(),
        fields: vec!["id".into(), "v".into()],
    };
    let result = scan(
        src,
        &req(JsonInput::Ndjson, core::slice::from_ref(&demand), Dialect::Rfc8259),
    )
    .expect("scan records");
    let got = mat_value(&result.answers[0]).expect("materialize");
    assert_eq!(got, dom_project(&tree, &Path::root(), &["id", "v"]));
}

fn scan_nested_projection(src: &[u8]) -> Value {
    scan_answer(src, &common::nested_projection())
}

/// Every element contributes a row even when the spine is absent: a missing
/// spine is an absent projection (`{}`), never a dropped or overwritten row. The
/// first case pins the bug where repeated absent elements collapsed the batch to
/// `rows=1` (the accumulator lives in the answer mark).
#[test]
fn nested_projection_keeps_one_row_per_element() {
    for (src, want) in [
        (
            br#"{"rows":[{"x":1},{"x":2},{"a":{"b":{"c":{"d":3}}}},{"x":4}]}"#.as_slice(),
            vec![obj(&[]), obj(&[]), obj(&[("d", num("3"))]), obj(&[])],
        ),
        (
            br#"{"rows":[{"a":{"b":{"c":{"d":1}}}},{"a":{"b":{"c":{"d":2}}}}]}"#.as_slice(),
            vec![obj(&[("d", num("1"))]), obj(&[("d", num("2"))])],
        ),
        (
            br#"{"rows":[{"x":1},{"a":{"b":{"c":{"d":42}}}},{"a":{"b":{"c":{"d":43}}}}]}"#.as_slice(),
            vec![obj(&[]), obj(&[("d", num("42"))]), obj(&[("d", num("43"))])],
        ),
    ] {
        assert_eq!(scan_nested_projection(src), Value::Array(want), "{src:?}");
    }
}

/// A long run whose key order rotates and whose keys drop in and out exercises
/// the content-keyed head index: keys also change the trivia after the colon, so
/// a retained head may only match byte-for-byte.
#[test]
fn rotated_run_index_matches_dom() {
    let keys = ["id", "name", "email", "active", "score", "v"];
    let mut src = String::from("{\"users\":[");
    for i in 0..60usize {
        if i > 0 {
            src.push(',');
        }
        src.push('{');
        let mut first = true;
        for n in 0..keys.len() {
            let key = keys[(n + i) % keys.len()];
            // Drop a rotating subset, and vary the colon trivia by row.
            if key != "id" && (n * 3 + i) % 4 == 0 {
                continue;
            }
            let sep = if first { "" } else { "," };
            first = false;
            let space = if (i + n) % 2 == 0 { "" } else { " " };
            let body = match key {
                "id" => i.to_string(),
                "name" => format!("\"n{i}\""),
                "email" => format!("\"e{i}@x\""),
                "active" => (i.is_multiple_of(2)).to_string(),
                "score" => (i % 11).to_string(),
                "v" => format!("{{\"x\":{i}}}"),
                _ => unreachable!(),
            };
            let _ = core::fmt::Write::write_fmt(&mut src, format_args!("{sep}\"{key}\":{space}{body}"));
        }
        src.push('}');
    }
    src.push_str("]}");
    let src = src.into_bytes();
    let tree = whole(&src);
    for fields in [&["name"][..], &["id", "score"][..], &["name", "v", "missing"][..]] {
        assert_eq!(
            scan_project(&src, users(), fields),
            dom_project(&tree, &users(), fields),
            "fields={fields:?}"
        );
    }
    let active = Predicate::Eq {
        field: "active".into(),
        value: Value::Bool(true),
    };
    assert_eq!(
        scan_filter(&src, users(), active.clone(), &["id", "score"]),
        dom_filter(&tree, &users(), &active, &["id", "score"])
    );
}

/// A retained head must not match as a strict prefix of a longer head, so a
/// shorter stored head followed by trivia falls back rather than advancing.
#[test]
fn head_prefix_with_trivia_is_not_a_match() {
    let src = br#"{"users":[{"a":1,"b":2},{"a": 1,"b":2},{"a":1,"b": 2}]}"#;
    let tree = whole(src);
    assert_eq!(
        scan_project(src, users(), &["a", "b"]),
        dom_project(&tree, &users(), &["a", "b"])
    );
}

/// One law row: `demands` over `text` (and `stream`, when present) must materialize to `want`.
struct LawRow {
    id: &'static str,
    demands: Vec<Demand>,
    text: &'static [u8],
    stream: Option<&'static [u8]>,
    answer: usize,
    want: &'static str,
}

/// Table-driven row laws: nested projection, nested filter, keyed collection,
/// Whole, slice, and index-scoped demands over text and streams.
#[allow(clippy::too_many_lines)]
fn law_rows() -> Vec<LawRow> {
    let np = nested_projection;
    let nf = nested_filter;
    let keys = || path_key("rows", Some(collection(Some(&["id"]), None)));
    let t = |id, demand: Demand, text: &'static [u8], want| LawRow {
        id,
        demands: vec![demand],
        text,
        stream: None,
        answer: 0,
        want,
    };
    let d = |id, demand: Demand, text: &'static [u8], stream: &'static [u8], want| LawRow {
        id,
        demands: vec![demand],
        text,
        stream: Some(stream),
        answer: 0,
        want,
    };
    let d_at = |id, demands: Vec<Demand>, text: &'static [u8], stream: &'static [u8], answer, want| LawRow {
        id,
        demands,
        text,
        stream: Some(stream),
        answer,
        want,
    };
    vec![
        t(
            "control",
            np(),
            br#"{"rows":[{"a":{"b":{"c":{"d":1}}}},{"a":{"b":{"c":{"d":2}}}}]}"#,
            r#"[{"d":1},{"d":2}]"#,
        ),
        t(
            "nested projection middle scalar",
            np(),
            br#"{"rows":[{"a":{"b":{"c":{"d":1}}}},5,{"a":{"b":{"c":{"d":3}}}}]}"#,
            r#"[{"d":1},{},{"d":3}]"#,
        ),
        t("nested projection all scalars", np(), br#"{"rows":[1,2]}"#, "[{},{}]"),
        t(
            "nested projection leading scalar",
            np(),
            br#"{"rows":[5,{"a":{"b":{"c":{"d":3}}}}]}"#,
            r#"[{},{"d":3}]"#,
        ),
        t(
            "nested projection every scalar kind",
            np(),
            br#"{"rows":["x",true,null,false]}"#,
            "[{},{},{},{}]",
        ),
        t(
            "nested projection empty spine",
            np(),
            br#"{"rows":[{"a":{"b":{"c":{}}}},{"a":{"b":{"c":{"d":4}}}}]}"#,
            r#"[{},{"d":4}]"#,
        ),
        t("nested projection empty array", np(), br#"{"rows":[]}"#, "[]"),
        t(
            "array then object element",
            np(),
            br#"{"rows":[[1],{"a":{"b":{"c":{"d":2}}}}]}"#,
            r#"[{},{"d":2}]"#,
        ),
        t(
            "object then array element",
            np(),
            br#"{"rows":[{"a":{"b":{"c":{"d":1}}}},[9]]}"#,
            r#"[{"d":1},{}]"#,
        ),
        t(
            "empty array element",
            np(),
            br#"{"rows":[{"a":{"b":{"c":{"d":1}}}},[]]}"#,
            r#"[{"d":1},{}]"#,
        ),
        t(
            "array after non-spine element",
            np(),
            br#"{"rows":[{"x":1},[1,2]]}"#,
            "[{},{}]",
        ),
        t(
            "nested filter middle scalar",
            nf(&["n"]),
            br#"{"rows":[{"n":5},{"n":15},7]}"#,
            r#"[{"n":5},{"n":15}]"#,
        ),
        t(
            "nested filter leading scalar",
            nf(&["n"]),
            br#"{"rows":[7,{"n":5}]}"#,
            r#"[{"n":5}]"#,
        ),
        t(
            "nested filter scalar between",
            nf(&["n"]),
            br#"{"rows":[{"n":-1},7,{"n":5}]}"#,
            r#"[{"n":5}]"#,
        ),
        t("nested filter all scalars", nf(&["n"]), br#"{"rows":[1,2]}"#, "[]"),
        t(
            "nested filter trailing scalar",
            nf(&["n"]),
            br#"{"rows":[{"n":5},7]}"#,
            r#"[{"n":5}]"#,
        ),
        t(
            "nested filter mixed scalars",
            nf(&["n"]),
            br#"{"rows":[7,{"n":5},8,{"n":6}]}"#,
            r#"[{"n":5},{"n":6}]"#,
        ),
        t(
            "nested filter array element",
            nf(&["n"]),
            br#"{"rows":[{"n":5},[9],{"n":15}]}"#,
            r#"[{"n":5},{"n":15}]"#,
        ),
        t(
            "whole-row filter",
            nf(&[]),
            br#"{"rows":[7,{"n":5},8,{"n":6}]}"#,
            r#"[{"n":5},{"n":6}]"#,
        ),
        t(
            "keyed collection scalar",
            keys(),
            br#"{"rows":[{"id":1},5,{"id":3}]}"#,
            r#"[{"id":1},{},{"id":3}]"#,
        ),
        t(
            "keyed collection array element",
            keys(),
            br#"{"rows":[{"id":1},[9],{"id":3}]}"#,
            r#"[{"id":1},{},{"id":3}]"#,
        ),
        t(
            "root filter",
            filter_root(gt("n", "0"), &["n"]),
            br#"[{"n":5},7,{"n":-1},{"n":15}]"#,
            r#"[{"n":5},{"n":15}]"#,
        ),
        t(
            "path project",
            project(Path::key("users"), &["id"]),
            br#"{"users":[{"id":1},5,{"id":3}]}"#,
            r#"[{"id":1},{},{"id":3}]"#,
        ),
        d(
            "stream whole",
            Demand::Whole,
            br#"[{"a":1}]"#,
            b"{\"a\":1}\n",
            r#"[{"a":1}]"#,
        ),
        d(
            "stream collection",
            collection(None, None),
            br#"[{"a":1}]"#,
            b"{\"a\":1}\n",
            r#"[{"a":1}]"#,
        ),
        d(
            "stream collection keys",
            collection(Some(&["a"]), None),
            br#"[{"a":1}]"#,
            b"{\"a\":1}\n",
            r#"[{"a":1}]"#,
        ),
        d(
            "stream slice",
            slice(None, None),
            br#"[{"a":1}]"#,
            b"{\"a\":1}\n",
            r#"[{"a":1}]"#,
        ),
        d(
            "stream two records",
            collection(None, None),
            br#"[{"a":1},{"b":2}]"#,
            b"{\"a\":1}\n{\"b\":2}\n",
            r#"[{"a":1},{"b":2}]"#,
        ),
        d("empty stream", project_root(&["a"]), b"[]", b"", "[]"),
        d(
            "absent spine",
            collection(None, Some(project(abc(), &["d"]))),
            b"[{}]",
            b"{}\n",
            "[{}]",
        ),
        d(
            "nested whole Path",
            collection(None, Some(path(&[], None))),
            b"[null]",
            b"null\n",
            "[null]",
        ),
        d(
            "index 0 on a record",
            project_index(0, &["id"]),
            b"[[1,2]]",
            b"[1,2]\n",
            "[{}]",
        ),
        d(
            "stream index 0 object",
            project_index(0, &["a"]),
            br#"[{"a":1},{"a":2}]"#,
            b"{\"a\":1}\n{\"a\":2}\n",
            r#"[{"a":1}]"#,
        ),
        d(
            "stream index -1 object",
            project_index(-1, &["a"]),
            br#"[{"a":1},{"a":2}]"#,
            b"{\"a\":1}\n{\"a\":2}\n",
            r#"[{"a":2}]"#,
        ),
        d(
            "spine on array",
            collection(None, Some(project(abc(), &["d"]))),
            br#"[{"a":{"b":{"c":[{"d":7}]}}}]"#,
            b"{\"a\":{\"b\":{\"c\":[{\"d\":7}]}}}\n",
            r#"[{"d":7}]"#,
        ),
        d(
            "spine on scalar array",
            collection(None, Some(project(abc(), &["d"]))),
            br#"[{"a":{"b":{"c":[1,2]}}}]"#,
            b"{\"a\":{\"b\":{\"c\":[1,2]}}}\n",
            "[{},{}]",
        ),
        d_at(
            "sibling slice",
            vec![project_root(&["id"]), slice(None, None)],
            br#"[[{"id":9}]]"#,
            b"[[{\"id\":9}]]\n",
            0,
            "[{}]",
        ),
        d_at(
            "record Whole sibling",
            vec![Demand::Whole, project_root(&["id"])],
            br#"[[1,2],{"id":1}]"#,
            b"[1,2]\n{\"id\":1}\n",
            1,
            r#"[{},{"id":1}]"#,
        ),
    ]
}

#[test]
fn row_laws() {
    let mut bad = Divergences::default();
    for r in law_rows() {
        let want = json_value(r.want);
        let text = mat(&common::answers(r.text, &text(&r.demands))[r.answer]);
        if text != want {
            bad.push(format!("[{}] text got={text:?} want={want:?}", r.id));
        }
        if let Some(stream) = r.stream {
            let got = mat(&common::answers(stream, &common::stream(&r.demands))[r.answer]);
            if got != want {
                bad.push(format!("[{}] stream got={got:?} want={want:?}", r.id));
            }
        }
    }
    bad.assert_empty("row laws");
}

/// The nested projection's rows equal the DOM oracle's.
#[test]
fn row_law_vs_dom() {
    let elements: &[&str] = &[
        r#"{"a":{"b":{"c":{"d":1}}}}"#,
        r#"{"a":{"b":{"c":{}}}}"#,
        r#"{"a":{"b":5}}"#,
        "5",
        "[1,2]",
        "[]",
        r#"[{"d":9}]"#,
        r#"{"x":1}"#,
    ];
    let src = format!("{{\"rows\":[{}]}}", elements.join(",")).into_bytes();
    let tree = common::parsed(&src).expect("parse");
    let inner = collection(None, Some(project(abc(), &["d"])));
    let want = dom_array(rows(&tree, &Path::key("rows")), &inner).expect("modelled");
    let got = observe(&common::answer_of(
        &src,
        &text(core::slice::from_ref(&nested_projection())),
    ));
    assert_eq!(got, want, "nested projection rows");
}

/// An empty stream is an empty batch beside a sibling; a key-scoped stream
/// demand is `TypeMismatch{Array}`.
#[test]
fn stream_marks() {
    let demands = [Demand::Whole, Demand::Oracle(Oracle::Count)];
    let got = common::answer_of(b"", &common::stream(&demands));
    assert!(
        matches!(got, Answer::Columns(_)),
        "Whole on an empty virtual array is an empty batch, got {got:?}"
    );

    let demands = [
        project_key("a", &["x"]),
        filter_key("a", gt("n", "0"), &["n"]),
        path_key("a", None),
    ];
    for src in [&b"{\"a\":1}\n"[..], b""] {
        for (i, got) in common::answers(src, &common::req(JsonInput::Ndjson, &demands, Dialect::Rfc8259))
            .iter()
            .enumerate()
        {
            assert!(
                matches!(got, Answer::TypeMismatch { .. }),
                "src={src:?} demand[{i}]={got:?} (want TypeMismatch{{Array}})"
            );
        }
    }
}

/// The virtual-array contract: every root demand answers the same on a stream and the equivalent text array.
#[test]
fn stream_root_demands_match_the_text_array() {
    let demands: Vec<Demand> = vec![
        Demand::Whole,
        project_root(&["a"]),
        project_root(&["a", "b"]),
        filter_root(gt("n", "0"), &["n"]),
        collection(Some(&["a"]), None),
        collection(None, None),
        path(&[Step::Index(0)], None),
        path(&[Step::Index(1)], None),
        path(&[Step::Index(-1)], None),
        path(&[Step::Index(9)], None),
        path_key("a", None),
        project_index(0, &["a"]),
        project_key("a", &["x"]),
        filter_key("a", gt("n", "0"), &["n"]),
        slice(Some(0), Some(2)),
        Demand::Oracle(Oracle::Count),
        Demand::Oracle(Oracle::Kind),
        Demand::Oracle(Oracle::MemberNames),
        Demand::Oracle(Oracle::HasKey { key: "a".into() }),
    ];
    let corpora: &[&[&str]] = &[
        &[],
        &["1"],
        &["1", "2", "3"],
        &["{}"],
        &["{}", "{}"],
        &[r#"{"a":1,"n":5}"#],
        &[r#"{"a":1,"n":5}"#, r#"{"a":2,"n":-1}"#],
        &[r#"{"n":5}"#, "7", r#"{"n":15}"#],
        &["5", r#"{"n":5}"#, "null", r#"{"n":15}"#, "[1,2]"],
        &[r#"{"a":{"x":1}}"#, r#"{"a":{"x":2}}"#],
    ];
    let mut bad = Divergences::default();
    for records in corpora {
        let text = text_array(records);
        let stream = ndjson(records);
        for demand in &demands {
            let from_text = observe(&one(&text, JsonInput::Text, demand, Dialect::Rfc8259));
            let from_stream = observe(&one(&stream, JsonInput::Ndjson, demand, Dialect::Rfc8259));
            if from_stream != from_text {
                bad.push(format!(
                    "demand={demand:?} records={records:?} stream={from_stream:?} text={from_text:?}"
                ));
            }
        }
    }
    bad.assert_empty("stream/text-array divergences");
}

/// `scan_each` visits exactly the marks `scan` answers, over text and streams.
#[test]
fn scan_each_mirrors_scan() {
    let mut bad = Divergences::default();

    let demand = path(&[Step::Index(1), Step::Key("id".into())], None);
    let request = common::stream(core::slice::from_ref(&demand));
    let mut visited = 0usize;
    scan_each(b"{\"id\":1}\n{\"id\":2}\n", &request, |answer| {
        visited += 1;
        assert!(matches!(answer, Answer::Document(_)), "visit {answer:?}");
    })
    .expect("scan_each");
    assert_eq!(visited, 1, "scan_each must mirror scan's virtual-array mapping");

    let one_record: Vec<Vec<Demand>> = vec![
        vec![project_root(&["a"])],
        vec![path(&[Step::Index(0)], None)],
        vec![path(&[Step::Index(0), Step::Key("a".into())], None)],
        vec![path_index(0, Some(project_root(&["a"])))],
        vec![path_index(0, Some(filter_root(gt("a", "0"), &["a"])))],
    ];
    let src = b"{\"a\":1}\n";
    for demands in &one_record {
        let request = common::req(JsonInput::Ndjson, demands, Dialect::Rfc8259);
        let scanned = observe_all(&common::answers(src, &request));
        let mut visited: Vec<Obs> = Vec::new();
        scan_each(src, &request, |answer| visited.push(observe(&answer))).expect("scan_each");
        if scanned != visited {
            bad.push(format!("demands={demands:?} scan={scanned:?} scan_each={visited:?}"));
        }
    }

    let demand = project_root(&["id"]);
    let request = common::stream(core::slice::from_ref(&demand));
    let scanned = mat(&common::answer_of(b"5\n", &request));
    let mut visited = Vec::new();
    scan_each(b"5\n", &request, |answer| visited.push(observe(&answer))).expect("scan_each");
    if scanned != json_value("[{}]") || visited != vec![Obs::Value(json_value("[{}]"))] {
        bad.push(format!("scan={scanned:?} visits={visited:?}"));
    }

    let demands_sets: Vec<Vec<Demand>> = vec![
        vec![project_root(&["id"])],
        vec![collection(Some(&["id"]), None)],
        vec![collection(None, Some(project(abc(), &["d"])))],
        vec![collection(None, Some(filter_root(gt("n", "0"), &["n"])))],
    ];
    let records = ["5", "\"s\"", "true", "null", "[1,2]", "[]", "{}", r#"{"id":9,"n":1}"#];
    for record in records {
        let stream = ndjson(&[record]);
        let text = text_array(&[record]);
        for demands in &demands_sets {
            let request = common::stream(demands);
            let scanned = observe_all(&common::answers(&stream, &request));
            let mut visited: Vec<Obs> = Vec::new();
            scan_each(&stream, &request, |answer| visited.push(observe(&answer))).expect("scan_each");
            let from_text = observe_all(&common::answers(&text, &common::text(demands)));
            if scanned != visited {
                bad.push(format!(
                    "record={record} demands={demands:?} scan={scanned:?} scan_each={visited:?}"
                ));
            }
            if from_text != scanned {
                bad.push(format!(
                    "record={record} demands={demands:?} text={from_text:?} stream={scanned:?}"
                ));
            }
        }
    }
    bad.assert_empty("scan/scan_each divergence");
}

/// A member-head slot cached under one array's field set must not leak into a
/// sibling array.
#[test]
fn cross_array_head_slot_does_not_leak() {
    let cases: &[(&[u8], Vec<Demand>, Vec<&str>)] = &[
        (
            br#"{"a":[{"x":1,"y":9}],"b":[{"y":2}],"c":[{"x":3}]}"#,
            vec![
                project_key("a", &["x"]),
                project_key("b", &["y"]),
                project_key("c", &["x"]),
            ],
            vec![r#"[{"x":1}]"#, r#"[{"y":2}]"#, r#"[{"x":3}]"#],
        ),
        (
            br#"{"p":{"a":[{"x":1}],"b":[{"y":2}]}}"#,
            vec![
                project_steps(&[Step::Key("p".into()), Step::Key("a".into())], &["x"]),
                project_steps(&[Step::Key("p".into()), Step::Key("b".into())], &["y"]),
            ],
            vec![r#"[{"x":1}]"#, r#"[{"y":2}]"#],
        ),
        (
            br#"{"a":[{"y":99,"x":1}],"b":[{"y":2}]}"#,
            vec![project_key("a", &["x"]), project_key("b", &["y"])],
            vec![r#"[{"x":1}]"#, r#"[{"y":2}]"#],
        ),
        (
            br#"{"a":[{"x":1}],"b":[{"x":2,"y":3}]}"#,
            vec![project_key("a", &["y"]), project_key("b", &["x"])],
            vec![r"[{}]", r#"[{"x":2}]"#],
        ),
    ];
    for (src, demands, wants) in cases {
        let answers = common::answers(src, &text(demands));
        for (i, want) in wants.iter().enumerate() {
            assert_eq!(
                mat(&answers[i]),
                json_value(want),
                "src={} demand[{i}]",
                String::from_utf8_lossy(src)
            );
        }
    }
}

/// Every project field set and filter predicate must agree with the DOM oracle.
#[test]
fn project_fields_and_predicates_match_dom() {
    let src = br#"{"users":[
        {"id":1,"name":"a","idx":9,"active":true,"score":10,"x":{"y":1}},
        {"idx":2,"id":3,"name":"b","active":false,"score":200},
        {"i":4,"id":5,"name":"c","active":true,"score":300,"id":6},
        {"id":7,"active":true,"score":5,"name":"d"},
        {"id":8,"nope":0},
        {"id":9,"name":"e","active":true,"score":50,"x":{"y":2}}
    ]}"#;
    let tree = common::parsed(src).expect("parse");
    let users = Path::key("users");
    let active = eq("active", Value::Bool(true));
    let high = gt("score", "100");
    let compound = or(not(active.clone()), ge("score", "300"));
    let mut bad = Divergences::default();

    let field_sets: &[&[&str]] = &[
        &["id"],
        &["id", "name"],
        &["name", "id"],
        &["id", "id"],
        &["idx"],
        &["i", "id"],
        &["i"],
        &["id", "idx", "i"],
        &["z", "id"],
        &["name", "x"],
        &["x", "name", "score"],
        &["missing", "id", "also_missing"],
    ];
    for fields in field_sets {
        let demand = project(users.clone(), fields);
        let got = mat(&common::answer_of(src, &text(core::slice::from_ref(&demand))));
        let want = dom_project(&tree, &users, fields);
        if got != want {
            bad.push(format!("Project fields={fields:?} got={got:?} want={want:?}"));
        }
    }

    let pred_sets: &[(&Predicate, &[&str])] = &[
        (&active, &["id", "name"]),
        (&active, &["active", "id"]),
        (&high, &["id", "score"]),
        (&high, &["score", "score", "id"]),
        (&compound, &["id"]),
        (&compound, &["score", "name", "id"]),
        (&and(high.clone(), active.clone()), &["id"]),
        (&not(high.clone()), &["id", "score"]),
        (&or(eq("id", num("1")), lt("score", "60")), &["id", "score"]),
    ];
    for (predicate, fields) in pred_sets {
        let demand = filter(users.clone(), (*predicate).clone(), fields);
        let got = mat(&common::answer_of(src, &text(core::slice::from_ref(&demand))));
        let want = dom_filter(&tree, &users, predicate, fields);
        if got != want {
            bad.push(format!(
                "Filter pred={predicate:?} fields={fields:?} got={got:?} want={want:?}"
            ));
        }
    }
    bad.assert_empty("project/filter divergence");
}

/// A fused `Whole` + sibling `Project` must not depend on fact recording.
#[test]
fn fused_facts_answer_a_sibling_project() {
    let src = br#"{"a":1, /* c */ "b":2, "z":3}"#;
    let demands = [Demand::Whole, project_root(&["a", "b", "z"])];
    let mut with_facts = text_dialect(&demands, Dialect::Jsonc);
    with_facts.facts = true;
    let plain = text_dialect(&demands, Dialect::Jsonc);
    assert_eq!(
        observe(&common::scan_of(src, &with_facts).answers[1]),
        observe(&common::scan_of(src, &plain).answers[1])
    );
}

/// A sibling oracle must not drop the project's member set.
#[test]
fn collect_all_sibling_oracle_keeps_project_fields() {
    let src = br#"{"users":[{"id":1,"name":"a"},{"id":2}]}"#;
    let demands = [
        project_key("users", &["id", "name"]),
        Demand::Oracle(Oracle::MemberCount),
    ];
    let result = common::scan_of(src, &text(&demands));
    assert_eq!(
        observe(&result.answers[0]),
        Obs::Value(json_value(r#"[{"id":1,"name":"a"},{"id":2}]"#))
    );
}

/// The shard `consumed_row_demand` mapping must stitch to the serial answer for
/// every row-demand shape.
#[test]
fn shard_answers_stitch_to_serial() {
    let mut bad = Divergences::default();
    let src = users_array(40_000);
    let users = Path::key("users");
    let keyed = |path: Path, predicate: Predicate| filter(path, predicate, &["id", "score"]);
    let demands: Vec<Demand> = vec![
        project(users.clone(), &["id"]),
        keyed(users.clone(), gt("score", "50")),
        path(&users.steps, Some(project_root(&["id"]))),
        path(&users.steps, Some(filter_root(gt("score", "50"), &["id"]))),
        path(&users.steps, Some(Demand::Oracle(Oracle::Count))),
    ];
    for demand in &demands {
        let request = text(core::slice::from_ref(demand));
        let serial = observe(&common::scan_of(&src, &request).answers[0]);
        let ranges = plan_ranges(&src, &request).expect("plan");
        let parts: Vec<_> = ranges
            .iter()
            .map(|range| common::scan_range(&src, *range, &request).expect("shard"))
            .collect();
        let stitched = observe(&stitch(parts).answers[0]);
        if serial != stitched {
            bad.push(format!("demand={demand:?} serial={serial:?} stitched={stitched:?}"));
        }
    }

    let mut src = Vec::new();
    for i in 0..30_000u64 {
        src.extend_from_slice(format!("{{\"id\":{i},\"score\":{}}}\n", i % 100).as_bytes());
    }
    for demand in [
        project_root(&["id"]),
        filter_root(gt("score", "50"), &["id"]),
        Demand::Oracle(Oracle::Count),
    ] {
        let request = common::req(JsonInput::Ndjson, core::slice::from_ref(&demand), Dialect::Rfc8259);
        let serial = observe(&common::scan_of(&src, &request).answers[0]);
        let ranges = plan_ranges(&src, &request).expect("plan");
        let parts: Vec<_> = ranges
            .iter()
            .map(|range| common::scan_range(&src, *range, &request).expect("shard"))
            .collect();
        let stitched = observe(&stitch(parts).answers[0]);
        if serial != stitched {
            bad.push(format!(
                "demand={demand:?} parts={} serial={serial:?} stitched={stitched:?}",
                ranges.len()
            ));
        }
    }
    bad.assert_empty("shard mapping");
}

/// Randomized mixtures of objects, spines, scalars and arrays under the nested
/// `Collection -> Project` demand.
#[test]
fn randomized_nested_collection_matches_dom() {
    let demand = nested_projection();
    let mut rng = Lcg::new(0x5eed_1234);
    let mut bad = Divergences::default();
    for case in 0..400 {
        let n = usize::try_from(rng.pick(7)).expect("small");
        let mut src = String::from("{\"rows\":[");
        let mut want = Vec::new();
        for i in 0..n {
            if i > 0 {
                src.push(',');
            }
            let d = rng.pick(100);
            let expected = match rng.pick(9) {
                0 => {
                    src.push_str("{\"a\":{\"b\":{\"c\":{\"d\":");
                    src.push_str(&d.to_string());
                    src.push_str("}}}}");
                    obj(&[("d", num(&d.to_string()))])
                }
                1 => {
                    src.push_str("{\"a\":{\"b\":{\"c\":{}}}}");
                    obj(&[])
                }
                2 => {
                    src.push_str("{\"x\":1}");
                    obj(&[])
                }
                3 => {
                    src.push_str("{}");
                    obj(&[])
                }
                4 => {
                    src.push_str(&d.to_string());
                    obj(&[])
                }
                5 => {
                    src.push_str("\"s\"");
                    obj(&[])
                }
                6 => {
                    src.push_str(if rng.pick(2) == 0 { "null" } else { "true" });
                    obj(&[])
                }
                7 => {
                    src.push_str("[1,2]");
                    obj(&[])
                }
                8 => {
                    src.push_str("{\"a\":5}");
                    obj(&[])
                }
                _ => unreachable!(),
            };
            want.push(expected);
        }
        src.push_str("]}");
        let src = src.into_bytes();
        let got = observe(&common::answer_of(&src, &text(core::slice::from_ref(&demand))));
        let expected = Obs::Value(Value::Array(want));
        if got != expected {
            bad.push(format!(
                "case={case} src={} got={got:?} want={expected:?}",
                String::from_utf8_lossy(&src)
            ));
        }
        if bad.len() > 5 {
            break;
        }
    }
    bad.assert_empty("random nested collection");
}

fn groups() -> Vec<(&'static str, Vec<Demand>)> {
    let g = |name: &'static str, demands: Vec<Demand>| (name, demands);
    vec![
        g("project-flat", vec![project_root(&["id"])]),
        g("collection-keys", vec![collection(Some(&["id"]), None)]),
        g(
            "collection-proj-flat",
            vec![collection(None, Some(project_root(&["id"])))],
        ),
        g(
            "collection-proj-spine",
            vec![collection(None, Some(project(abc(), &["d"])))],
        ),
        g(
            "collection-filter",
            vec![collection(None, Some(filter_root(gt("n", "0"), &["n"])))],
        ),
        g(
            "collection-filter-whole",
            vec![collection(None, Some(filter_root(gt("n", "0"), &[])))],
        ),
        g("collection-path", vec![collection(None, Some(path(&[], None)))]),
        g("collection-whole", vec![collection(None, None)]),
        g("whole", vec![Demand::Whole]),
        g("slice", vec![slice(Some(0), Some(2))]),
        g("count", vec![Demand::Oracle(Oracle::Count)]),
        g("kind", vec![Demand::Oracle(Oracle::Kind)]),
        g("project-root-key-spine", vec![project_key("a", &["x"])]),
        g("project-index-path", vec![project_index(0, &["id"])]),
        g("mixed-whole-project", vec![Demand::Whole, project_root(&["id"])]),
        g(
            "mixed-whole-coll-spine",
            vec![Demand::Whole, collection(None, Some(project(abc(), &["d"])))],
        ),
        g(
            "mixed-project-count",
            vec![project_root(&["id"]), Demand::Oracle(Oracle::Count)],
        ),
        g(
            "mixed-whole-project-count-slice",
            vec![
                Demand::Whole,
                project_root(&["id"]),
                Demand::Oracle(Oracle::Count),
                slice(Some(0), Some(2)),
            ],
        ),
        g(
            "mixed-two-row-demands",
            vec![
                collection(None, Some(project_root(&["id"]))),
                collection(None, Some(filter_root(gt("n", "0"), &["n"]))),
            ],
        ),
    ]
}

fn check_corpus(bad: &mut Divergences, label: &str, elements: &[&str]) {
    let text = text_array(elements);
    let stream = ndjson(elements);
    let Value::Array(items) = common::parsed(&text).expect("dom") else {
        panic!("not array")
    };
    for (name, demands) in groups() {
        check_text(bad, &format!("{name}/{label}"), &text, &items, &demands);
        check_stream(bad, &format!("{name}/{label}"), &text, &stream, &demands);
    }
}

/// Every demand form × element kind × framing against the DOM oracle; zero
/// divergences.
#[test]
fn matrix_single_and_mixed_element_kinds() {
    let mut bad = Divergences::default();
    for element in element_kinds() {
        check_corpus(&mut bad, "one", &[element]);
    }
    let multisets: &[&[&str]] = &[
        &[r#"{"id":1,"n":5}"#, "5", r#"{"id":3,"n":15}"#],
        &[r#"{"a":{"b":{"c":{"d":1}}}}"#, "[9]", r#"{"a":{"b":{"c":{"d":3}}}}"#],
        &["5", r#"{"id":2}"#, "null", "true", r#""s""#, "[1,2]", "{}", "[]"],
        &[r#"[{"id":9}]"#, r#"{"id":1}"#, r"[[1],[2]]"],
        &[r#"{"id":1,"n":5}"#],
        &[r#"{"id":1,"n":5}"#, r#"{"id":2,"n":-1}"#],
    ];
    for elements in multisets {
        check_corpus(&mut bad, "mix", elements);
    }
    bad.assert_empty("matrix divergences");
}

/// An array *record* under a mixed `Whole` + row demand.
#[test]
fn matrix_array_record_under_mixed_whole_and_project() {
    let cases: &[&[&str]] = &[
        &["[1,2]", r#"{"id":1}"#],
        &[r#"{"id":1}"#, "[1,2]"],
        &["[1,2]", "[3,4]", r#"{"id":9}"#],
        &["[]", r#"{"id":1}"#],
        &["[[1],[2]]", r#"{"id":1}"#],
    ];
    let mixed = [
        ("m-whole-project", vec![Demand::Whole, project_root(&["id"])]),
        (
            "m-whole-coll-keys",
            vec![Demand::Whole, collection(Some(&["id"]), None)],
        ),
        (
            "m-whole-coll-spine",
            vec![Demand::Whole, collection(None, Some(project(abc(), &["d"])))],
        ),
    ];
    let mut bad = Divergences::default();
    for elements in cases {
        let text = text_array(elements);
        let stream = ndjson(elements);
        let Value::Array(items) = common::parsed(&text).expect("dom") else {
            panic!("not array")
        };
        for (name, demands) in &mixed {
            check_text(&mut bad, name, &text, &items, demands);
            check_stream(&mut bad, name, &text, &stream, demands);
        }
    }
    bad.assert_empty("array-record mixed divergences");
}

/// Zero-record NDJSON answers the empty virtual array for every demand form.
#[test]
fn matrix_zero_record_stream() {
    let text = b"[]";
    let stream = b"";
    let Value::Array(items) = common::parsed(text).expect("dom") else {
        panic!("not array")
    };
    let mut bad = Divergences::default();
    for (name, demands) in groups() {
        check_text(&mut bad, name, text, &items, &demands);
        check_stream(&mut bad, name, text, stream, &demands);
    }
    bad.assert_empty("zero-record divergences");
}

/// An oracle at an array element must not change a sibling projection's row law.
#[test]
fn count_oracle_at_an_array_element_leaves_a_sibling_projection_one_row() {
    let project = project_root(&["id"]);
    let src = br"[[1,2],[3]]";
    let alone = mat(&common::answer_of(src, &text(core::slice::from_ref(&project))));
    assert_eq!(alone, json_value("[{},{}]"));
    for (oracle, want) in [(Oracle::Count, 2_u64), (Oracle::DescendCount, 3_u64)] {
        let indexed = path_index(0, Some(Demand::Oracle(oracle.clone())));
        let demands = [project.clone(), indexed];
        let answers = common::answers(src, &text(&demands));
        assert_eq!(
            mat(&answers[0]),
            alone,
            "a {oracle:?} sibling changed the projection's rows"
        );
        assert_eq!(
            observe(&answers[1]),
            Obs::Oracle(format!("{:?}", OracleAnswer::Count(want))),
            "{oracle:?} answers the located element"
        );
    }
}

/// `Path{steps: []}` is its nested demand at the root; only the shard law keeps the wrapper.
#[test]
fn empty_path_spine_is_its_nested_demand_at_the_root() {
    let nested = project_key("rows", &["id"]);
    let degenerate = path(&[], Some(nested.clone()));
    let want = json_value(r#"[{"id":1},{"id":2}]"#);
    let src = br#"{"rows":[{"id":1},{"id":2}]}"#;
    assert_eq!(
        mat(&common::answer_of(src, &text(core::slice::from_ref(&nested)))),
        want
    );
    assert_eq!(
        mat(&common::answer_of(src, &text(core::slice::from_ref(&degenerate)))),
        want
    );
    let stream = b"{\"rows\":[{\"id\":1},{\"id\":2}]}\n";
    assert_eq!(
        mat(&common::answer_of(
            stream,
            &common::stream(core::slice::from_ref(&degenerate))
        )),
        want
    );
    assert_eq!(degenerate.shard(), structury::Shard::Serial);
    assert_eq!(nested.shard(), structury::Shard::Concat);
}

/// The accumulator stack is LIFO-safe: nested element loops and a triple-nested accumulator.
#[test]
fn nested_accumulators_are_lifo_safe() {
    let src = br"[[1,2],[3],[],[4]]";
    let result = common::scan_of(
        src,
        &text(&[collection(None, Some(collection(None, None))), Demand::Whole]),
    );
    assert_eq!(
        mat(&result.answers[0]),
        Value::Array(vec![num("1"), num("2"), num("3"), num("4")])
    );
    assert_eq!(mat(&result.answers[1]), common::parsed(src).expect("dom"));

    let deep = collection(None, Some(collection(None, Some(collection(None, None)))));
    let src = br"[[[1],[2]],[[3]],[[]]]";
    assert_eq!(
        mat(&common::scan_of(src, &text(core::slice::from_ref(&deep))).answers[0]),
        Value::Array(vec![num("1"), num("2"), num("3")])
    );
}

/// An inner multi-field batch must stay rows, not be re-shaped into one-cell rows.
#[test]
fn nested_collection_multi_field_batch_reshapes_rows() {
    let demand = collection(None, Some(collection(Some(&["id", "name"]), None)));
    let src = br#"[[{"id":1,"name":"a"},{"id":2,"name":"b"}]]"#;
    let got = mat(&common::answer_of(src, &text(core::slice::from_ref(&demand))));
    assert_eq!(
        got,
        Value::Array(vec![
            obj(&[("id", num("1")), ("name", Value::Str(CompactStr::from("a")))]),
            obj(&[("id", num("2")), ("name", Value::Str(CompactStr::from("b")))]),
        ])
    );
}

/// When a sibling `Whole` fuses the facts walk, a keyed `Project` must still
/// decode its spine key.
#[test]
fn fused_facts_keeps_a_nested_projection() {
    let root_demands = [Demand::Whole, project_key("rows", &["id"])];
    let root_src = br#"{"rows":[{"id":1}]}"#;
    let mut fused = text_dialect(&root_demands, Dialect::Jsonc);
    fused.facts = true;
    let plain = text_dialect(&root_demands, Dialect::Jsonc);
    assert_eq!(
        mat(&common::scan_of(root_src, &plain).answers[1]),
        Value::Array(vec![obj(&[("id", num("1"))])]),
        "collector (facts off)"
    );
    assert_eq!(
        mat(&common::scan_of(root_src, &fused).answers[1]),
        Value::Array(vec![obj(&[("id", num("1"))])]),
        "fused facts"
    );

    let nested = path_key("rows", Some(collection(None, Some(project(Path::key("a"), &["d"])))));
    let nested_demands = [Demand::Whole, nested];
    let nested_src = br#"{"rows":[{"a":{"d":1}},{"x":1}]}"#;
    let mut nested_fused = text_dialect(&nested_demands, Dialect::Jsonc);
    nested_fused.facts = true;
    let nested_plain = text_dialect(&nested_demands, Dialect::Jsonc);
    let want = Value::Array(vec![obj(&[("d", num("1"))]), obj(&[])]);
    assert_eq!(
        mat(&common::scan_of(nested_src, &nested_plain).answers[1]),
        want,
        "collector (facts off)"
    );
    assert_eq!(
        mat(&common::scan_of(nested_src, &nested_fused).answers[1]),
        want,
        "the nested projection must not depend on fact recording"
    );
}

/// A `Columns` answer binds the bytes its spans index, so no `(src, columns)` pair
/// can be mismatched.
#[test]
fn columns_answer_names_its_bound_source() {
    let src_a = br#"{"rows":[{"id":11},{"id":22}]}"#;
    let src_b = br#"{"rows":[{"id":99},{"id":88}]}"#;
    assert_eq!(src_b.len(), src_a.len(), "same offsets, different bytes");
    let demand = project_key("rows", &["id"]);
    let answer = common::answer_of(src_a, &text(core::slice::from_ref(&demand)));
    let Answer::Columns(columns) = &answer else {
        panic!("expected a column batch, got {answer:?}")
    };
    assert_eq!(columns.source(), src_a, "the batch names its own bytes");
    assert_eq!(
        mat(&answer),
        json_value(r#"[{"id":11},{"id":22}]"#),
        "read out of its bound source"
    );
}

/// A `BorrowedDocument` never outlives its source.
#[test]
fn borrowed_view_is_tied_to_the_source_lifetime() {
    let src: &[u8] = br#"{"a":1}"#;
    let span = ByteRange::try_new(0, src.len()).expect("range");
    let answer = Answer::Document(Document::from_span(src, span));
    let borrowed = common::mat_borrowed(&answer).expect("borrowed");
    assert_eq!(borrowed.root().member("a").expect("a").to_i64(), Some(1));
}

/// A fold across different sources is refused, not silently folded onto the
/// accumulator's bytes.
#[test]
#[should_panic(expected = "different sources")]
fn stitch_mixed_sources_is_refused() {
    let src_a = br#"{"rows":[{"id":11}]}"#;
    let src_b = br#"{"rows":[{"id":99}]}"#;
    assert_eq!(src_a.len(), src_b.len(), "same offsets, different bytes");
    let demand = project_key("rows", &["id"]);
    let ra = common::scan_of(src_a, &text(core::slice::from_ref(&demand)));
    let rb = common::scan_of(src_b, &text(core::slice::from_ref(&demand)));
    let _ = structury::stitch(vec![ra, rb]);
}

/// Identity, not content, is compared: equal bytes at different addresses.
#[test]
#[should_panic(expected = "different sources")]
fn stitch_equal_bytes_different_buffers_is_refused() {
    let src_a: Vec<u8> = br#"{"rows":[{"id":11}]}"#.to_vec();
    let src_b: Vec<u8> = br#"{"rows":[{"id":11}]}"#.to_vec();
    assert_eq!(src_a, src_b);
    assert_ne!(src_a.as_ptr(), src_b.as_ptr());
    let demand = project_key("rows", &["id"]);
    let ra = common::scan_of(&src_a, &text(core::slice::from_ref(&demand)));
    let rb = common::scan_of(&src_b, &text(core::slice::from_ref(&demand)));
    let _ = structury::stitch(vec![ra, rb]);
}

/// The same source check applies at the raw `Columns` level.
#[test]
#[should_panic(expected = "different sources")]
fn columns_append_rows_mixed_sources_is_refused() {
    let src_a: &[u8] = b"1111";
    let src_b: &[u8] = b"9999";
    let mut acc = Columns::new(src_a, vec!["$".into()]);
    acc.push(ColumnCell::Span(ByteRange::try_new(0, 3).expect("range")));
    let mut other = Columns::new(src_b, vec!["$".into()]);
    other.push(ColumnCell::Span(ByteRange::try_new(0, 3).expect("range")));
    acc.append_rows(&mut other);
}

/// The same-source fold must still append rows and stitch to the serial answer.
#[test]
fn same_source_folds_are_allowed() {
    let src: &[u8] = b"abcdef";
    let mut left = Columns::new(src, vec!["$".into()]);
    left.push(ColumnCell::Span(ByteRange::try_new(0, 1).expect("range")));
    let mut right = Columns::new(src, vec!["$".into()]);
    right.push(ColumnCell::Span(ByteRange::try_new(1, 2).expect("range")));
    left.append_rows(&mut right);
    assert_eq!(left.rows(), 2);

    let mut src = Vec::from(&b"{\"rows\":["[..]);
    for i in 0..30_000 {
        if i > 0 {
            src.push(b',');
        }
        src.extend_from_slice(format!("{{\"id\":{i}}}").as_bytes());
    }
    src.extend_from_slice(b"]}");
    let demand = project_key("rows", &["id"]);
    let request = text(core::slice::from_ref(&demand));
    let serial = common::scan_of(&src, &request);
    let ranges = plan_ranges(&src, &request).expect("plan");
    let parts: Vec<_> = ranges
        .iter()
        .map(|range| common::scan_range(&src, *range, &request).expect("shard"))
        .collect();
    assert_eq!(mat(&serial.answers[0]), mat(&stitch(parts).answers[0]));
}
