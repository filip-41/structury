//! Structural edit generality: insert/replace/clear on members and elements,
//! checked against a DOM mutate + serialize reference (`serde_json`) and with
//! byte-exact expectations for the glue cases.

mod common;

use common::key;

use structury::{Number, Step, Value};
use structury_json::{Dialect, Edit, EditOptions, validate};

fn num(text: &str) -> Value {
    Value::Number(Number::parse(text).expect("number"))
}

fn s(text: &str) -> Value {
    Value::Str(text.into())
}

fn to_serde(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => serde_json::from_str(n.spelling()).expect("finite number"),
        Value::Str(text) => serde_json::Value::String(text.as_str().to_owned()),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(to_serde).collect()),
        Value::Object(members) => {
            let mut map = serde_json::Map::new();
            for (key, value) in members {
                map.insert(key.as_str().to_owned(), to_serde(value));
            }
            serde_json::Value::Object(map)
        }
    }
}

/// Apply `edit` and assert the spliced output is Strict-valid under `dialect`
/// and parses to the same value as a serde DOM mutation of the input.
fn check(src: &str, edit: &Edit, mutate: impl FnOnce(&mut serde_json::Value), dialect: Dialect) {
    let before = common::parsed_dialect(src.as_bytes(), dialect).expect("input parses");
    let mut dom = to_serde(&before);
    mutate(&mut dom);
    let out = structury_json::edit(src.as_bytes(), core::slice::from_ref(edit), EditOptions::new(dialect))
        .unwrap_or_else(|error| panic!("edit failed for {src:?}: {error}"));
    validate(&out, dialect).unwrap_or_else(|error| panic!("invalid output {out:?}: {error}"));
    let after = common::parsed_dialect(&out, dialect).expect("output parses");
    assert_eq!(
        to_serde(&after),
        dom,
        "src={src:?} out={:?}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn append_and_prepend_members_and_elements() {
    check(
        r#"{"a":1}"#,
        &Edit::Insert {
            path: key("b"),
            value: num("2"),
        },
        |dom| {
            dom["b"] = serde_json::json!(2);
        },
        Dialect::Rfc8259,
    );
    check(
        "[1,2,3]",
        &Edit::Insert {
            path: vec![Step::Index(0)],
            value: num("9"),
        },
        |dom| dom.as_array_mut().unwrap().insert(0, serde_json::json!(9)),
        Dialect::Rfc8259,
    );
    check(
        "[1,2]",
        &Edit::Insert {
            path: vec![Step::Index(2)],
            value: num("3"),
        },
        |dom| dom.as_array_mut().unwrap().push(serde_json::json!(3)),
        Dialect::Rfc8259,
    );
    check(
        "[1,3]",
        &Edit::Insert {
            path: vec![Step::Index(1)],
            value: num("2"),
        },
        |dom| dom.as_array_mut().unwrap().insert(1, serde_json::json!(2)),
        Dialect::Rfc8259,
    );
}

#[test]
fn removing_first_middle_last_member_and_element() {
    for name in ["a", "b", "c"] {
        check(
            r#"{"a":1,"b":2,"c":3}"#,
            &Edit::Delete { path: key(name) },
            move |dom| {
                dom.as_object_mut().unwrap().remove(name);
            },
            Dialect::Rfc8259,
        );
    }
    for index in 0..3usize {
        check(
            "[1,2,3]",
            &Edit::Delete {
                path: vec![Step::Index(i64::try_from(index).expect("small index"))],
            },
            move |dom| {
                dom.as_array_mut().unwrap().remove(index);
            },
            Dialect::Rfc8259,
        );
    }
}

#[test]
fn single_member_and_element_containers() {
    check(
        r#"{"a":1}"#,
        &Edit::Delete { path: key("a") },
        |dom| {
            dom.as_object_mut().unwrap().remove("a");
        },
        Dialect::Rfc8259,
    );
    check(
        "[1]",
        &Edit::Delete {
            path: vec![Step::Index(0)],
        },
        |dom| {
            dom.as_array_mut().unwrap().remove(0);
        },
        Dialect::Rfc8259,
    );
    check(
        "{}",
        &Edit::Insert {
            path: key("a"),
            value: num("1"),
        },
        |dom| {
            dom["a"] = serde_json::json!(1);
        },
        Dialect::Rfc8259,
    );
    check(
        "[]",
        &Edit::Insert {
            path: vec![Step::Index(0)],
            value: num("1"),
        },
        |dom| dom.as_array_mut().unwrap().push(serde_json::json!(1)),
        Dialect::Rfc8259,
    );
}

#[test]
fn replace_member_rewrites_key_and_value() {
    check(
        r#"{"a" : 1, "b":2}"#,
        &Edit::ReplaceMember {
            path: key("a"),
            key: "x".into(),
            value: num("9"),
        },
        |dom| {
            let object = dom.as_object_mut().unwrap();
            object.remove("a");
            object.insert("x".into(), serde_json::json!(9));
        },
        Dialect::Rfc8259,
    );
    check(
        r#"{"a":{"b":1}}"#,
        &Edit::ReplaceMember {
            path: vec![Step::Key("a".into()), Step::Key("b".into())],
            key: "c".into(),
            value: s("v"),
        },
        |dom| {
            let inner = dom["a"].as_object_mut().unwrap();
            inner.remove("b");
            inner.insert("c".into(), serde_json::json!("v"));
        },
        Dialect::Rfc8259,
    );
}

#[test]
fn replace_member_onto_an_existing_key_reencodes() {
    let out = structury_json::edit(
        br#"{"a":1,"b":2}"#,
        &[Edit::ReplaceMember {
            path: key("a"),
            key: "b".into(),
            value: num("9"),
        }],
        EditOptions::new(Dialect::Rfc8259),
    )
    .expect("fallback");
    assert_eq!(out, br#"{"b":9}"#, "one member, not a duplicate");
}

#[test]
fn clear_empties_a_container() {
    check(
        r#"{"a":1,"b":[2,3]}"#,
        &Edit::Clear { path: key("b") },
        |dom| dom["b"] = serde_json::json!([]),
        Dialect::Rfc8259,
    );
    check(
        r#"[1,{"a":2},3]"#,
        &Edit::Clear {
            path: vec![Step::Index(1)],
        },
        |dom| dom.as_array_mut().unwrap()[1] = serde_json::json!({}),
        Dialect::Rfc8259,
    );
    check(
        r#"{"a":1,"b":2}"#,
        &Edit::Clear { path: Vec::new() },
        |dom| *dom = serde_json::json!({}),
        Dialect::Rfc8259,
    );
}

#[test]
fn nested_paths_insert_delete_and_clear() {
    check(
        r#"{"a":{"b":[1,2]}}"#,
        &Edit::Insert {
            path: vec![Step::Key("a".into()), Step::Key("b".into()), Step::Index(1)],
            value: num("9"),
        },
        |dom| dom["a"]["b"].as_array_mut().unwrap().insert(1, serde_json::json!(9)),
        Dialect::Rfc8259,
    );
    check(
        r#"{"a":{"b":[1,2,3]}}"#,
        &Edit::Delete {
            path: vec![Step::Key("a".into()), Step::Key("b".into()), Step::Index(1)],
        },
        |dom| {
            dom["a"]["b"].as_array_mut().unwrap().remove(1);
        },
        Dialect::Rfc8259,
    );
    check(
        r#"{"a":{"b":[1,2,3]},"c":4}"#,
        &Edit::Clear {
            path: vec![Step::Key("a".into())],
        },
        |dom| dom["a"] = serde_json::json!({}),
        Dialect::Rfc8259,
    );
}

#[test]
fn escaping_in_inserted_keys_and_values() {
    let name = "a\t\"\nb";
    check(
        r#"{"keep":1}"#,
        &Edit::Insert {
            path: key(name),
            value: s("x\u{1f600}\"\\\n"),
        },
        |dom| {
            dom.as_object_mut()
                .unwrap()
                .insert(name.into(), serde_json::json!("x\u{1f600}\"\\\n"));
        },
        Dialect::Rfc8259,
    );
    check(
        r#"{"a":1}"#,
        &Edit::ReplaceMember {
            path: key("a"),
            key: "\u{20ac}".into(),
            value: s("\t"),
        },
        |dom| {
            let object = dom.as_object_mut().unwrap();
            object.remove("a");
            object.insert("\u{20ac}".into(), serde_json::json!("\t"));
        },
        Dialect::Rfc8259,
    );
}

#[test]
fn trailing_comma_dialect_keeps_output_valid() {
    // Appending to a trailing-comma array must not double the comma; deleting
    // from one must not leave an RFC trailing comma.
    check(
        r#"{"a":1,"b":2,}"#,
        &Edit::Delete { path: key("b") },
        |dom| {
            dom.as_object_mut().unwrap().remove("b");
        },
        Dialect::Jsonc,
    );
    check(
        "[1,2,]",
        &Edit::Insert {
            path: vec![Step::Index(2)],
            value: num("3"),
        },
        |dom| dom.as_array_mut().unwrap().push(serde_json::json!(3)),
        Dialect::Jsonc,
    );
    check(
        "[1,2,]",
        &Edit::Delete {
            path: vec![Step::Index(1)],
        },
        |dom| {
            dom.as_array_mut().unwrap().remove(1);
        },
        Dialect::Jsonc,
    );
    check(
        r#"{"a":1,}"#,
        &Edit::Clear { path: Vec::new() },
        |dom| *dom = serde_json::json!({}),
        Dialect::Jsonc,
    );
}

#[test]
fn comment_dialect_structural_edits_keep_output_valid() {
    check(
        "{\n  // lead\n  \"a\": 1, // inline\n  \"b\": 2\n}",
        &Edit::Insert {
            path: key("c"),
            value: num("3"),
        },
        |dom| {
            dom["c"] = serde_json::json!(3);
        },
        Dialect::Jsonc,
    );
    check(
        "{\n  \"a\": 1, /* inter */ \"b\": 2, // tail\n}",
        &Edit::Delete { path: key("b") },
        |dom| {
            dom.as_object_mut().unwrap().remove("b");
        },
        Dialect::Jsonc,
    );
    check(
        "[ /* one */ 1, 2, /* tail */ ]",
        &Edit::Delete {
            path: vec![Step::Index(0)],
        },
        |dom| {
            dom.as_array_mut().unwrap().remove(0);
        },
        Dialect::Jsonc,
    );
    check(
        "[ 1, /* keep */ [2, 3] ]",
        &Edit::Clear {
            path: vec![Step::Index(1)],
        },
        |dom| dom.as_array_mut().unwrap()[1] = serde_json::json!([]),
        Dialect::Jsonc,
    );
}

#[test]
fn malformed_structural_edits_are_refused() {
    for (src, edit) in [
        (
            r#"{"a":1}"#,
            Edit::ReplaceMember {
                path: key("missing"),
                key: "x".into(),
                value: num("2"),
            },
        ),
        (r#"{"a":1}"#, Edit::Clear { path: key("a") }),
        (
            "[1,2]",
            Edit::Insert {
                path: vec![Step::Index(5)],
                value: num("3"),
            },
        ),
    ] {
        assert!(
            structury_json::edit(src.as_bytes(), &[edit], EditOptions::new(Dialect::Rfc8259)).is_err(),
            "src={src:?}"
        );
    }
}

#[test]
fn append_keeps_sibling_glue_and_drops_trailing_glue() {
    // The appended member renders compact; the earlier member keeps its glue.
    let out = structury_json::edit(
        br#"{ "a" : 1 }"#,
        &[Edit::Insert {
            path: key("b"),
            value: num("2"),
        }],
        EditOptions::new(Dialect::Rfc8259),
    )
    .expect("append");
    assert_eq!(out, br#"{ "a" : 1,"b":2}"#);

    let out = structury_json::edit(
        b"[ 1, 2 ]",
        &[Edit::Insert {
            path: vec![Step::Index(2)],
            value: num("3"),
        }],
        EditOptions::new(Dialect::Rfc8259),
    )
    .expect("append");
    assert_eq!(out, b"[ 1, 2,3]");
}

#[test]
fn append_into_a_trailing_comma_drops_the_trailing_comma() {
    let out = structury_json::edit(
        br#"{"a":1,}"#,
        &[Edit::Insert {
            path: key("b"),
            value: num("2"),
        }],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("append");
    assert_eq!(out, br#"{"a":1,"b":2}"#);
}

#[test]
fn delete_removes_a_members_leading_comment() {
    // An interstitial comment belongs to the member it leads, so deleting that
    // member takes it (next/ semantics).
    let out = structury_json::edit(
        br#"{"a":1, // inter
"b":2}"#,
        &[Edit::Delete { path: key("b") }],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("delete");
    assert_eq!(out, br#"{"a":1}"#);

    // Deleting the earlier member keeps the interstitial on the later one.
    let out = structury_json::edit(
        br#"{"a":1, // inter
"b":2}"#,
        &[Edit::Delete { path: key("a") }],
        EditOptions::new(Dialect::Jsonc),
    )
    .expect("delete");
    assert_eq!(
        out,
        b"{ // inter\n\"b\":2}",
        "the comment stays with b: {}",
        String::from_utf8_lossy(&out)
    );
}
