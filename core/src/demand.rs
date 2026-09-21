//! What to locate and how much to build.
//!
//! Overlapping demands stay separate marks while still sharing a single pass.
//! The default [`Strictness`] is [`Strictness::Structural`].
//!
//! [`Demand::Path`] and the codec's `Edit` name a chain as a `Vec<Step>`.
//! [`Demand::Project`] and [`Demand::Filter`] wrap the same chain in [`Path`]:
//! the raw vector lets a caller pass steps straight through, and [`Path`]
//! carries the builders. Build steps with [`Step::key`] and [`Step::index`],
//! and chains with [`Path::push_key`] and [`Path::push_index`].

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::value::Value;

/// Object member name.
pub type Name = String;

/// One static path step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    /// Object member, compared as the decoded key.
    Key(Name),
    /// Array index; negatives count from the end.
    Index(i64),
}

impl Step {
    /// Object-member step.
    #[must_use]
    pub fn key(name: impl Into<String>) -> Self {
        Self::Key(name.into())
    }

    /// Array-index step; negatives count from the end.
    #[must_use]
    pub const fn index(index: i64) -> Self {
        Self::Index(index)
    }
}

/// Static key/index chain.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Path {
    /// Steps from the current node.
    pub steps: Vec<Step>,
}

impl Path {
    /// Empty path (the current node).
    #[must_use]
    pub const fn root() -> Self {
        Self { steps: Vec::new() }
    }

    /// One key step.
    #[must_use]
    pub fn key(name: impl Into<String>) -> Self {
        Self {
            steps: alloc::vec![Step::Key(name.into())],
        }
    }

    /// One index step.
    #[must_use]
    pub fn index(index: i64) -> Self {
        Self {
            steps: alloc::vec![Step::Index(index)],
        }
    }

    /// Append a step.
    #[must_use]
    pub fn push(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }

    /// Append an object-member step.
    #[must_use]
    pub fn push_key(self, name: impl Into<String>) -> Self {
        self.push(Step::key(name))
    }

    /// Append an array-index step.
    #[must_use]
    pub fn push_index(self, index: i64) -> Self {
        self.push(Step::index(index))
    }
}

impl From<Vec<Step>> for Path {
    fn from(steps: Vec<Step>) -> Self {
        Self { steps }
    }
}

/// Half-open incoming window, `None` points to an edge.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Range {
    /// Inclusive start, or the container start.
    pub start: Option<i64>,
    /// Exclusive end, or the container end.
    pub end: Option<i64>,
}

impl Range {
    /// Resolve against `len` as a half-open `start..end` window.
    #[must_use]
    pub fn window(self, len: usize) -> core::ops::Range<usize> {
        let start = bound(len, self.start, true);
        let end = bound(len, self.end, false);
        start..end.min(len).max(start)
    }
}

fn bound(len: usize, edge: Option<i64>, is_start: bool) -> usize {
    match edge {
        None => {
            if is_start {
                0
            } else {
                len
            }
        }
        Some(i) if i >= 0 => usize::try_from(i).unwrap_or(len).min(len),
        Some(i) => {
            let mag = i.checked_neg().and_then(|m| usize::try_from(m).ok()).unwrap_or(len);
            len.saturating_sub(mag)
        }
    }
}

/// Strictness dial. Structure is never skipped; queries never require Strict,
/// writes always do. Default [`Strictness::Structural`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Strictness {
    /// Structure + all values, document order.
    Strict,
    /// Structure + demanded values, demand order. Default.
    #[default]
    Structural,
    /// Structure only; value checks on materialize.
    Lazy,
}

/// How one demand's per-shard answers fold back into the whole answer; a
/// [`Shard::Serial`] demand is never answered per range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shard {
    /// One range: the demand observes the whole node.
    Serial,
    /// Per-range answers concatenate in document order.
    Concat,
    /// Per-range answers are partial totals to add.
    Sum,
}

impl Shard {
    /// Whether a demand with this kind may be answered per shard.
    #[must_use]
    pub const fn is_parallel(self) -> bool {
        !matches!(self, Self::Serial)
    }
}

/// Closed oracle filled on the walk. Not a document. The codec matches it
/// exhaustively, so a new variant is a deliberate minor-version change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Oracle {
    /// Direct child or member count of a located array or object; a non-container
    /// answers a type mismatch. The polymorphic container length; use
    /// [`Oracle::MemberCount`] for the object-only form.
    Count,
    /// This node plus every descendant (preorder count).
    DescendCount,
    /// Kind of the located node.
    Kind,
    /// Object-member presence after last-wins.
    HasKey {
        /// Member name.
        key: Name,
    },
    /// Last-wins member names of the located object, first-key order; a
    /// non-object answers a type mismatch.
    MemberNames,
    /// Last-wins member count of a located object; a non-object answers a type
    /// mismatch.
    MemberCount,
    /// Byte length of the decoded string; a non-string answers a type mismatch.
    StringByteLength,
}

/// Filter predicate over an object row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Predicate {
    /// `field == value`.
    Eq {
        /// Member name.
        field: Name,
        /// Comparison value.
        value: Value,
    },
    /// `field != value`.
    Ne {
        /// Member name.
        field: Name,
        /// Comparison value.
        value: Value,
    },
    /// Numeric `field > value`.
    Gt {
        /// Member name.
        field: Name,
        /// Comparison value.
        value: Value,
    },
    /// Numeric `field < value`.
    Lt {
        /// Member name.
        field: Name,
        /// Comparison value.
        value: Value,
    },
    /// Numeric `field >= value`.
    Ge {
        /// Member name.
        field: Name,
        /// Comparison value.
        value: Value,
    },
    /// Numeric `field <= value`.
    Le {
        /// Member name.
        field: Name,
        /// Comparison value.
        value: Value,
    },
    /// Conjunction.
    And(Box<Predicate>, Box<Predicate>),
    /// Disjunction.
    Or(Box<Predicate>, Box<Predicate>),
    /// Negation.
    Not(Box<Predicate>),
}

impl Predicate {
    /// Evaluate this predicate against an object `value`. A value that is not an
    /// object has no fields, so only [`Self::Ne`] matches it, exactly as an
    /// absent field does.
    ///
    /// Equality uses [`Value::equal`] (numbers by value; objects by member name,
    /// ignoring order; everything else structurally); the ordering arms use
    /// [`Value::compare`] (numbers by value, a boolean as `1`/`0`, any other kind
    /// no match).
    ///
    /// A codec that answers filters from source spans without materializing a
    /// value may refuse a container-valued `Eq`/`Ne` operand with
    /// [`crate::ErrorClass::Shape`] rather than compare it structurally.
    #[must_use]
    pub fn matches(&self, value: &Value) -> bool {
        match self {
            Self::And(a, b) => a.matches(value) && b.matches(value),
            Self::Or(a, b) => a.matches(value) || b.matches(value),
            Self::Not(p) => !p.matches(value),
            Self::Eq { field, value: want } => value.member(field).is_some_and(|got| got.equal(want)),
            Self::Ne { field, value: want } => value.member(field).is_none_or(|got| !got.equal(want)),
            Self::Gt { field, value: want } => order(value, field, want, Cmp::Gt),
            Self::Lt { field, value: want } => order(value, field, want, Cmp::Lt),
            Self::Ge { field, value: want } => order(value, field, want, Cmp::Ge),
            Self::Le { field, value: want } => order(value, field, want, Cmp::Le),
        }
    }
}

/// The ordering arms a predicate can ask for.
#[derive(Clone, Copy)]
enum Cmp {
    Gt,
    Lt,
    Ge,
    Le,
}

/// Whether the ordering arm `cmp` holds for `row.field` against `want`.
fn order(row: &Value, field: &str, want: &Value, cmp: Cmp) -> bool {
    let Some(order) = row.member(field).and_then(|got| got.compare(want)) else {
        return false;
    };
    match order {
        Ordering::Greater => matches!(cmp, Cmp::Gt | Cmp::Ge),
        Ordering::Less => matches!(cmp, Cmp::Lt | Cmp::Le),
        Ordering::Equal => matches!(cmp, Cmp::Ge | Cmp::Le),
    }
}

/// What to locate and how much to build. One pass, N demands. The codec matches
/// it exhaustively, so a new variant is a deliberate minor-version change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Demand {
    /// The entire document value.
    Whole,
    /// Static key/index chain, then optional nested demand.
    Path {
        /// Steps from this node.
        steps: Vec<Step>,
        /// Demand under the located node.
        nested: Option<Box<Demand>>,
    },
    /// Array of objects (or of the nested demand).
    Collection {
        /// Keep these object fields; `None` keeps every member.
        fields: Option<Vec<Name>>,
        /// Per-element demand.
        nested: Option<Box<Demand>>,
    },
    /// Window of a collection.
    Slice {
        /// `[start:end]` before negative bounds are resolved.
        range: Range,
        /// Per-element demand inside the window.
        nested: Option<Box<Demand>>,
    },
    /// Path to an array of objects; keep `fields` per element.
    Project {
        /// Path to the array.
        path: Path,
        /// Member names to keep.
        fields: Vec<Name>,
    },
    /// Path to an array of objects; keep rows matching `predicate`.
    Filter {
        /// Path to the array.
        path: Path,
        /// Row predicate.
        predicate: Predicate,
        /// Member names to keep on matching rows. Empty keeps the whole row.
        project: Vec<Name>,
    },
    /// Closed oracle at this node.
    Oracle(Oracle),
}

impl Demand {
    /// `Path` with no nested demand, from any step sequence.
    #[must_use]
    pub fn path(steps: impl IntoIterator<Item = Step>) -> Self {
        Self::Path {
            steps: steps.into_iter().collect(),
            nested: None,
        }
    }

    /// Keyless `Collection` over every member, or one keeping `fields`.
    #[must_use]
    pub const fn collection(fields: Option<Vec<Name>>) -> Self {
        Self::Collection { fields, nested: None }
    }

    /// `Project` of `fields` at `path`.
    #[must_use]
    pub const fn project(path: Path, fields: Vec<Name>) -> Self {
        Self::Project { path, fields }
    }

    /// `Filter` of rows matching `predicate`, keeping `project` (empty keeps the
    /// whole row).
    #[must_use]
    pub const fn filter(path: Path, predicate: Predicate, project: Vec<Name>) -> Self {
        Self::Filter {
            path,
            predicate,
            project,
        }
    }

    /// `Slice` over `range` with no nested demand.
    #[must_use]
    pub const fn slice(range: Range) -> Self {
        Self::Slice { range, nested: None }
    }

    /// Attach a nested demand to a `Path`, `Collection`, or `Slice`; any other
    /// shape is returned unchanged.
    #[must_use]
    pub fn nested(self, nested: Demand) -> Self {
        match self {
            Self::Path { steps, .. } => Self::Path {
                steps,
                nested: Some(Box::new(nested)),
            },
            Self::Collection { fields, .. } => Self::Collection {
                fields,
                nested: Some(Box::new(nested)),
            },
            Self::Slice { range, .. } => Self::Slice {
                range,
                nested: Some(Box::new(nested)),
            },
            other => other,
        }
    }

    /// How this demand folds across shard ranges: a **proven whitelist**.
    ///
    /// A flat `Project`/`Filter`, a keyless `Collection` (or its per-element row
    /// demand, without a spent `Path{[]}` wrapper), a key-only `Path` to
    /// [`Oracle::Count`], and a root [`Oracle::Count`] shard; every other shape
    /// is [`Shard::Serial`] — correct, merely not parallel.
    #[must_use]
    pub fn shard(&self) -> Shard {
        match self {
            Self::Project { path, .. } | Self::Filter { path, .. } if key_steps(&path.steps) => Shard::Concat,
            Self::Collection { nested: None, .. } => Shard::Concat,
            Self::Collection {
                fields: None,
                nested: Some(nested),
            } => match nested.as_ref() {
                Self::Project { path, .. } | Self::Filter { path, .. } if path.steps.is_empty() => Shard::Concat,
                _ => Shard::Serial,
            },
            Self::Path {
                steps,
                nested: Some(nested),
            } if key_steps(steps) && matches!(**nested, Self::Oracle(Oracle::Count)) => Shard::Sum,
            Self::Oracle(Oracle::Count) => Shard::Sum,
            _ => Shard::Serial,
        }
    }
}

/// A path of object keys only: it directly locates the keyed array to map (an
/// `Index` selects one element, not an array).
fn key_steps(steps: &[Step]) -> bool {
    steps.iter().all(|step| matches!(step, Step::Key(_)))
}
