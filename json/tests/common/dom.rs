//! A hand-written DOM oracle, deliberately separate from the scan walk so it is
//! a real differential. It encodes the one-row-per-element law: a flat
//! projection over an element contributes one row per element, and a non-object
//! element is an absent row, never a dropped row. Numeric `Eq`/`Ne` compare by
//! value, as the walk's `span_eq_value` does.

use core::cmp::Ordering;

use structury::{CompactStr, Demand, Number, OracleAnswer, Path, Predicate, Step, Value, ValueKind};

use crate::common::observe::Obs;

/// Build an expected tree from a JSON literal with `serde_json`, independent of
/// the product parse; the tables pin their laws as data.
pub(crate) fn json_value(text: &str) -> Value {
    from_serde(&serde_json::from_str::<serde_json::Value>(text).expect("expected JSON"))
}

fn from_serde(node: &serde_json::Value) -> Value {
    match node {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(flag) => Value::Bool(*flag),
        serde_json::Value::Number(number) => Value::Number(Number::parse(&number.to_string()).expect("number")),
        serde_json::Value::String(text) => Value::Str(CompactStr::from(text.as_str())),
        serde_json::Value::Array(items) => Value::Array(items.iter().map(from_serde).collect()),
        serde_json::Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(name, value)| (CompactStr::from(name.as_str()), from_serde(value)))
                .collect(),
        ),
    }
}

pub(crate) fn nav<'a>(value: &'a Value, path: &Path) -> Option<&'a Value> {
    let mut cur = value;
    for step in &path.steps {
        cur = match step {
            Step::Key(key) => cur.member(key)?,
            Step::Index(index) => cur.element(*index)?,
        };
    }
    Some(cur)
}

pub(crate) fn rows<'a>(tree: &'a Value, path: &Path) -> &'a [Value] {
    match nav(tree, path) {
        Some(Value::Array(items)) => items,
        other => panic!("oracle path {path:?} is not an array: {other:?}"),
    }
}

/// One projected row: the selected fields in demand order, last-wins; a
/// non-object contributes an absent row.
pub(crate) fn project_row<S: AsRef<str>>(value: &Value, fields: &[S]) -> Value {
    if !matches!(value, Value::Object(_)) {
        return Value::Object(Vec::new());
    }
    Value::Object(
        fields
            .iter()
            .filter_map(|field| {
                let field = field.as_ref();
                value
                    .member(field)
                    .map(|found| (CompactStr::from(field), found.clone()))
            })
            .collect(),
    )
}

fn value_eq(got: &Value, want: &Value) -> bool {
    match (got, want) {
        (Value::Number(a), Value::Number(b)) => a.numeric_eq(b),
        _ => got == want,
    }
}

fn value_cmp(row: &Value, field: &str, want: &Value) -> Option<Ordering> {
    match (row.member(field)?, want) {
        (Value::Number(a), Value::Number(b)) => a.numeric_cmp(b),
        _ => None,
    }
}

/// Does `value` satisfy `predicate`? A non-object never matches.
pub(crate) fn holds(value: &Value, predicate: &Predicate) -> bool {
    if !matches!(value, Value::Object(_)) {
        return false;
    }
    match predicate {
        Predicate::And(a, b) => holds(value, a) && holds(value, b),
        Predicate::Or(a, b) => holds(value, a) || holds(value, b),
        Predicate::Not(inner) => !holds(value, inner),
        Predicate::Eq { field, value: want } => value.member(field).is_some_and(|got| value_eq(got, want)),
        Predicate::Ne { field, value: want } => value.member(field).is_none_or(|got| !value_eq(got, want)),
        Predicate::Gt { field, value: want } => value_cmp(value, field, want) == Some(Ordering::Greater),
        Predicate::Lt { field, value: want } => value_cmp(value, field, want) == Some(Ordering::Less),
        Predicate::Ge { field, value: want } => {
            matches!(value_cmp(value, field, want), Some(Ordering::Greater | Ordering::Equal))
        }
        Predicate::Le { field, value: want } => {
            matches!(value_cmp(value, field, want), Some(Ordering::Less | Ordering::Equal))
        }
    }
}

/// The expected answer for `demand` over the virtual array `items`; `None` for a
/// demand shape the oracle does not model.
pub(crate) fn dom_array(items: &[Value], demand: &Demand) -> Option<Obs> {
    match demand {
        Demand::Whole => Some(Obs::Value(Value::Array(items.to_vec()))),
        Demand::Project { path, fields } if path.steps.is_empty() => Some(Obs::Value(Value::Array(
            items.iter().map(|e| project_row(e, fields)).collect(),
        ))),
        Demand::Project { path, fields } => match path.steps.first() {
            Some(Step::Index(index)) => structury::resolve_index(items.len(), *index)
                .map(|at| Obs::Value(Value::Array(vec![project_row(&items[at], fields)]))),
            Some(Step::Key(_)) => Some(Obs::TypeMismatch(ValueKind::Array)),
            None => None,
        },
        Demand::Collection {
            fields: Some(keys),
            nested: None,
        } => Some(Obs::Value(Value::Array(
            items.iter().map(|e| project_row(e, keys)).collect(),
        ))),
        Demand::Collection {
            fields: None,
            nested: Some(inner),
        } => match inner.as_ref() {
            Demand::Project { path, fields } => {
                let mut rows = Vec::new();
                for element in items {
                    // A flat projection is one row per outer element; an array
                    // element is an absent row, never a map of its inner
                    // elements (the element-position rule). A non-empty spine's
                    // located node *may* be an array, and then it maps.
                    if path.steps.is_empty() {
                        rows.push(project_row(element, fields));
                        continue;
                    }
                    match nav(element, path) {
                        Some(Value::Array(sub)) => rows.extend(sub.iter().map(|e| project_row(e, fields))),
                        Some(located) => rows.push(project_row(located, fields)),
                        None => rows.push(Value::Object(Vec::new())),
                    }
                }
                Some(Obs::Value(Value::Array(rows)))
            }
            Demand::Filter {
                path,
                predicate,
                project,
            } if path.steps.is_empty() => {
                let mut rows = Vec::new();
                for element in items {
                    if holds(element, predicate) {
                        rows.push(if project.is_empty() {
                            element.clone()
                        } else {
                            project_row(element, project)
                        });
                    }
                }
                Some(Obs::Value(Value::Array(rows)))
            }
            Demand::Path { steps, nested: None } if steps.is_empty() => Some(Obs::Value(Value::Array(items.to_vec()))),
            _ => None,
        },
        Demand::Slice { range, nested: None } => {
            let window = range.window(items.len());
            Some(Obs::Value(Value::Array(items[window].to_vec())))
        }
        Demand::Oracle(structury::Oracle::Count) => {
            Some(Obs::Oracle(format!("{:?}", OracleAnswer::Count(items.len() as u64))))
        }
        Demand::Oracle(structury::Oracle::Kind) => {
            Some(Obs::Oracle(format!("{:?}", OracleAnswer::Kind(ValueKind::Array))))
        }
        _ => None,
    }
}

pub(crate) fn dom_project(tree: &Value, path: &Path, fields: &[&str]) -> Value {
    Value::Array(rows(tree, path).iter().map(|row| project_row(row, fields)).collect())
}

pub(crate) fn dom_filter(tree: &Value, path: &Path, predicate: &Predicate, fields: &[&str]) -> Value {
    Value::Array(
        rows(tree, path)
            .iter()
            .filter(|row| holds(row, predicate))
            .map(|row| project_row(row, fields))
            .collect(),
    )
}
