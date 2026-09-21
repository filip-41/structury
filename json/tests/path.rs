//! Path demand: nested keys, negative index, type mismatch, and spent
//! `Path{[]}` normalisation.

mod common;

use crate::common::*;

use structury::{Answer, Demand, Step, ValueKind};
use structury_json::{Dialect, JsonInput, ScanRequest, scan};

fn one<'a>(bytes: &'a [u8], demand: &Demand) -> Answer<'a> {
    common::answer_of(bytes, &common::text(core::slice::from_ref(demand)))
}

#[test]
fn nested_path() {
    let mark = one(
        br#"{"users":[{"id":7}]}"#,
        &Demand::path(vec![Step::Key("users".into()), Step::Index(0), Step::Key("id".into())]),
    );
    let v = common::mat_value(&mark).expect("mat");
    match v {
        structury::Value::Number(n) => assert_eq!(n.spelling(), "7"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn key_on_array_is_type_mismatch() {
    let mark = one(b"[1,2]", &Demand::path(vec![Step::Key("a".into())]));
    assert!(matches!(
        mark,
        Answer::TypeMismatch {
            actual: ValueKind::Array
        }
    ));
}

/// A spent `Path{[]}` is its nested demand in the row law too: a `Collection`
/// element with a spent path is one row per element.
#[test]
fn spent_path_inside_collection_is_the_nested_demand() {
    let src: &[u8] = br#"{"rows":[{"d":1},5,{"d":3}]}"#;
    let direct = path_key("rows", Some(collection(None, Some(project_root(&["d"])))));
    let spent = path_key(
        "rows",
        Some(collection(None, Some(path(&[], Some(project_root(&["d"])))))),
    );
    let a = obs_text(src, &direct);
    let b = obs_text(src, &spent);
    println!("collection direct: {a}");
    println!("collection spent : {b}");
    assert_eq!(
        a, b,
        "spent Path{{[]}} nested demand diverges from the direct projection (Collection)"
    );
}

/// The same spent-path law inside a `Slice`.
#[test]
fn spent_path_inside_slice_is_the_nested_demand() {
    let src: &[u8] = br#"[{"d":1},5,{"d":3}]"#;
    let direct = slice_nested(None, None, project_root(&["d"]));
    let spent = slice_nested(None, None, path(&[], Some(project_root(&["d"]))));
    let a = obs_text(src, &direct);
    let b = obs_text(src, &spent);
    println!("slice direct: {a}");
    println!("slice spent : {b}");
    assert_eq!(
        a, b,
        "spent Path{{[]}} nested demand diverges from the direct projection (Slice)"
    );
}

/// JSON5 block comment and a spent `Path{[]}` in a `Collection` at a JSON5
/// dialect, to make sure the law is dialect-independent.
#[test]
fn spent_path_law_is_dialect_independent() {
    let src: &[u8] = b"{rows:[{d:1},5,{d:3}]}";
    let direct = path_key("rows", Some(collection(None, Some(project_root(&["d"])))));
    let spent = path_key(
        "rows",
        Some(collection(None, Some(path(&[], Some(project_root(&["d"])))))),
    );
    let run = |d: Demand| {
        let demands = [d];
        let request = ScanRequest::new(JsonInput::Text, &demands).with_dialect(Dialect::Json5);
        format!("{:?}", observe_all(&scan(src, &request).unwrap().answers))
    };
    println!("json5 direct: {}", run(direct.clone()));
    println!("json5 spent : {}", run(spent.clone()));
    assert_eq!(run(direct), run(spent), "spent-path law is dialect-independent");
}

#[test]
fn spent_path_at_depth_two_and_siblings() {
    let src = br#"{"rows":[{"inner":[{"d":1},5]},{"inner":[{"d":2}]}]}"#;
    let direct = path_key(
        "rows",
        Some(collection(
            None,
            Some(Demand::Path {
                steps: vec![Step::Key("inner".into())],
                nested: Some(Box::new(collection(None, Some(project_root(&["d"]))))),
            }),
        )),
    );
    let spent = path_key(
        "rows",
        Some(collection(
            None,
            Some(spent_path(Demand::Path {
                steps: vec![Step::Key("inner".into())],
                nested: Some(Box::new(collection(None, Some(spent_path(project_root(&["d"])))))),
            })),
        )),
    );
    let a = obs(src, &direct, Dialect::Rfc8259);
    let b = obs(src, &spent, Dialect::Rfc8259);
    println!("depth2 direct: {a}");
    println!("depth2 spent : {b}");
    assert_eq!(a, b, "spent path at depth >=2");

    let json5: &[u8] = b"{rows:[{inner:[{d:1},5]},{inner:[{d:2}]}]}";
    let a5 = obs(
        json5,
        &path_key("rows", Some(collection(None, Some(project_root(&["d"]))))),
        Dialect::Json5,
    );
    let b5 = obs(
        json5,
        &path_key("rows", Some(collection(None, Some(spent_path(project_root(&["d"])))))),
        Dialect::Json5,
    );
    println!("json5 direct: {a5}");
    println!("json5 spent : {b5}");
}

#[test]
fn spent_path_with_filter_and_project() {
    let src = br#"[{"n":1},{"n":0},5]"#;
    let direct = collection(None, Some(filter_root(gt("n", "0"), &["n"])));
    let spent = collection(None, Some(spent_path(filter_root(gt("n", "0"), &["n"]))));
    let a = obs(src, &direct, Dialect::Rfc8259);
    let b = obs(src, &spent, Dialect::Rfc8259);
    println!("filter direct: {a}");
    println!("filter spent : {b}");
    assert_eq!(a, b, "spent Path{{[]}} wrapping a Filter inside a Collection");

    let directp = collection(None, Some(project_root(&["n"])));
    let spentp = collection(None, Some(spent_path(project_root(&["n"]))));
    let a = obs(src, &directp, Dialect::Rfc8259);
    let b = obs(src, &spentp, Dialect::Rfc8259);
    assert_eq!(a, b, "spent Path{{[]}} wrapping a Project inside a Collection");
}

#[test]
fn multi_layer_spent_path_matches_direct() {
    let src = br#"{"rows":[{"d":1},5,{"d":3}]}"#;
    let nested = project_root(&["d"]);
    let wrap = |demand: Demand| path_key("rows", Some(collection(None, Some(demand))));
    let want = obs(src, &wrap(nested.clone()), Dialect::Rfc8259);
    for (name, demand) in [
        ("one", wrap(spent_path(nested.clone()))),
        ("two", wrap(spent_path(spent_path(nested.clone())))),
        ("three", wrap(spent_path(spent_path(spent_path(nested.clone()))))),
    ] {
        assert_eq!(
            obs(src, &demand, Dialect::Rfc8259),
            want,
            "{name} spent Path{{[]}} layers must fold to the nested demand"
        );
    }
}
