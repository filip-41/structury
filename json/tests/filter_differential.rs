//! The span filter and the core value predicate must agree on every row shape
//! and operand kind they both serve.

use structury::{Answer, ByteRange, ColumnCell, Demand, Number, Path, Predicate, Value};
use structury_json::{Dialect, Form, JsonInput, MaterializeOptions, Materialized, ScanRequest, parse, scan};

/// One root array, and one array under a key: the two path shapes a filter maps.
const DOCS: &[(&[u8], Option<&str>)] = &[
    (
        br#"[{"a":1,"b":"x","c":true},{"a":2,"b":"y","c":false},{"a":3,"b":"x"},{"a":1.0,"b":"z"},{"b":"x"},{"a":null,"b":null,"c":null},7,[1,2],"scalar"]"#,
        None,
    ),
    (
        br#"{"rows":[{"a":1,"b":"x"},{"a":2},{"a":1e0,"b":"x"},[],null]}"#,
        Some("rows"),
    ),
];

fn number(spelling: &str) -> Value {
    Value::Number(Number::parse(spelling).expect("a number spelling"))
}

#[allow(
    clippy::too_many_lines,
    reason = "one table of every predicate shape the differential covers, clearer whole than split by operator"
)]
fn cases() -> Vec<(&'static str, Predicate)> {
    let field = |name: &str| structury::Name::from(name);
    vec![
        (
            "a",
            Predicate::Eq {
                field: field("a"),
                value: number("1"),
            },
        ),
        (
            "a",
            Predicate::Ne {
                field: field("a"),
                value: number("1"),
            },
        ),
        (
            "a",
            Predicate::Gt {
                field: field("a"),
                value: number("1"),
            },
        ),
        (
            "a",
            Predicate::Ge {
                field: field("a"),
                value: number("1"),
            },
        ),
        (
            "a",
            Predicate::Lt {
                field: field("a"),
                value: number("2"),
            },
        ),
        (
            "a",
            Predicate::Le {
                field: field("a"),
                value: number("2"),
            },
        ),
        (
            "a",
            Predicate::Eq {
                field: field("a"),
                value: number("1.0"),
            },
        ),
        (
            "a",
            Predicate::Eq {
                field: field("a"),
                value: number("1e0"),
            },
        ),
        (
            "a",
            Predicate::Ge {
                field: field("a"),
                value: Value::Str("1".into()),
            },
        ),
        (
            "b",
            Predicate::Eq {
                field: field("b"),
                value: Value::Str("x".into()),
            },
        ),
        (
            "b",
            Predicate::Ne {
                field: field("b"),
                value: Value::Str("x".into()),
            },
        ),
        (
            "c",
            Predicate::Eq {
                field: field("c"),
                value: Value::Bool(true),
            },
        ),
        (
            "c",
            Predicate::Ne {
                field: field("c"),
                value: Value::Bool(true),
            },
        ),
        (
            "a",
            Predicate::Eq {
                field: field("a"),
                value: Value::Null,
            },
        ),
        (
            "a",
            Predicate::Ne {
                field: field("a"),
                value: Value::Null,
            },
        ),
        (
            "missing",
            Predicate::Eq {
                field: field("missing"),
                value: number("1"),
            },
        ),
        (
            "missing",
            Predicate::Ne {
                field: field("missing"),
                value: number("1"),
            },
        ),
        (
            "a",
            Predicate::And(
                Box::new(Predicate::Gt {
                    field: field("a"),
                    value: number("1"),
                }),
                Box::new(Predicate::Eq {
                    field: field("b"),
                    value: Value::Str("x".into()),
                }),
            ),
        ),
        (
            "a",
            Predicate::Or(
                Box::new(Predicate::Eq {
                    field: field("a"),
                    value: number("1"),
                }),
                Box::new(Predicate::Eq {
                    field: field("a"),
                    value: number("3"),
                }),
            ),
        ),
        (
            "a",
            Predicate::Not(Box::new(Predicate::Eq {
                field: field("a"),
                value: number("1"),
            })),
        ),
    ]
}

/// One cell's value, read from its span the way a caller would.
fn cell_value(src: &[u8], range: ByteRange) -> Value {
    let Materialized::Value(value) = parse(
        &src[range.start()..range.end()],
        MaterializeOptions::new(Dialect::Rfc8259, Form::Value),
    )
    .expect("a cell value") else {
        panic!("Form::Value materializes a value");
    };
    value
}

/// The value-side answer: materialize the document, then filter its rows with
/// the row law applied on top of `Predicate::matches` (a non-object is not a row).
fn reference_rows(src: &[u8], root: Option<&str>, field: &str, predicate: &Predicate) -> Vec<Option<Value>> {
    let Materialized::Value(document) =
        parse(src, MaterializeOptions::new(Dialect::Rfc8259, Form::Value)).expect("a value")
    else {
        panic!("Form::Value materializes a value");
    };
    let rows = match root {
        Some(key) => document.member(key).and_then(Value::as_array).expect("the rows array"),
        None => document.as_array().expect("the root array"),
    };
    rows.iter()
        .filter(|row| row.as_object().is_some() && predicate.matches(row))
        .map(|row| row.member(field).cloned())
        .collect()
}

/// The codec-side answer: the span filter's projected column.
fn span_rows(src: &[u8], root: Option<&str>, field: &str, predicate: &Predicate) -> Vec<Option<Value>> {
    let path = match root {
        Some(key) => Path::key(key),
        None => Path::root(),
    };
    let demands = [Demand::Filter {
        path,
        predicate: predicate.clone(),
        project: vec![field.into()],
    }];
    let req = ScanRequest::new(JsonInput::Text, &demands);
    let result = scan(src, &req).expect("a filter scan");
    let Answer::Columns(columns) = &result.answers[0] else {
        panic!("a filter answers a column batch");
    };
    assert_eq!(columns.width(), 1);
    columns
        .column(0)
        .map(|cell| match cell {
            ColumnCell::Span(range) => Some(cell_value(src, *range)),
            ColumnCell::Absent => None,
        })
        .collect()
}

#[test]
fn the_span_filter_agrees_with_the_value_predicate() {
    for (src, root) in DOCS {
        for (field, predicate) in cases() {
            assert_eq!(
                span_rows(src, *root, field, &predicate),
                reference_rows(src, *root, field, &predicate),
                "{src:?} projects {field} under {predicate:?}"
            );
        }
    }
}
