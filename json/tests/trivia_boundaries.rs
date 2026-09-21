//! Trivia at every punctuation boundary must reach the same answer through the
//! demand scan and the materializing lexer. The fused RFC member-head fast path
//! once assumed no whitespace after the colon, so a spaced member failed.

mod common;

use structury::{Demand, Strictness, Value};
use structury_json::{Dialect, JsonInput, ScanRequest, scan};

/// RFC-legal bases: every dialect accepts them, so each one is a cross-dialect
/// fixture. They cover scalar, nested-array, nested-object, and empty containers.
const BASES: &[&[u8]] = &[
    br#"{"a":1}"#,
    br#"{"a":[1,2],"b":{"c":true}}"#,
    br#"[{"x":"y"},null,false,1.5]"#,
    br#"{"empty_obj":{},"empty_arr":[]}"#,
    br#"{"a":1,"b":2,"c":3}"#,
];

fn request(demands: &[Demand], strictness: Strictness, dialect: Dialect) -> ScanRequest<'_> {
    common::req_with(JsonInput::Text, demands, strictness, dialect)
}

/// `src` with `trivia` inserted at each index adjacent to a punctuation byte.
fn with_trivia_at_boundaries(src: &[u8], trivia: &[u8]) -> Vec<Vec<u8>> {
    let punct = |byte: u8| matches!(byte, b'{' | b'}' | b'[' | b']' | b':' | b',');
    let mut variants = Vec::new();
    for i in 0..=src.len() {
        let before = i > 0 && punct(src[i - 1]);
        let after = src.get(i).is_some_and(|byte| punct(*byte));
        if before || after {
            let mut variant = Vec::with_capacity(src.len() + trivia.len());
            variant.extend_from_slice(&src[..i]);
            variant.extend_from_slice(trivia);
            variant.extend_from_slice(&src[i..]);
            variants.push(variant);
        }
    }
    variants
}

fn scan_whole(src: &[u8], strictness: Strictness, dialect: Dialect) -> Result<Value, structury::Error> {
    let demand = Demand::Whole;
    let result = scan(src, &request(core::slice::from_ref(&demand), strictness, dialect))?;
    common::mat_value_dialect(&result.answers[0], dialect)
}

fn assert_boundaries(dialect: Dialect, trivia: &[&[u8]]) {
    for base in BASES {
        let expected = common::parsed_dialect(base, dialect).expect("base parse");
        for trivia in trivia {
            for variant in with_trivia_at_boundaries(base, trivia) {
                let text = String::from_utf8_lossy(&variant).into_owned();
                assert_eq!(
                    common::parsed_dialect(&variant, dialect).unwrap_or_else(|e| panic!("lexer {text}: {e}")),
                    expected,
                    "lexer {text}"
                );
                for strictness in [Strictness::Lazy, Strictness::Structural, Strictness::Strict] {
                    assert_eq!(
                        scan_whole(&variant, strictness, dialect)
                            .unwrap_or_else(|e| panic!("scan {strictness:?} {text}: {e}")),
                        expected,
                        "scan {strictness:?} {text}"
                    );
                }
            }
        }
    }
}

/// RFC has no comments, so only whitespace variants apply.
#[test]
fn rfc_whitespace_at_every_boundary_scans_like_the_lexer() {
    assert_boundaries(Dialect::Rfc8259, &[b" ", b"\t", b"\n", b"\r", b"  \n\t", b"\r\n"]);
}

#[test]
fn jsonc_whitespace_and_comments_at_every_boundary_scan_like_the_lexer() {
    assert_boundaries(
        Dialect::Jsonc,
        &[b" ", b"\n", b"/* c */", b" /* c */ ", b"// c\n", b"\n/* c\nd */\n"],
    );
}

#[test]
fn json5_whitespace_and_comments_at_every_boundary_scan_like_the_lexer() {
    assert_boundaries(Dialect::Json5, &[b" ", b"\t", b"/* c */", b"// c\n", b" \r\n/* c */"]);
}

/// The same RFC text read by the RFC tokenizer and by the dialect lexer must
/// agree: `RfcScan` versus `DialectScan` on whitespace-varied inputs.
#[test]
fn rfc_scan_answers_match_dialect_scan_on_spaced_input() {
    for base in BASES {
        for trivia in [&b" "[..], b"\t", b"\n", b"\r", b"  \n"] {
            for variant in with_trivia_at_boundaries(base, trivia) {
                let text = String::from_utf8_lossy(&variant).into_owned();
                for strictness in [Strictness::Lazy, Strictness::Structural, Strictness::Strict] {
                    let rfc = scan_whole(&variant, strictness, Dialect::Rfc8259);
                    let dialect = scan_whole(&variant, strictness, Dialect::Jsonc);
                    assert_eq!(
                        rfc.unwrap_or_else(|e| panic!("rfc scan {strictness:?} {text}: {e}")),
                        dialect.unwrap_or_else(|e| panic!("dialect scan {strictness:?} {text}: {e}")),
                        "strategy answers {strictness:?} {text}"
                    );
                }
            }
        }
    }
}
