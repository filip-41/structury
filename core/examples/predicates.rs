//! Filter owned rows with `Predicate`, the evaluator behind filter demands.

use structury::{Number, Predicate, Value};

/// One exact number from its authored spelling.
fn number(spelling: &str) -> Value {
    Value::Number(Number::parse(spelling).expect("a base-ten spelling"))
}

/// One row with a name and a score.
fn row(name: &str, score: &str) -> Value {
    Value::Object(vec![
        ("name".into(), Value::Str(name.into())),
        ("score".into(), number(score)),
    ])
}

/// Score at least 85 and name is not "eve".
fn passing() -> Predicate {
    Predicate::And(
        Box::new(Predicate::Ge {
            field: "score".into(),
            value: number("85"),
        }),
        Box::new(Predicate::Not(Box::new(Predicate::Eq {
            field: "name".into(),
            value: Value::Str("eve".into()),
        }))),
    )
}

fn main() {
    let rows = [row("ada", "91"), row("bob", "72"), row("eve", "85")];
    let passing = passing();

    let names: Vec<&str> = rows
        .iter()
        .filter(|row| passing.matches(row))
        .filter_map(|row| row.member("name"))
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(names, ["ada"]);

    // A value that is not an object never matches, and an absent field never
    // matches an ordering arm. Only `Ne` matches an absent field.
    let absent = Predicate::Gt {
        field: "missing".into(),
        value: number("0"),
    };
    assert!(!absent.matches(&row("bob", "72")));
    assert!(!absent.matches(&Value::Null));
}
