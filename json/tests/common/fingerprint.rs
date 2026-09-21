//! A comparable fingerprint of one scan result's answer shapes.

use structury::{Answer, ScanResult};

/// Write each answer's shape: column extents, oracle text, or the mark.
pub(crate) fn fingerprint(result: &ScanResult<'_>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for answer in &result.answers {
        match answer {
            Answer::Columns(columns) => {
                let _ = write!(out, "columns:{}x{}[", columns.rows(), columns.width());
                for cell in columns.cells() {
                    match cell {
                        structury::ColumnCell::Span(range) => {
                            let _ = write!(out, "{}..{},", range.start(), range.end());
                        }
                        structury::ColumnCell::Absent => out.push_str("_,"),
                    }
                }
                out.push(']');
            }
            Answer::Oracle(oracle) => {
                let _ = write!(out, "oracle:{oracle:?}");
            }
            Answer::Missing => out.push_str("missing"),
            Answer::TypeMismatch { actual } => {
                let _ = write!(out, "mismatch:{actual:?}");
            }
            Answer::Document(document) => {
                let _ = write!(out, "document:{:?}", document.root());
            }
        }
    }
    out
}
