//! The format-neutral stitch fold: ordered per-part scan results in, one result
//! out. The core crate re-exports it; a codec calls it (the JSON codec's `Plan`
//! drive does).

use alloc::string::String;
use alloc::vec::Vec;

use crate::document::{ColumnCell, Columns, same_source};
use crate::scan::{Answer, OracleAnswer, ScanResult};

/// Stitch part results in plan order. Parts index the same bytes and answer the
/// same demands, so each slot folds across the parts; a part shorter than the
/// longest contributes [`Answer::Missing`] for the gaps.
///
/// Per slot the fold is: `Columns` wins (rows concatenated by move, and an
/// [`Answer::Document`] becomes one `"$"` column row), then a summed
/// [`OracleAnswer::Count`] (`saturating_add`), then the first other answer
/// (`Missing` when every part is missing). A single part is returned unchanged;
/// an empty input yields an empty result. A folded `Document` keeps only its
/// root span, so any `facts()` on it do not survive the fold.
///
/// # Panics
///
/// When two answers bind different non-empty sources: an answer cannot be
/// applied to bytes other than the ones it was found in.
pub fn stitch<'src>(mut parts: Vec<ScanResult<'src>>) -> ScanResult<'src> {
    if parts.is_empty() {
        return ScanResult {
            answers: Vec::new(),
            issues: Vec::new(),
        };
    }
    if parts.len() == 1 {
        return parts
            .into_iter()
            .next()
            .expect("BUG: exactly one part after the empty and single-part checks");
    }
    // The longest part sets the answer count; taking the first part's length
    // would silently truncate.
    let n = parts.iter().map(|part| part.answers.len()).max().unwrap_or(0);
    // A part shorter than the longest contributes `Missing` for its gaps, and
    // that synthesized `Missing` holds `first`-position like any other answer: a
    // later part's `TypeMismatch`/oracle must not overtake it. Pad first, so the
    // part-major fold below sees exactly the per-slot sequence the slot-major
    // fold saw.
    for part in &mut parts {
        if part.answers.len() < n {
            part.answers.resize(n, Answer::Missing);
        }
    }
    let mut issues = Vec::new();
    let mut source: Option<&[u8]> = None;
    let mut slots: Vec<Slot<'src>> = (0..n).map(|_| Slot::default()).collect();
    for part in &mut parts {
        issues.append(&mut part.issues);
        for (i, answer) in part.answers.drain(..).enumerate() {
            let slot = &mut slots[i];
            match answer {
                Answer::Columns(mut next) => {
                    check_source(&mut source, next.source());
                    if let Some(acc) = slot.columns.as_mut() {
                        acc.append_rows(&mut next);
                    } else {
                        slot.columns = Some(next);
                    }
                }
                Answer::Document(document) => {
                    check_source(&mut source, document.source());
                    if let Some(acc) = slot.columns.as_mut() {
                        assert_eq!(
                            acc.width(),
                            1,
                            "a Document folds into a one-column batch, not a wider row"
                        );
                        acc.push(ColumnCell::Span(document.root()));
                    } else {
                        let mut acc = Columns::new(document.source(), alloc::vec![String::from("$")]);
                        acc.push(ColumnCell::Span(document.root()));
                        slot.columns = Some(acc);
                    }
                }
                Answer::Oracle(OracleAnswer::Count(count)) => {
                    slot.count = Some(slot.count.unwrap_or(0).saturating_add(count));
                }
                other => {
                    if slot.first.is_none() {
                        slot.first = Some(other);
                    }
                }
            }
        }
    }
    let answers = slots
        .into_iter()
        .map(|slot| {
            if let Some(columns) = slot.columns {
                Answer::Columns(columns)
            } else if let Some(count) = slot.count {
                Answer::Oracle(OracleAnswer::Count(count))
            } else {
                slot.first.unwrap_or(Answer::Missing)
            }
        })
        .collect();
    ScanResult { answers, issues }
}

/// One answer slot's fold state.
#[derive(Default)]
struct Slot<'src> {
    columns: Option<Columns<'src>>,
    count: Option<u64>,
    first: Option<Answer<'src>>,
}

/// Refuse a second, different non-empty source.
fn check_source<'a>(known: &mut Option<&'a [u8]>, bytes: &'a [u8]) {
    match known {
        Some(known) => assert!(
            same_source(known, bytes),
            concat!(
                "stitch across different sources: ",
                "an answer cannot be applied to different bytes"
            )
        ),
        None => *known = Some(bytes),
    }
}
