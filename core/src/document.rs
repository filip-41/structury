//! Borrowed, format-faithful document.
//!
//! Optional [`Fact`]s attach to nodes; [`Document::is_fully_validated`] is the write gate.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::error::{Error, ErrorClass};
use crate::scan::ByteRange;

/// Attachment role of a [`Fact`]. The codec matches it exhaustively, so a later
/// codec that attaches a different kind of metadata adds a variant as a
/// deliberate minor-version change.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FactRole {
    /// Trivia before a node, attached to the node that follows.
    CommentLead,
    /// Trivia after a node on its last line, attached to the node it follows.
    CommentFoot,
    /// Trivia the grammar attaches without a sibling position.
    CommentInline,
}

/// The node a [`Fact`] attaches to, named by its source span (a document has no
/// node table).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FactOwner {
    node: ByteRange,
}

impl FactOwner {
    /// Owner is the node spanning `node`.
    #[must_use]
    pub const fn node(node: ByteRange) -> Self {
        Self { node }
    }

    /// Span of the owner node.
    #[must_use]
    pub const fn span(self) -> ByteRange {
        self.node
    }
}

/// One attached metadata record. [`Fact`]s cannot change a node's meaning; a
/// codec attaches whatever its grammar carries. `text` borrows the source for
/// valid UTF-8, so a fact costs no allocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fact<'src> {
    role: FactRole,
    text: Cow<'src, str>,
    source_span: Option<ByteRange>,
    owner: FactOwner,
}

impl<'src> Fact<'src> {
    /// Fact with `role` and `text`, an authored glyph `source_span` (present only
    /// when the codec retained glyphs), attached to `owner`.
    #[must_use]
    pub const fn new(role: FactRole, text: Cow<'src, str>, source_span: Option<ByteRange>, owner: FactOwner) -> Self {
        Self {
            role,
            text,
            source_span,
            owner,
        }
    }

    /// Attachment role.
    #[must_use]
    pub const fn role(&self) -> FactRole {
        self.role
    }

    /// The fact's UTF-8 text body. A comment fact carries the body without its
    /// `//` or `/* */` delimiters.
    #[must_use]
    pub const fn text(&self) -> &str {
        match &self.text {
            Cow::Borrowed(text) => text,
            Cow::Owned(text) => text.as_str(),
        }
    }

    /// Authored glyph span this fact addresses, when the codec retained one.
    #[must_use]
    pub const fn source_span(&self) -> Option<ByteRange> {
        self.source_span
    }

    /// Owner this fact attaches to.
    #[must_use]
    pub const fn owner(&self) -> FactOwner {
        self.owner
    }
}

/// Opaque grammar provenance a validating codec records on a [`Document`].
///
/// Core never interprets a tag: it exists so a codec can tell a document it
/// validated under one grammar from another — the JSON codec refuses a
/// byte-preserving write whose grammar does not match the options. Compare
/// tags for equality only; the numeric space belongs to the codec that
/// assigned the tag.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GrammarTag(u8);

impl GrammarTag {
    /// A document whose grammar the caller did not record. A codec may refuse a
    /// byte-preserving write of such a document.
    pub const UNKNOWN: Self = Self(0);

    /// The tag a codec assigns to one of its grammars. **Codec-internal**: a
    /// dependent must not invent tags, only pass back the codec's own. `0` is
    /// reserved for [`GrammarTag::UNKNOWN`].
    #[doc(hidden)]
    #[must_use]
    pub const fn codec(tag: u8) -> Self {
        Self(tag)
    }

    /// Raw tag value. Compare for equality; do not interpret.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Borrowed document over `source`, carrying the grammar a validating pass
/// recorded for it.
#[derive(Clone, Debug)]
pub struct Document<'src> {
    source: &'src [u8],
    root: ByteRange,
    fully_validated: bool,
    grammar: GrammarTag,
    facts: Vec<Fact<'src>>,
}

impl<'src> Document<'src> {
    const fn span(source: &'src [u8], root: ByteRange, fully_validated: bool, grammar: GrammarTag) -> Self {
        Self {
            source,
            root,
            fully_validated,
            grammar,
            facts: Vec::new(),
        }
    }

    /// Span-only document that has **not** been fully validated. The write gate
    /// refuses [`crate::Value`]-emitting writes on it. A fully validating pass
    /// produces a separately named constructor, so the gate cannot be flipped by
    /// a typo.
    #[must_use]
    pub const fn from_span(source: &'src [u8], root: ByteRange) -> Self {
        Self::span(source, root, false, GrammarTag::UNKNOWN)
    }

    /// Span-only document produced by a fully validating (Strict) pass, so the
    /// write gate is open and `grammar` is the codec's grammar provenance.
    ///
    /// **Codec-internal.** A caller that has not actually validated the bytes
    /// must use [`Self::from_span`] as this constructor records the codec's own
    /// Strict result, it does not perform validation. It is `#[doc(hidden)]`
    /// because a codec in a sibling crate needs it, but it remains public and is
    /// not hidden from the type system.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_span_validated(source: &'src [u8], root: ByteRange, grammar: GrammarTag) -> Self {
        Self::span(source, root, true, grammar)
    }

    /// Source bytes.
    #[must_use]
    pub const fn source(&self) -> &'src [u8] {
        self.source
    }

    /// Root span.
    #[must_use]
    pub const fn root(&self) -> ByteRange {
        self.root
    }

    /// Root source slice.
    #[must_use]
    pub fn root_bytes(&self) -> &'src [u8] {
        self.source.get(self.root.start()..self.root.end()).unwrap_or(&[])
    }

    /// Whether every value in the source was checked. Write gate: Strict only.
    #[must_use]
    pub const fn is_fully_validated(&self) -> bool {
        self.fully_validated
    }

    /// Grammar provenance a validating codec recorded, or
    /// [`GrammarTag::UNKNOWN`] for a span-only document. Compare for equality;
    /// the tag's meaning belongs to the codec that assigned it.
    #[must_use]
    pub const fn grammar(&self) -> GrammarTag {
        self.grammar
    }

    /// Every retained fact, in attachment order.
    #[must_use]
    pub fn facts(&self) -> &[Fact<'src>] {
        &self.facts
    }

    /// Whether `facts` are ascending by glyph start; ties (dual records) are allowed and
    /// glyph-less facts skipped.
    #[must_use]
    pub(crate) fn facts_are_ordered(facts: &[Fact<'_>]) -> bool {
        let mut last: Option<usize> = None;
        for fact in facts {
            if let Some(span) = fact.source_span() {
                if last.is_some_and(|previous| span.start() < previous) {
                    return false;
                }
                last = Some(span.start());
            }
        }
        true
    }

    /// Replace every fact; the document is untouched when `facts` break the
    /// order invariant.
    ///
    /// # Errors
    ///
    /// [`ErrorClass::Write`] when a glyph span is out of order.
    pub fn set_facts(&mut self, facts: Vec<Fact<'src>>) -> Result<(), Error> {
        Self::check_facts(&facts)?;
        self.facts = facts;
        Ok(())
    }

    /// Authored glyph bytes of `fact`. Empty when it has no glyph span or the span is out of bounds.
    #[must_use]
    pub fn fact_bytes(&self, fact: &Fact<'src>) -> &'src [u8] {
        match fact.source_span() {
            Some(span) => self.source.get(span.start()..span.end()).unwrap_or(&[]),
            None => &[],
        }
    }

    fn check_facts(facts: &[Fact<'_>]) -> Result<(), Error> {
        if Self::facts_are_ordered(facts) {
            return Ok(());
        }
        let offset = facts
            .windows(2)
            .find(|pair| match (pair[0].source_span(), pair[1].source_span()) {
                (Some(a), Some(b)) => b.start() < a.start(),
                _ => false,
            })
            .map_or(0, |pair| pair[1].source_span().map_or(0, ByteRange::start));
        Err(Error::new(
            ErrorClass::Write,
            "facts-not-ordered",
            "facts must be ascending by glyph span",
            offset,
        ))
    }
}

/// One cell in a [`Columns`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColumnCell {
    /// Present value as a source span.
    Span(ByteRange),
    /// Missing key. Not [`crate::Value::Null`].
    Absent,
}

/// Whether two batches index the same bytes. Identity, not content: equal bytes at
/// different addresses are a mismatch; two empty sources are interchangeable.
pub(crate) fn same_source(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && (a.is_empty() || core::ptr::eq(a.as_ptr(), b.as_ptr()))
}

/// Refuse a merge of two batches whose spans index different bytes
///
/// # Panics
///
/// When `a` and `b` are not the same source.
fn assert_same_source(a: &[u8], b: &[u8]) {
    assert!(
        same_source(a, b),
        "Columns rows across different sources: spans would read the wrong bytes"
    );
}

/// Ensure room for `additional` more cells, growing a quarter of the current
/// capacity at a time (floored at one 16-cell block). Doubling overshoots a long
/// accumulation's peak by up to 2x, an eighth bounds the peak but carries ~9x the
/// final buffer in churn, while a quarter sits between. A producer-supplied count
/// through [`Columns::reserve`] is the exact fix.
fn grow_cells(cells: &mut Vec<ColumnCell>, additional: usize) {
    if cells.capacity() - cells.len() < additional {
        let chunk = (cells.capacity() / 4).max(16);
        cells.reserve_exact(additional.max(chunk));
    }
}

/// Row-major projected batch (`cells[row * width + col]`). Cells sit behind an
/// [`Arc`], so cloning for a second demand shares them; a batch under construction
/// is never shared. Binds the `source` its spans index.
#[derive(Clone, Debug)]
pub struct Columns<'src> {
    source: &'src [u8],
    fields: Arc<Vec<String>>,
    cells: Arc<Vec<ColumnCell>>,
}

impl<'src> Columns<'src> {
    /// Empty batch over `source` with `fields`.
    #[must_use]
    pub fn new(source: &'src [u8], fields: Vec<String>) -> Self {
        Self {
            source,
            fields: Arc::new(fields),
            cells: Arc::new(Vec::new()),
        }
    }

    /// The bytes this batch's [`ColumnCell::Span`]s index.
    #[must_use]
    pub const fn source(&self) -> &'src [u8] {
        self.source
    }

    /// Field names.
    #[must_use]
    pub fn fields(&self) -> &[String] {
        &self.fields
    }

    /// Column count.
    #[must_use]
    pub fn width(&self) -> usize {
        self.fields.len()
    }

    /// Row-major cells, `rows * width()`.
    #[must_use]
    pub fn cells(&self) -> &[ColumnCell] {
        &self.cells
    }

    /// Number of rows. Zero when there are no fields.
    #[must_use]
    pub fn rows(&self) -> usize {
        if self.fields.is_empty() {
            0
        } else {
            self.cells.len() / self.fields.len()
        }
    }

    /// Column `index` in row order (`cells[row * width + index]`); empty when out of range.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    pub fn column(&self, index: usize) -> impl Iterator<Item = &ColumnCell> + '_ {
        let width = self.width().max(1);
        let height = if index < self.width() { self.rows() } else { 0 };
        self.cells.iter().skip(index).step_by(width).take(height)
    }

    /// Append one cell to the last row; allocation-free while unshared with spare capacity.
    pub fn push(&mut self, cell: ColumnCell) {
        let cells = Arc::make_mut(&mut self.cells);
        grow_cells(cells, 1);
        cells.push(cell);
    }

    /// Reserve capacity for `additional` more cells, so a known batch never reallocates on push.
    pub fn reserve(&mut self, additional: usize) {
        Arc::make_mut(&mut self.cells).reserve(additional);
    }

    /// Reserve capacity for `rows` more rows of this batch's width.
    pub fn reserve_rows(&mut self, rows: usize) {
        self.reserve(rows.saturating_mul(self.width().max(1)));
    }

    /// Cell capacity of the batch, as [`Vec::capacity`]; lets a caller check that
    /// a [`Self::reserve`] took effect before any push.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cells.capacity()
    }

    /// Move every cell of `other` after this batch's cells. Both share a field
    /// set **and a source**, so this appends rows.
    ///
    /// # Panics
    ///
    /// When the two do not index the same bytes; the appended
    /// spans would otherwise read the wrong buffer.
    pub fn append_rows(&mut self, other: &mut Self) {
        assert_same_source(self.source, other.source);
        debug_assert_eq!(
            self.width(),
            other.width(),
            "append_rows needs one field set: both batches must have the same width"
        );
        let extra = core::mem::take(Arc::make_mut(&mut other.cells));
        Arc::make_mut(&mut self.cells).extend(extra);
    }

    /// Append `other`'s cells after this batch's, leaving `other` empty but
    /// keeping its capacity, so a stream accumulator that refills one batch per
    /// row never reallocates.
    ///
    /// # Panics
    ///
    /// When the two do not index the same bytes; see [`Self::append_rows`].
    pub fn absorb(&mut self, other: &mut Self) {
        assert_same_source(self.source, other.source);
        debug_assert_eq!(
            self.width(),
            other.width(),
            "absorb needs one field set: both batches must have the same width"
        );
        let extra = Arc::make_mut(&mut other.cells);
        let cells = Arc::make_mut(&mut self.cells);
        grow_cells(cells, extra.len());
        cells.extend_from_slice(extra);
        extra.clear();
    }

    /// Keep only the rows in `rows`, dropping the rest. `rows` is clamped to
    /// the current row count.
    pub fn retain_rows(&mut self, rows: core::ops::Range<usize>) {
        let width = self.width().max(1);
        let height = self.rows();
        let start = rows.start.min(height);
        let end = rows.end.min(height);
        let cells = Arc::make_mut(&mut self.cells);
        if start >= end {
            cells.clear();
            return;
        }
        cells.drain(..start * width);
        cells.truncate((end - start) * width);
    }
}
