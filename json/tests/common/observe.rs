//! A materialize-free view of one scan answer, shared by the differentials.

use structury::{Answer, Demand, Strictness, Value, ValueKind};
use structury_json::{Dialect, JsonInput, scan};

use crate::common;

/// One answer, with the two non-value marks kept distinct: `Missing` is not
/// `TypeMismatch`, and neither is `null`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Obs {
    Value(Value),
    Missing,
    TypeMismatch(ValueKind),
    Oracle(String),
}

pub(crate) fn observe(answer: &Answer<'_>) -> Obs {
    match answer {
        Answer::Missing => Obs::Missing,
        Answer::TypeMismatch { actual } => Obs::TypeMismatch(*actual),
        Answer::Oracle(oracle) => Obs::Oracle(format!("{oracle:?}")),
        Answer::Document(_) | Answer::Columns(_) => Obs::Value(common::mat_value(answer).expect("materialize answer")),
    }
}

pub(crate) fn observe_all(answers: &[Answer<'_>]) -> Vec<Obs> {
    answers.iter().map(observe).collect()
}

/// One demand's answers under a structural scan, formatted for exact-string laws.
pub(crate) fn obs(src: &[u8], demand: &Demand, dialect: Dialect) -> String {
    let request = common::req_with(
        JsonInput::Text,
        core::slice::from_ref(demand),
        Strictness::Structural,
        dialect,
    );
    let result = scan(src, &request).expect("scan");
    format!("{:?}", observe_all(&result.answers))
}

pub(crate) fn obs_text(src: &[u8], demand: &Demand) -> String {
    obs(src, demand, Dialect::Rfc8259)
}

/// Materialize an answer or panic: the differentials' expected-value shortcut.
pub(crate) fn mat(answer: &Answer<'_>) -> Value {
    common::mat_value(answer).expect("materialize")
}
