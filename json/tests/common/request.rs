//! Scan-request builders shared by the scan and control suites.

use structury::{Answer, Demand, Strictness};
use structury_json::{Dialect, JsonInput, ScanRequest};

pub(crate) fn req_with(
    input: JsonInput,
    demands: &[Demand],
    strictness: Strictness,
    dialect: Dialect,
) -> ScanRequest<'_> {
    ScanRequest::new(input, demands)
        .with_strictness(strictness)
        .with_dialect(dialect)
}

pub(crate) fn req(input: JsonInput, demands: &[Demand], dialect: Dialect) -> ScanRequest<'_> {
    req_with(input, demands, Strictness::Structural, dialect)
}

pub(crate) fn mk_req(demands: &[Demand], dialect: Dialect, strictness: Strictness, facts: bool) -> ScanRequest<'_> {
    req_with(JsonInput::Text, demands, strictness, dialect).with_facts(facts)
}

pub(crate) fn text_dialect(demands: &[Demand], dialect: Dialect) -> ScanRequest<'_> {
    req(JsonInput::Text, demands, dialect)
}

pub(crate) fn text(demands: &[Demand]) -> ScanRequest<'_> {
    text_dialect(demands, Dialect::Rfc8259)
}

pub(crate) fn stream(demands: &[Demand]) -> ScanRequest<'_> {
    req(JsonInput::Ndjson, demands, Dialect::Rfc8259)
}

pub(crate) fn scan_of<'src>(src: &'src [u8], request: &ScanRequest<'_>) -> structury::ScanResult<'src> {
    structury_json::scan(src, request).expect("scan")
}

pub(crate) fn answers<'src>(src: &'src [u8], request: &ScanRequest<'_>) -> Vec<Answer<'src>> {
    scan_of(src, request).answers
}

pub(crate) fn answer_of<'src>(src: &'src [u8], request: &ScanRequest<'_>) -> Answer<'src> {
    answers(src, request).remove(0)
}

pub(crate) fn one<'src>(src: &'src [u8], input: JsonInput, demand: &Demand, dialect: Dialect) -> Answer<'src> {
    answer_of(src, &req(input, core::slice::from_ref(demand), dialect))
}
