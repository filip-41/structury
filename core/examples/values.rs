//! Build a small value tree, walk it, and compare identities.

use structury::{Number, Value};

/// One exact number from its authored spelling.
fn number(spelling: &str) -> Value {
    Value::Number(Number::parse(spelling).expect("a base-ten spelling"))
}

fn main() {
    let alice = Value::Object(vec![
        ("name".into(), Value::Str("alice".into())),
        ("score".into(), number("90")),
    ]);
    let bob = Value::Object(vec![
        ("name".into(), Value::Str("bob".into())),
        ("score".into(), number("72.50")),
    ]);
    let users = Value::Array(vec![alice, bob]);

    // `element` takes a signed index, so -1 is the last element.
    let first_name = users.element(0).and_then(|row| row.member("name"));
    assert_eq!(first_name.and_then(Value::as_str), Some("alice"));
    let last_score = users.element(-1).and_then(|row| row.member("score"));
    assert_eq!(last_score.and_then(Value::as_f64), Some(72.5));

    // `==` compares by value, so two spellings of one number are equal.
    assert_eq!(number("90"), number("90.0"));
    // `strict_equal` keeps the authored spelling, so those two differ.
    assert!(!number("90").strict_equal(&number("90.0")));

    // Object member order carries no meaning, so a reordered object is equal.
    let reordered = Value::Object(vec![
        ("score".into(), number("90")),
        ("name".into(), Value::Str("alice".into())),
    ]);
    assert_eq!(users.element(0).expect("one row"), &reordered);

    println!("{users}");
}
