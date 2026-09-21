//! Grammar provenance, facts, and `Columns` cell batches.

mod common;

use common::range;
use std::borrow::Cow;

use structury::{ByteRange, ColumnCell, Columns, Document, Fact, FactOwner, FactRole, GrammarTag};

fn lead(start: usize, end: usize, owner: (usize, usize)) -> Fact<'static> {
    Fact::new(
        FactRole::CommentLead,
        Cow::Borrowed(""),
        Some(range(start, end)),
        FactOwner::node(range(owner.0, owner.1)),
    )
}

fn document(source: &[u8]) -> Document<'_> {
    Document::from_span_validated(source, range(0, source.len()), GrammarTag::UNKNOWN)
}

#[test]
fn grammar_provenance_round_trips_and_defaults_unknown() {
    let source = b"1";
    let plain = Document::from_span(source, range(0, 1));
    assert_eq!(plain.grammar(), GrammarTag::UNKNOWN);
    assert!(!plain.is_fully_validated());

    let validated = Document::from_span_validated(source, range(0, 1), GrammarTag::codec(7));
    assert_eq!(validated.grammar(), GrammarTag::codec(7));
    assert!(validated.is_fully_validated());
}

#[test]
fn set_facts_rejects_out_of_order() {
    let mut doc = document(b"// a // b");
    doc.set_facts(vec![lead(0, 4, (5, 6)), lead(6, 10, (11, 12))])
        .expect("ascending");
    assert_eq!(doc.facts().len(), 2);

    assert!(
        doc.set_facts(vec![lead(6, 10, (11, 12)), lead(0, 4, (5, 6))]).is_err(),
        "out of order"
    );
    assert_eq!(doc.facts().len(), 2, "rejected set");
}

#[test]
fn equal_glyph_starts_are_allowed_for_a_dual_record() {
    let mut doc = document(b"[1 /* c */ 2]");
    let glyph = range(4, 11);
    let foot = Fact::new(
        FactRole::CommentFoot,
        Cow::Borrowed(" c "),
        Some(glyph),
        FactOwner::node(range(1, 2)),
    );
    let lead = Fact::new(
        FactRole::CommentLead,
        Cow::Borrowed(" c "),
        Some(glyph),
        FactOwner::node(range(12, 13)),
    );
    doc.set_facts(vec![foot, lead]).expect("dual records share one glyph");
    assert_eq!(doc.facts().len(), 2);
}

#[test]
fn glyphless_facts_are_skipped_for_ordering() {
    let mut doc = document(b"// a 1");
    let glyphless = Fact::new(
        FactRole::CommentLead,
        Cow::Borrowed(""),
        None,
        FactOwner::node(range(5, 6)),
    );
    doc.set_facts(vec![lead(6, 10, (11, 12)), glyphless])
        .expect("a fact with no glyph is skipped");
    assert_eq!(doc.facts().len(), 2);
}

#[test]
fn set_facts_empty_clears_facts() {
    let mut doc = document(b"// a 1");
    doc.set_facts(vec![lead(0, 4, (5, 6))]).expect("fact");
    assert_eq!(doc.facts().len(), 1);
    doc.set_facts(Vec::new()).expect("clear");
    assert!(doc.facts().is_empty());
}

#[test]
fn cloned_columns_observe_the_same_cells() {
    let mut columns = Columns::new(b"", vec![String::from("$")]);
    columns.push(ColumnCell::Span(ByteRange::try_new(0, 1).expect("ordered")));
    let shared = columns.clone();
    assert_eq!(shared.rows(), 1);
    assert_eq!(shared.cells(), columns.cells());
}

#[test]
fn append_rows_moves_cells_from_a_unique_batch() {
    let mut left = Columns::new(b"", vec![String::from("$")]);
    left.push(ColumnCell::Span(ByteRange::try_new(0, 1).expect("ordered")));
    let mut right = Columns::new(b"", vec![String::from("$")]);
    right.push(ColumnCell::Span(ByteRange::try_new(1, 2).expect("ordered")));
    left.append_rows(&mut right);
    assert_eq!(left.rows(), 2);
    assert_eq!(right.rows(), 0, "append_rows drains the source");
}

#[test]
fn reserve_prepares_the_whole_batch_in_one_allocation() {
    let mut columns = Columns::new(b"", vec![String::from("$")]);
    columns.reserve(4096);
    for i in 0..4096 {
        columns.push(ColumnCell::Span(ByteRange::try_new(i, i + 1).expect("ordered")));
    }
    assert_eq!(columns.cells().len(), 4096);
    assert_eq!(
        columns.capacity(),
        4096,
        "a known batch reserves exactly and push never regrows"
    );
}

#[test]
fn push_growth_stays_within_a_quarter_of_the_used_cells() {
    let mut columns = Columns::new(b"", vec![String::from("$")]);
    for i in 0..20_000_usize {
        columns.push(ColumnCell::Span(ByteRange::try_new(i, i + 1).expect("ordered")));
        let used = columns.cells().len();
        if used >= 1024 {
            let capacity = columns.capacity();
            assert!(
                capacity * 4 <= used * 5,
                "capacity {capacity} exceeds a quarter over {used} cells"
            );
        }
    }
}

#[test]
fn absorb_moves_cells_and_leaves_the_source_batch_empty() {
    let source = b"abcdef";
    let mut acc = Columns::new(source, vec![String::from("c")]);
    acc.push(ColumnCell::Span(range(0, 1)));
    let mut next = Columns::new(source, vec![String::from("c")]);
    next.push(ColumnCell::Span(range(1, 2)));
    let reserved = next.capacity();
    assert!(reserved > 0, "the pushed batch owns a buffer");

    acc.absorb(&mut next);
    assert_eq!(acc.rows(), 2);
    assert!(matches!(acc.cells()[1], ColumnCell::Span(span) if span == range(1, 2)));
    assert_eq!(next.rows(), 0, "absorb empties the source batch");
    assert_eq!(next.capacity(), reserved, "the emptied batch keeps its buffer");

    next.push(ColumnCell::Span(range(2, 3)));
    assert_eq!(next.rows(), 1, "the emptied batch refills");
}

#[test]
fn retain_rows_clamps_to_the_current_rows_and_keeps_the_width() {
    let source = b"abcdefgh";
    let mut batch = Columns::new(source, vec![String::from("a"), String::from("b")]);
    for i in 0..4 {
        batch.push(ColumnCell::Span(range(i, i + 1)));
        batch.push(ColumnCell::Span(range(i, i + 1)));
    }
    assert_eq!((batch.rows(), batch.width()), (4, 2));

    batch.retain_rows(1..3);
    assert_eq!((batch.rows(), batch.width()), (2, 2));
    assert!(matches!(batch.cells()[0], ColumnCell::Span(span) if span == range(1, 2)));

    batch.retain_rows(0..99);
    assert_eq!((batch.rows(), batch.width()), (2, 2), "the end clamps");

    batch.retain_rows(5..7);
    assert_eq!(batch.rows(), 0, "a start past the end clears");

    batch.push(ColumnCell::Span(range(0, 1)));
    batch.push(ColumnCell::Span(range(0, 1)));
    assert_eq!(batch.rows(), 1, "the width survives an empty retain");
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "same width")]
fn append_rows_refuses_a_width_mismatch() {
    let source = b"ab";
    let mut wide = Columns::new(source, vec![String::from("a"), String::from("b")]);
    wide.push(ColumnCell::Span(range(0, 1)));
    wide.push(ColumnCell::Span(range(1, 2)));
    let mut narrow = Columns::new(source, vec![String::from("a")]);
    narrow.push(ColumnCell::Span(range(0, 1)));
    wide.append_rows(&mut narrow);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "same width")]
fn absorb_refuses_a_width_mismatch() {
    let source = b"ab";
    let mut wide = Columns::new(source, vec![String::from("a"), String::from("b")]);
    wide.push(ColumnCell::Span(range(0, 1)));
    wide.push(ColumnCell::Span(range(1, 2)));
    let mut narrow = Columns::new(source, vec![String::from("a")]);
    narrow.push(ColumnCell::Span(range(0, 1)));
    wide.absorb(&mut narrow);
}
