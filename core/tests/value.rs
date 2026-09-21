//! Semantic value equality, ordering, navigation, and iterators.

use std::string::ToString;

use structury::{CompactStr, NonFinite, Number, Value, resolve_index};

fn num(spelling: &str) -> Value {
    Value::Number(Number::parse(spelling).expect("number"))
}

fn obj(members: &[(&str, Value)]) -> Value {
    Value::Object(
        members
            .iter()
            .map(|(key, value)| (CompactStr::from(*key), value.clone()))
            .collect(),
    )
}

/// Compile-time proof that a type implements `Eq`.
fn assert_eq_bound<T: Eq>() {}

#[test]
fn accessors_answer_only_their_variant() {
    assert_eq!(Value::Null.as_str(), None);
    assert_eq!(Value::Null.as_bool(), None);
    assert_eq!(Value::Null.as_array(), None);
    assert_eq!(Value::Null.as_object(), None);

    assert_eq!(Value::Bool(true).as_bool(), Some(true));
    assert_eq!(Value::Bool(true).as_i64(), None);
    assert_eq!(num("1").as_i64(), Some(1));
    assert_eq!(num("1").as_bool(), None);
    assert_eq!(Value::Str(CompactStr::from("x")).as_str(), Some("x"));
    assert_eq!(Value::Str(CompactStr::from("x")).as_array(), None);
    assert_eq!(Value::Array(vec![]).as_array(), Some(&[][..]));
    assert_eq!(Value::Array(vec![]).as_object(), None);
    assert_eq!(Value::Object(vec![]).as_object(), Some(&[][..]));
    assert_eq!(Value::Object(vec![]).as_array(), None);
}

#[test]
fn member_is_last_wins_over_duplicates() {
    let value = Value::Object(vec![
        (CompactStr::from("a"), Value::Bool(false)),
        (CompactStr::from("a"), Value::Bool(true)),
    ]);
    assert_eq!(value.member("a").and_then(Value::as_bool), Some(true));
    assert_eq!(value.member("missing"), None);
    assert_eq!(value.members().count(), 2);
}

#[test]
fn index_resolves_arrays_and_negatives() {
    let value = Value::Array(vec![num("10"), num("20"), num("30")]);
    assert_eq!(value.element(0).and_then(Value::as_i64), Some(10));
    assert_eq!(value.element(2).and_then(Value::as_i64), Some(30));
    assert_eq!(value.element(-1).and_then(Value::as_i64), Some(30));
    assert_eq!(value.element(-3).and_then(Value::as_i64), Some(10));
    assert_eq!(value.element(3), None);
    assert_eq!(value.element(-4), None);
    assert_eq!(Value::Str(CompactStr::from("x")).element(0), None);
    assert_eq!(Value::Object(vec![]).element(0), None);
}

#[test]
fn iterators_are_empty_for_the_wrong_variant() {
    assert_eq!(Value::Array(vec![]).members().count(), 0);
    assert_eq!(Value::Object(vec![]).elements().count(), 0);
    assert_eq!(Value::Null.elements().count(), 0);
}

#[test]
fn iterators_keep_member_order() {
    let value = Value::Object(vec![
        (CompactStr::from("b"), Value::Bool(true)),
        (CompactStr::from("a"), Value::Null),
    ]);
    let names: Vec<&str> = value.members().map(|(name, _)| name).collect();
    assert_eq!(names, vec!["b", "a"]);
    assert_eq!(Value::Array(vec![num("1"), num("2")]).elements().count(), 2);
}

#[test]
fn display_is_compact_json_shaped() {
    let value = Value::Object(vec![(
        CompactStr::from("k"),
        Value::Array(vec![
            Value::Str(CompactStr::from("a\nb")),
            num("1.50"),
            Value::Bool(false),
            Value::Null,
        ]),
    )]);
    assert_eq!(value.to_string(), r#"{"k":["a\nb",1.50,false,null]}"#);
}

#[test]
fn display_escapes_control_bytes_and_keeps_unicode() {
    let value = Value::Str(CompactStr::from("q\"b\\c\n\t\u{1} café €"));
    assert_eq!(value.to_string(), r#""q\"b\\c\n\t\u0001 café €""#);
}

#[test]
fn display_renders_empty_containers_and_a_non_finite_name() {
    assert_eq!(Value::Array(vec![]).to_string(), "[]");
    assert_eq!(Value::Object(vec![]).to_string(), "{}");
    assert_eq!(Value::Null.to_string(), "null");
    assert_eq!(Value::Bool(true).to_string(), "true");
    assert_eq!(
        Value::Number(Number::NonFinite(NonFinite::Infinity)).to_string(),
        "Infinity"
    );
    assert_eq!(num("-0").to_string(), "-0", "authored spelling is kept");
}

#[test]
fn resolve_index_bounds() {
    assert_eq!(resolve_index(3, 0), Some(0));
    assert_eq!(resolve_index(3, 2), Some(2));
    assert_eq!(resolve_index(3, 3), None);
    assert_eq!(resolve_index(3, -1), Some(2));
    assert_eq!(resolve_index(3, -3), Some(0));
    assert_eq!(resolve_index(3, -4), None);
    assert_eq!(resolve_index(3, i64::MIN), None);
    assert_eq!(resolve_index(0, 0), None);
}

#[test]
fn equal_is_numeric_while_strict_equal_is_spelling() {
    let one_five = num("1.5");
    let one_fifty = num("1.50");
    assert!(one_fifty.equal(&one_five), "1.50 equals 1.5 by value");
    assert_eq!(one_fifty, one_five, "== is the semantic notion");
    assert!(!one_fifty.strict_equal(&one_five), "spellings differ");
    assert!(one_fifty.strict_equal(&num("1.50")));
    assert!(Value::Str(CompactStr::from("x")).equal(&Value::Str(CompactStr::from("x"))));
    assert!(!Value::Bool(true).equal(&num("1")), "equality does not coerce bool");
    let a = Value::Array(vec![num("1.50")]);
    let b = Value::Array(vec![num("1.5")]);
    assert!(a.equal(&b), "containers descend semantically");
    assert!(!a.strict_equal(&b), "the spelling survives inside a container");
}

#[test]
fn object_equality_ignores_member_order() {
    let ab = obj(&[("a", num("1")), ("b", num("2"))]);
    let ba = obj(&[("b", num("2")), ("a", num("1"))]);
    assert!(ab.equal(&ba), "member order is not significant");
    assert_eq!(ab, ba, "== is the semantic notion");
    assert!(!ab.strict_equal(&ba), "the strict notion keeps order");
    assert!(ab.equal(&ab.clone()), "reflexive");

    let nested = obj(&[("o", obj(&[("x", num("1")), ("y", num("2"))]))]);
    let reordered = obj(&[("o", obj(&[("y", num("2")), ("x", num("1"))]))]);
    assert!(nested.equal(&reordered), "nested objects compare under the same rule");

    assert!(!ab.equal(&obj(&[("b", num("3")), ("a", num("1"))])), "value differs");
    assert!(!ab.equal(&obj(&[("c", num("2")), ("a", num("1"))])), "name differs");
    assert!(!ab.equal(&obj(&[("a", num("1"))])), "count differs");
    assert_eq!(ab.equal(&ba), ba.equal(&ab), "symmetric");
}

#[test]
fn duplicate_keys_compare_their_pairs_not_last_wins() {
    let dup = obj(&[("k", num("1")), ("k", num("2"))]);
    assert!(dup.equal(&obj(&[("k", num("1")), ("k", num("2"))])), "identical pairs");
    assert!(!dup.equal(&obj(&[("k", num("2"))])), "counts differ");
    assert!(!dup.equal(&obj(&[("k", num("0")), ("k", num("2"))])), "pairs differ");
}

#[test]
fn equal_is_reflexive_for_non_finite_values() {
    assert!(Value::Number(Number::NonFinite(NonFinite::NaN)).equal(&Value::Number(Number::NonFinite(NonFinite::NaN))));
    assert!(
        Value::Number(Number::NonFinite(NonFinite::Infinity))
            .equal(&Value::Number(Number::NonFinite(NonFinite::Infinity)))
    );
    assert!(
        !Value::Number(Number::NonFinite(NonFinite::NaN)).equal(&Value::Number(Number::NonFinite(NonFinite::Infinity)))
    );
    // `Eq` survives because the non-finite arm compares by variant.
    assert_eq_bound::<Value>();
}

#[test]
fn compare_is_numeric_with_bool_coercion() {
    use core::cmp::Ordering;

    assert_eq!(num("2").compare(&num("10")), Some(Ordering::Less));
    assert_eq!(num("1e2").compare(&num("100")), Some(Ordering::Equal));
    assert_eq!(Value::Bool(true).compare(&num("1")), Some(Ordering::Equal));
    assert_eq!(Value::Bool(false).compare(&Value::Bool(true)), Some(Ordering::Less));
    assert_eq!(num("1").compare(&Value::Str(CompactStr::from("1"))), None);
    assert_eq!(Value::Str(CompactStr::from("x")).compare(&num("1")), None);
    assert_eq!(Value::Null.compare(&Value::Null), None);
}
