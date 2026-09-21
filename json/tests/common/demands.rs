//! Demands and predicates reused across the scan suites.
//!
//! The tables pin laws as data; these builders keep a row one line instead of a
//! five-line `Demand` literal.

use structury::{Demand, Path, Predicate, Range, Step, Value};

use crate::common::num;

pub(crate) fn into_fields(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).into()).collect()
}

pub(crate) fn project(path: Path, fields: &[&str]) -> Demand {
    Demand::Project {
        path,
        fields: into_fields(fields),
    }
}

pub(crate) fn project_root(fields: &[&str]) -> Demand {
    project(Path::root(), fields)
}

pub(crate) fn project_key(key: &str, fields: &[&str]) -> Demand {
    project(Path::key(key), fields)
}

pub(crate) fn project_index(index: i64, fields: &[&str]) -> Demand {
    project(
        Path {
            steps: vec![Step::Index(index)],
        },
        fields,
    )
}

pub(crate) fn project_steps(steps: &[Step], fields: &[&str]) -> Demand {
    project(Path { steps: steps.to_vec() }, fields)
}

pub(crate) fn collection(keys: Option<&[&str]>, nested: Option<Demand>) -> Demand {
    Demand::Collection {
        fields: keys.map(into_fields),
        nested: nested.map(Box::new),
    }
}

pub(crate) fn path(steps: &[Step], nested: Option<Demand>) -> Demand {
    Demand::Path {
        steps: steps.to_vec(),
        nested: nested.map(Box::new),
    }
}

pub(crate) fn path_key(key: &str, nested: Option<Demand>) -> Demand {
    path(&[Step::Key(key.into())], nested)
}

pub(crate) fn path_index(index: i64, nested: Option<Demand>) -> Demand {
    path(&[Step::Index(index)], nested)
}

pub(crate) fn key(name: &str) -> Vec<Step> {
    vec![Step::Key(name.into())]
}

pub(crate) fn filter(path: Path, predicate: Predicate, project: &[&str]) -> Demand {
    Demand::Filter {
        path,
        predicate,
        project: into_fields(project),
    }
}

pub(crate) fn filter_root(predicate: Predicate, project: &[&str]) -> Demand {
    filter(Path::root(), predicate, project)
}

pub(crate) fn filter_key(key: &str, predicate: Predicate, project: &[&str]) -> Demand {
    filter(Path::key(key), predicate, project)
}

pub(crate) fn slice(start: Option<i64>, end: Option<i64>) -> Demand {
    Demand::Slice {
        range: Range { start, end },
        nested: None,
    }
}

pub(crate) fn slice_nested(start: Option<i64>, end: Option<i64>, nested: Demand) -> Demand {
    Demand::Slice {
        range: Range { start, end },
        nested: Some(Box::new(nested)),
    }
}

/// `Demand::Path { steps: [], nested: Some(nested) }`: the spent spine.
pub(crate) fn spent_path(nested: Demand) -> Demand {
    Demand::Path {
        steps: Vec::new(),
        nested: Some(Box::new(nested)),
    }
}

pub(crate) fn eq(field: &str, value: Value) -> Predicate {
    Predicate::Eq {
        field: field.into(),
        value,
    }
}

pub(crate) fn gt(field: &str, value: &str) -> Predicate {
    Predicate::Gt {
        field: field.into(),
        value: num(value),
    }
}

pub(crate) fn ge(field: &str, value: &str) -> Predicate {
    Predicate::Ge {
        field: field.into(),
        value: num(value),
    }
}

pub(crate) fn lt(field: &str, value: &str) -> Predicate {
    Predicate::Lt {
        field: field.into(),
        value: num(value),
    }
}

pub(crate) fn not(inner: Predicate) -> Predicate {
    Predicate::Not(Box::new(inner))
}

pub(crate) fn and(left: Predicate, right: Predicate) -> Predicate {
    Predicate::And(Box::new(left), Box::new(right))
}

pub(crate) fn or(left: Predicate, right: Predicate) -> Predicate {
    Predicate::Or(Box::new(left), Box::new(right))
}

/// The `a.b.c` spine.
pub(crate) fn abc() -> Path {
    Path {
        steps: vec![Step::Key("a".into()), Step::Key("b".into()), Step::Key("c".into())],
    }
}

/// `n > 0`, the predicate the nested-filter demands share.
pub(crate) fn gt_zero() -> Predicate {
    gt("n", "0")
}

/// The `project-nested` demand: `rows`, then per element a `Project` along the
/// `a.b.c` spine keeping field `d`.
pub(crate) fn nested_projection() -> Demand {
    path_key("rows", Some(collection(None, Some(project(abc(), &["d"])))))
}

/// `rows`, then per element a `Filter{n > 0}` projecting `project`.
pub(crate) fn nested_filter(project: &[&str]) -> Demand {
    path_key("rows", Some(collection(None, Some(filter_root(gt_zero(), project)))))
}
