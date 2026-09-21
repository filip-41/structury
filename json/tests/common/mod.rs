//! Helpers over the one `materialize`/`parse` entry.
//!
//! The read surface is a single function selected by `Form`; these wrappers keep
//! the differentials readable and preserve each call site's `.expect(...)`/`.is_err()`.
#![allow(dead_code, unused_imports)]

mod assert;
mod control;
mod demands;
mod differential;
mod dom;
mod fingerprint;
mod fixtures;
mod observe;
mod request;

pub(crate) use assert::*;
pub(crate) use control::*;
pub(crate) use demands::*;
pub(crate) use differential::*;
pub(crate) use dom::*;
pub(crate) use fingerprint::*;
pub(crate) use fixtures::*;
pub(crate) use observe::*;
pub(crate) use request::*;

use structury::{Answer, BorrowedDocument, ByteRange, Demand, Document, Error, OwnedDocument, Value};
use structury_json::{Dialect, FIRST_SHARD_BYTES, Form, MaterializeOptions, Materialized, Plan, ScanRequest};

/// Whole-buffer document answer: the span every read form is compared on.
pub(crate) fn full_span(bytes: &[u8]) -> Answer<'_> {
    let span = ByteRange::try_new(0, bytes.len()).expect("ordered");
    Answer::Document(Document::from_span(bytes, span))
}

/// Scan one planned range through the same plan the codec builds.
pub(crate) fn scan_range<'src>(
    src: &'src [u8],
    range: ByteRange,
    req: &structury_json::ScanRequest<'_>,
) -> Result<structury::ScanResult<'src>, structury::Error> {
    structury_json::Plan::build(src, req)?.scan(src, range)
}

/// The codec's serial plan: the text cut or stream partition, packed at the
/// first-shard size.
pub(crate) fn plan_ranges(src: &[u8], req: &ScanRequest<'_>) -> Result<Vec<ByteRange>, Error> {
    Plan::build(src, req).map(|plan| plan.ranges(FIRST_SHARD_BYTES))
}

/// How many morsels the plan cuts the source into, one element per morsel.
pub(crate) fn morsels(src: &[u8], demands: &[Demand], dialect: Dialect) -> usize {
    Plan::build(src, &text_dialect(demands, dialect)).map_or(0, |plan| plan.ranges(1).len())
}

/// Documents every read form must agree on: nesting, duplicate keys, decoded
/// escapes, wide integers, and the empty containers.
pub(crate) const FIXTURES: &[&[u8]] = &[
    br#"{"a":1,"b":{"c":[true,null,"x"]},"d":1.50}"#,
    br#"[1,{"k":-0},3]"#,
    br#"{"users":[{"id":1,"name":"a"},{"id":2,"name":"b"}]}"#,
    b"null",
    b"true",
    b"42",
    b"\"hi\"",
    br#"{"dup":1,"dup":2,"nested":{"x":1,"x":2,"x":3}}"#,
    br#"{"esc":"caf\u00e9\n\t\\\" \u20ac \ud83d\ude00","plain":"x","\u0041":"key-decoded"}"#,
    br#"{"empty":"","num":1e2,"frac":-0.5,"wide":123456789012345678901234567890}"#,
    br#"[[],{},[""],[{"k":""}]]"#,
];

fn opts(dialect: Dialect, form: Form) -> MaterializeOptions {
    MaterializeOptions::new(dialect, form)
}

pub(crate) fn mat_value(answer: &Answer<'_>) -> Result<Value, Error> {
    mat_value_dialect(answer, Dialect::Rfc8259)
}

pub(crate) fn mat_value_dialect(answer: &Answer<'_>, dialect: Dialect) -> Result<Value, Error> {
    structury_json::materialize(answer, opts(dialect, Form::Value)).map(Materialized::into_value)
}

pub(crate) fn mat_owned(answer: &Answer<'_>) -> Result<OwnedDocument, Error> {
    mat_owned_dialect(answer, Dialect::Rfc8259)
}

pub(crate) fn mat_owned_dialect(answer: &Answer<'_>, dialect: Dialect) -> Result<OwnedDocument, Error> {
    structury_json::materialize(answer, opts(dialect, Form::Owned)).map(|materialized| match materialized {
        Materialized::Owned(document) => document,
        _ => unreachable!("owned form"),
    })
}

pub(crate) fn mat_borrowed<'src>(answer: &Answer<'src>) -> Result<BorrowedDocument<'src>, Error> {
    mat_borrowed_dialect(answer, Dialect::Rfc8259)
}

pub(crate) fn mat_borrowed_dialect<'src>(
    answer: &Answer<'src>,
    dialect: Dialect,
) -> Result<BorrowedDocument<'src>, Error> {
    structury_json::materialize(answer, opts(dialect, Form::Borrowed)).map(|materialized| match materialized {
        Materialized::Borrowed(document) => document,
        _ => unreachable!("borrowed form"),
    })
}

pub(crate) fn parsed(src: &[u8]) -> Result<Value, Error> {
    parsed_dialect(src, Dialect::Rfc8259)
}

pub(crate) fn parsed_dialect(src: &[u8], dialect: Dialect) -> Result<Value, Error> {
    structury_json::parse(src, opts(dialect, Form::Value)).map(Materialized::into_value)
}
