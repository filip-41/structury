//! Demand construction and the `Shard` law over paths and collections.

use structury::{CompactStr, Demand, Number, Oracle, Path, Predicate, Range, Shard, Step, Value};

fn path_to(nested: Demand) -> Demand {
    Demand::Path {
        steps: vec![Step::Key("rows".into())],
        nested: Some(Box::new(nested)),
    }
}

fn collection(nested: Option<Demand>) -> Demand {
    Demand::Collection {
        fields: None,
        nested: nested.map(Box::new),
    }
}

fn flat_project() -> Demand {
    Demand::Project {
        path: Path::root(),
        fields: vec!["id".into()],
    }
}

fn slice() -> Demand {
    Demand::Slice {
        range: Range { start: None, end: None },
        nested: None,
    }
}

#[test]
fn shard_kind_is_recursive_through_a_path_spine() {
    // A bare `Whole` is not on the whitelist (a text document is one value).
    assert_eq!(Demand::Whole.shard(), Shard::Serial);
    assert_eq!(Demand::Oracle(Oracle::Count).shard(), Shard::Sum);
    assert_eq!(Demand::Oracle(Oracle::DescendCount).shard(), Shard::Serial);
    assert_eq!(Demand::Oracle(Oracle::Kind).shard(), Shard::Serial);
    // A key-only `Path` to a count is the located array's element count.
    assert_eq!(path_to(Demand::Oracle(Oracle::Count)).shard(), Shard::Sum);
    assert_eq!(path_to(Demand::Oracle(Oracle::Kind)).shard(), Shard::Serial);
    assert_eq!(path_to(Demand::Whole).shard(), Shard::Serial);
    assert_eq!(Demand::path(vec![Step::Key("id".into())]).shard(), Shard::Serial);
}

/// Only a whitelisted per-element row demand shards under a `Collection`; every
/// other nested variant is `Serial`, so a new one cannot leak in.
#[test]
fn shard_kind_is_a_proven_whitelist_through_a_collection() {
    assert_eq!(collection(Some(flat_project())).shard(), Shard::Concat);
    assert_eq!(collection(None).shard(), Shard::Concat);
    assert_eq!(collection(Some(slice())).shard(), Shard::Serial);
    assert_eq!(collection(Some(collection(Some(slice())))).shard(), Shard::Serial);
    assert_eq!(
        collection(Some(Demand::Path {
            steps: vec![],
            nested: Some(Box::new(slice())),
        }))
        .shard(),
        Shard::Serial
    );
    // A spent spine is *not* normalised away: json maps `Collection{spent
    // Path{[]}→Project}` through its per-value fallback, which diverges on a
    // root array of arrays, so the wrapper is refused.
    assert_eq!(
        collection(Some(Demand::Path {
            steps: vec![],
            nested: Some(Box::new(flat_project())),
        }))
        .shard(),
        Shard::Serial
    );
    assert_eq!(collection(Some(Demand::Whole)).shard(), Shard::Serial);
    // A count oracle under the container is a row law json drops; the whitelist
    // refuses it rather than folding it per element.
    let count = || Demand::Oracle(Oracle::Count);
    assert_eq!(collection(Some(count())).shard(), Shard::Serial);
    assert_eq!(
        collection(Some(Demand::Path {
            steps: vec![Step::Key("x".into())],
            nested: Some(Box::new(count())),
        }))
        .shard(),
        Shard::Serial
    );
}

#[test]
fn only_serial_declines_a_shard() {
    assert!(Shard::Concat.is_parallel());
    assert!(Shard::Sum.is_parallel());
    assert!(!Shard::Serial.is_parallel());
}

#[test]
fn predicate_matches_by_value_and_structure() {
    let row = Value::Object(vec![
        (
            CompactStr::from("n"),
            Value::Number(Number::parse("1.50").expect("number")),
        ),
        (CompactStr::from("s"), Value::Str(CompactStr::from("x"))),
        (CompactStr::from("b"), Value::Bool(true)),
    ]);
    let eq_number = Predicate::Eq {
        field: String::from("n"),
        value: Value::Number(Number::parse("1.5").expect("number")),
    };
    assert!(eq_number.matches(&row), "1.50 equals 1.5 by value");
    assert!(
        Predicate::Eq {
            field: String::from("s"),
            value: Value::Str(CompactStr::from("x")),
        }
        .matches(&row)
    );
    assert!(
        !Predicate::Eq {
            field: String::from("s"),
            value: Value::Str(CompactStr::from("y")),
        }
        .matches(&row)
    );
    assert!(
        Predicate::Gt {
            field: String::from("n"),
            value: Value::Number(Number::parse("1").expect("number")),
        }
        .matches(&row)
    );
    assert!(
        Predicate::Ge {
            field: String::from("b"),
            value: Value::Number(Number::parse("1").expect("number")),
        }
        .matches(&row),
        "true counts as 1"
    );
    assert!(
        Predicate::Ne {
            field: String::from("absent"),
            value: Value::Null,
        }
        .matches(&row)
    );
    assert!(
        !Predicate::And(
            Box::new(eq_number),
            Box::new(Predicate::Not(Box::new(Predicate::Eq {
                field: String::from("s"),
                value: Value::Str(CompactStr::from("x")),
            }))),
        )
        .matches(&row)
    );
}

#[test]
fn predicate_equality_compares_container_operands_order_insensitively() {
    let row = Value::Object(vec![(
        CompactStr::from("tags"),
        Value::Object(vec![
            (CompactStr::from("a"), Value::Bool(true)),
            (CompactStr::from("b"), Value::Bool(false)),
        ]),
    )]);
    let want = Predicate::Eq {
        field: String::from("tags"),
        value: Value::Object(vec![
            (CompactStr::from("b"), Value::Bool(false)),
            (CompactStr::from("a"), Value::Bool(true)),
        ]),
    };
    assert!(
        want.matches(&row),
        "an object operand compares by member name, ignoring order"
    );
    assert!(
        !Predicate::Eq {
            field: String::from("tags"),
            value: Value::Array(Vec::new()),
        }
        .matches(&row),
        "a different container kind is unequal"
    );
}

#[test]
fn path_and_demand_builders_compose() {
    let path = Path::key("rows").push_index(0).push_key("id");
    assert_eq!(path.steps, vec![Step::key("rows"), Step::index(0), Step::key("id")]);
    let demand = Demand::path([Step::key("rows"), Step::index(0)]).nested(Demand::collection(Some(vec!["id".into()])));
    assert_eq!(
        demand,
        Demand::Path {
            steps: vec![Step::key("rows"), Step::index(0)],
            nested: Some(Box::new(Demand::Collection {
                fields: Some(vec!["id".into()]),
                nested: None,
            })),
        }
    );
}
