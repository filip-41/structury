//! Materialize an answer to a read value.
//!
//! Missing is an error, not null.

use structury::{Answer, BorrowedDocument, ByteRange, Document, Error, OwnedDocument, Value};

use crate::borrowed::BorrowedPayload;
use crate::dialect::Dialect;
use crate::error;
use crate::tape::{Builder, OwnedPayload, Payload};

/// Artifact a materialize call produces.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Form {
    /// Zero-copy source-backed arena. The default.
    #[default]
    Borrowed,
    /// Arena whose payloads are copied into document-owned storage.
    Owned,
    /// Owned mutable tree, produced from the source-backed view on demand.
    Value,
}

/// Options for [`materialize`] and [`parse`]: the dialect and the artifact [`Form`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct MaterializeOptions {
    /// Grammar the answer is read under.
    pub dialect: Dialect,
    /// Artifact form to produce.
    pub form: Form,
}

impl MaterializeOptions {
    /// Options reading `dialect` and producing `form`.
    #[must_use]
    pub const fn new(dialect: Dialect, form: Form) -> Self {
        Self { dialect, form }
    }

    /// These options reading `dialect`.
    #[must_use]
    pub const fn with_dialect(mut self, dialect: Dialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// These options producing `form`.
    #[must_use]
    pub const fn with_form(mut self, form: Form) -> Self {
        self.form = form;
        self
    }
}

/// Artifact produced by [`materialize`]/[`parse`], selected by [`Form`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Materialized<'src> {
    /// Source-backed arena view ([`Form::Borrowed`]).
    Borrowed(BorrowedDocument<'src>),
    /// Owned arena ([`Form::Owned`]).
    Owned(OwnedDocument),
    /// Owned mutable tree ([`Form::Value`]).
    Value(Value),
}

impl Materialized<'_> {
    /// Consume any form as an owned [`Value`]. Allocates.
    ///
    /// # Panics
    ///
    /// Panics when a borrowed or owned document holds a number node whose
    /// payload is not a valid number spelling, as a document assembled through
    /// the public `from_parts` can hold.
    #[must_use]
    pub fn into_value(self) -> Value {
        match self {
            Self::Value(value) => value,
            Self::Borrowed(document) => document.to_value(),
            Self::Owned(document) => document.to_value(),
        }
    }
}

/// Materialize `answer` under `opts`. The source comes from the answer itself
/// ([`Answer::Document`] binds its document, [`Answer::Columns`] the bytes its
/// spans index), so the spans cannot be read against different bytes:
///
/// ```compile_fail
/// use structury::Answer;
/// use structury_json::{MaterializeOptions, materialize};
///
/// let answer: Answer<'static> = Answer::Missing;
/// let _ = materialize(b"other bytes", &answer, MaterializeOptions::default());
/// ```
///
/// # Errors
///
/// Grammar, value-level checks Lazy deferred, an answer that is not a value, or
/// a JSON5 number with no exact representation.
pub fn materialize<'src>(answer: &Answer<'src>, opts: MaterializeOptions) -> Result<Materialized<'src>, Error> {
    let source: &'src [u8] = match answer {
        Answer::Document(document) => document.source(),
        Answer::Columns(columns) => columns.source(),
        Answer::Oracle(_) | Answer::Missing | Answer::TypeMismatch { .. } => &[],
    };
    match opts.form {
        Form::Borrowed => {
            let mut builder = Builder::<BorrowedPayload<'src>>::new(source, opts.dialect);
            let root = fill(&mut builder, answer)?;
            Ok(Materialized::Borrowed(builder.finish(root)))
        }
        Form::Owned => {
            let mut builder = Builder::<OwnedPayload>::new(source, opts.dialect);
            let root = fill(&mut builder, answer)?;
            Ok(Materialized::Owned(builder.finish(root)))
        }
        Form::Value => {
            let mut builder = Builder::<BorrowedPayload<'src>>::new(source, opts.dialect);
            let root = fill(&mut builder, answer)?;
            Ok(Materialized::Value(builder.finish(root).to_value()))
        }
    }
}

fn fill<'src, P: Payload<'src>>(builder: &mut Builder<'src, P>, answer: &Answer<'src>) -> Result<u32, Error> {
    match answer {
        Answer::Document(document) => builder.span(document.root()),
        Answer::Columns(columns) => builder.columns(columns),
        Answer::Oracle(_) => Err(error::shape("oracle is not a value", 0)),
        Answer::Missing => Err(error::shape("missing is not a value", 0)),
        Answer::TypeMismatch { .. } => Err(error::shape("type mismatch is not a value", 0)),
    }
}

/// Parse one text value under `opts`. Always value-checks.
///
/// # Errors
///
/// Grammar, trailing content, or a JSON5 number with no exact representation.
///
/// # Panics
///
/// Never: the whole-input range is built from the BOM offset and `src.len()`.
pub fn parse(src: &[u8], opts: MaterializeOptions) -> Result<Materialized<'_>, Error> {
    let start = crate::scan::strip_bom(src);
    let span = ByteRange::try_new(start, src.len()).expect("BUG: strip_bom offset never exceeds src.len()");
    let answer = Answer::Document(Document::from_span(src, span));
    materialize(&answer, opts)
}
