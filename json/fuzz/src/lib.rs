//! Shared helpers for structury-json fuzz targets.

use structury::{Answer, ByteRange, Demand, Document, Strictness, Value};
use structury_json::{
    Dialect, EncodeOptions, Form, JsonInput, MaterializeOptions, Materialized, ScanRequest, Source,
    encode, materialize, scan,
};

pub const MAX_INPUT: usize = 64 * 1024;

/// The `Value` form of [`materialize`], for targets that walk an owned tree.
pub fn value(answer: &Answer<'_>) -> Result<Value, structury::Error> {
    materialize(answer, MaterializeOptions::default().with_form(Form::Value)).map(Materialized::into_value)
}

/// The borrowed zero-copy view and the owned tape must agree byte for byte on
/// arbitrary input: same acceptance, same error, same values.
pub fn borrow_whole(data: &[u8]) {
    let span = ByteRange::try_new(0, data.len()).expect("0 <= data.len()");
    let answer = Answer::Document(Document::from_span(data, span));
    let borrowed = materialize(&answer, MaterializeOptions::default().with_form(Form::Borrowed));
    let tape = materialize(&answer, MaterializeOptions::default().with_form(Form::Owned));
    match (borrowed, tape) {
        (Ok(Materialized::Borrowed(view)), Ok(Materialized::Owned(owned))) => {
            assert_eq!(view.to_value(), owned.to_value(), "borrowed/tape value disagree");
        }
        (Err(a), Err(b)) => assert_eq!(a, b, "borrowed/tape error disagree"),
        (a, b) => panic!("borrowed/tape acceptance disagree: {a:?} {b:?}"),
    }
}

/// One Whole scan per input/arrangement/strictness triple.
pub fn scan_whole(data: &[u8], input: JsonInput, strictness: Strictness) {
    let demand = Demand::Whole;
    let req = ScanRequest::new(input, core::slice::from_ref(&demand)).with_strictness(strictness);
    let _ = scan(data, &req);
}

/// Collecting facts, then encoding them back, must not panic: the collector can
/// stop early on malformed input, and a Strict document must encode and re-parse.
pub fn scan_facts(data: &[u8]) {
    let demand = Demand::Whole;
    for dialect in [Dialect::Jsonc, Dialect::Json5] {
        for strictness in [Strictness::Structural, Strictness::Strict] {
            let req = ScanRequest::new(JsonInput::Text, core::slice::from_ref(&demand))
                .with_strictness(strictness)
                .with_dialect(dialect)
                .with_facts(true);
            let Ok(result) = scan(data, &req) else {
                continue;
            };
            if !matches!(strictness, Strictness::Strict) {
                continue;
            }
            let Answer::Document(document) = &result.answers[0] else {
                continue;
            };
            for opts in [
                EncodeOptions::compact().with_dialect(dialect),
                EncodeOptions::pretty().with_dialect(dialect),
            ] {
                let mut out = Vec::new();
                if encode(Source::Document(document), &opts, &mut out).is_ok() {
                    let _ = scan(&out, &req).expect("successfully encoded document must re-parse");
                }
            }
            // A byte-preserving write is gated on the validating dialect: the
            // same document under another grammar must be refused, never copied.
            let other = if dialect == Dialect::Jsonc {
                Dialect::Json5
            } else {
                Dialect::Rfc8259
            };
            let mut out = Vec::new();
            let error = encode(
                Source::Document(document),
                &EncodeOptions::compact().with_dialect(other),
                &mut out,
            )
            .expect_err("verbatim write under another dialect");
            assert_eq!(error.code(), "dialect-mismatch");
        }
    }
}
