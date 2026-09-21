//! The stitch fold: one slot per demand across the parts.

mod common;

use common::range;
use structury::{
    Answer, ColumnCell, Columns, Document, Fact, FactOwner, FactRole, GrammarTag, OracleAnswer, ScanResult, stitch,
};

/// One ordered comment fact over `source`.
fn lead(start: usize, end: usize, owner: (usize, usize)) -> Fact<'static> {
    Fact::new(
        FactRole::CommentLead,
        std::borrow::Cow::Borrowed(""),
        Some(range(start, end)),
        FactOwner::node(range(owner.0, owner.1)),
    )
}

/// A validated document carrying one fact.
fn fact_document(source: &[u8]) -> Document<'_> {
    let mut document = Document::from_span_validated(source, range(0, source.len()), GrammarTag::UNKNOWN);
    document.set_facts(vec![lead(0, 4, (5, 6))]).expect("one ordered fact");
    document
}

fn one(answer: Answer<'_>) -> ScanResult<'_> {
    ScanResult::new(vec![answer], Vec::new())
}

#[test]
fn columns_concatenate_by_move_and_counts_sum() {
    let mut left = Columns::new(b"", vec![String::from("$")]);
    left.push(ColumnCell::Span(range(0, 1)));
    let mut right = Columns::new(b"", vec![String::from("$")]);
    right.push(ColumnCell::Span(range(1, 2)));
    let parts = vec![
        ScanResult::new(
            vec![Answer::Columns(left), Answer::Oracle(OracleAnswer::Count(2))],
            Vec::new(),
        ),
        ScanResult::new(
            vec![Answer::Columns(right), Answer::Oracle(OracleAnswer::Count(3))],
            Vec::new(),
        ),
    ];
    let out = stitch(parts);
    assert_eq!(out.answers.len(), 2);
    let Answer::Columns(columns) = &out.answers[0] else {
        panic!("expected columns");
    };
    assert_eq!(columns.rows(), 2);
    assert!(matches!(&out.answers[1], Answer::Oracle(OracleAnswer::Count(5))));
}

#[test]
fn single_part_is_returned_unchanged() {
    let out = stitch(vec![one(Answer::Missing)]);
    assert!(matches!(&out.answers[0], Answer::Missing));
}

#[test]
fn a_shorter_part_fills_missing_without_dropping_the_batch() {
    let mut longer = Columns::new(b"", vec![String::from("$")]);
    longer.push(ColumnCell::Span(range(0, 1)));
    let parts = vec![one(Answer::Columns(longer)), one(Answer::Missing)];
    let out = stitch(parts);
    let Answer::Columns(columns) = &out.answers[0] else {
        panic!("expected columns");
    };
    assert_eq!(columns.rows(), 1);
}

#[test]
fn a_shorter_first_part_does_not_truncate() {
    let mut cells = Columns::new(b"", vec![String::from("$")]);
    cells.push(ColumnCell::Span(range(0, 1)));
    let out = stitch(vec![
        ScanResult::new(Vec::new(), Vec::new()),
        one(Answer::Columns(cells)),
    ]);
    assert_eq!(out.answers.len(), 1, "the longest part sets the answer count");
    let Answer::Columns(columns) = &out.answers[0] else {
        panic!("columns");
    };
    assert_eq!(columns.rows(), 1);
}

#[test]
fn the_longest_part_sets_the_answer_count() {
    let out = stitch(vec![
        one(Answer::Missing),
        ScanResult::new(
            vec![
                Answer::Oracle(OracleAnswer::Count(2)),
                Answer::Oracle(OracleAnswer::Count(3)),
            ],
            Vec::new(),
        ),
    ]);
    assert_eq!(out.answers.len(), 2);
    assert!(matches!(&out.answers[0], Answer::Oracle(OracleAnswer::Count(2))));
    assert!(matches!(&out.answers[1], Answer::Oracle(OracleAnswer::Count(3))));
}

#[test]
fn an_empty_input_is_an_empty_result() {
    let out = stitch(Vec::new());
    assert!(out.answers.is_empty());
    assert!(out.issues.is_empty());
}

#[test]
#[should_panic(expected = "different sources")]
fn parts_from_different_sources_are_refused() {
    let mut left = Columns::new(b"aaaa", vec![String::from("$")]);
    left.push(ColumnCell::Span(range(0, 1)));
    let mut right = Columns::new(b"bbbb", vec![String::from("$")]);
    right.push(ColumnCell::Span(range(0, 1)));
    let _ = stitch(vec![one(Answer::Columns(left)), one(Answer::Columns(right))]);
}

#[test]
fn a_document_folds_into_one_column_row() {
    let source = b"{\"a\":1}";
    let document = Document::from_span(source, range(0, source.len()));
    let out = stitch(vec![one(Answer::Document(document)), one(Answer::Missing)]);
    let Answer::Columns(columns) = &out.answers[0] else {
        panic!("columns");
    };
    assert_eq!(columns.width(), 1);
    assert_eq!(columns.rows(), 1);
}

#[test]
fn counts_saturate_instead_of_overflowing() {
    let out = stitch(vec![
        one(Answer::Oracle(OracleAnswer::Count(u64::MAX))),
        one(Answer::Oracle(OracleAnswer::Count(5))),
    ]);
    assert!(matches!(&out.answers[0], Answer::Oracle(OracleAnswer::Count(u64::MAX))));
}

#[test]
#[should_panic(expected = "one-column batch")]
fn a_document_into_a_wider_batch_is_refused() {
    let source = b"ab";
    let mut wide = Columns::new(source, vec![String::from("a"), String::from("b")]);
    wide.push(ColumnCell::Span(range(0, 1)));
    wide.push(ColumnCell::Span(range(1, 2)));
    let document = Document::from_span(source, range(0, 1));
    let _ = stitch(vec![one(Answer::Columns(wide)), one(Answer::Document(document))]);
}

#[test]
fn a_single_document_keeps_its_facts_but_a_fold_does_not() {
    let source = b"// a\n1";

    let single = stitch(vec![one(Answer::Document(fact_document(source)))]);
    let Answer::Document(kept) = &single.answers[0] else {
        panic!("expected the document back");
    };
    assert_eq!(kept.facts().len(), 1, "a single part is returned unchanged");

    let folded = stitch(vec![
        one(Answer::Document(fact_document(source))),
        one(Answer::Document(fact_document(source))),
    ]);
    let Answer::Columns(columns) = &folded.answers[0] else {
        panic!("expected a column batch");
    };
    assert_eq!(columns.rows(), 2);
    assert_eq!(columns.fields().len(), 1);
    assert_eq!(columns.fields()[0], "$");
}
