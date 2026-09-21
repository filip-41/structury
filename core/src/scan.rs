//! Scan results, answers, and byte ranges.
//!
//! `answers.len() == demands.len()`. Missing is [`Answer::Missing`], not [`crate::Value::Null`].

use alloc::vec::Vec;

use crate::document::{Columns, Document};
use crate::value::ValueKind;

/// Half-open `[start, end)` byte range. `start <= end`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteRange {
    start: usize,
    end: usize,
}

impl ByteRange {
    /// `None` when `start > end`.
    #[inline]
    #[must_use]
    pub const fn try_new(start: usize, end: usize) -> Option<Self> {
        if start > end { None } else { Some(Self { start, end }) }
    }

    /// Inclusive start.
    #[inline]
    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    /// Exclusive end.
    #[inline]
    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }

    /// Length in bytes.
    #[inline]
    #[must_use]
    pub const fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the range is empty.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// One complete document value's answers.
#[derive(Clone, Debug)]
#[must_use = "scan answers are the result"]
#[non_exhaustive]
pub struct ScanResult<'src> {
    /// Zip with the request's demands. One answer per demand.
    pub answers: Vec<Answer<'src>>,
    /// Recovering issues (for example a truncated trailing record).
    pub issues: Vec<crate::error::Issue>,
}

impl<'src> ScanResult<'src> {
    /// A result of `answers` with `issues`.
    pub const fn new(answers: Vec<Answer<'src>>, issues: Vec<crate::error::Issue>) -> Self {
        Self { answers, issues }
    }
}

/// One demand's answer. A located value is always a [`Document`], the one value handle.
///
/// Diagnostics are deliberately minimal: the answer carries only the kind it
/// found ([`Answer::TypeMismatch`]) or that the path was absent
/// ([`Answer::Missing`]). The failing step is not repeated here because
/// `answers` zips one-to-one with the request's demands, whose static path names
/// it. The codec matches this exhaustively.
#[derive(Clone, Debug)]
pub enum Answer<'src> {
    /// Located document value for this demand. Facts are empty for a plain span.
    Document(Document<'src>),
    /// Projected columns, shared on clone and bound to the bytes their spans index.
    Columns(Columns<'src>),
    /// Oracle filled on the walk.
    Oracle(OracleAnswer),
    /// Path absent, not found.
    Missing,
    /// Intermediate step met the wrong kind.
    TypeMismatch {
        /// Kind actually found.
        actual: ValueKind,
    },
}

/// Oracle payload. Member names are first-key last-wins order.
/// Oracles are meant to be a shortcut for work that requires **just** a scan.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OracleAnswer {
    /// Container child/member count, or descend-count (this node plus every
    /// descendant).
    Count(u64),
    /// Kind of the located node.
    Kind(ValueKind),
    /// Object-member presence after last-wins.
    HasKey(bool),
    /// Last-wins member names of the located object, first-key order.
    MemberNames(Vec<alloc::string::String>),
    /// Byte length of the located string.
    StringByteLength(u64),
}
