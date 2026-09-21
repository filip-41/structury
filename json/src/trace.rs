//! Walk trace: developer-visible record of what a scan did.
//!
//! The fused architecture is fast but opaque: answers say what each demand
//! produced, never what was skipped, where the walk stopped, or how often the
//! control was polled. The trace records those at the point of action.
//! Tracing is monomorphized behind [`TraceSink`], exactly like the walk's
//! `CONTROLLED` flag and fact [`RecordSink`](crate::facts::RecordSink):
//! [`NoTrace`] folds away, [`Recorder`] collects. The hot `scan` entries
//! always run untraced; [`scan_traced`](crate::scan_traced) opts in.

use alloc::vec::Vec;

use structury::{Answer, Demand};

use crate::lex::Check;

/// Developer-visible record of one scan: coarse region events plus counters.
/// Owned by the caller; hosts drop it or render it (text rendering is host-side).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Trace {
    /// Region events. A single walk records in walk order. Stream extras
    /// append their events after the main walk's, so a held-out record's
    /// events come after the later records'. Events carry absolute offsets
    /// and no record index.
    pub events: Vec<TraceEvent>,
    /// Always-counted walk quantities.
    pub counters: TraceCounters,
}

impl Trace {
    /// Fold another trace in (stream extras): events append after the main
    /// walk's; counters merge.
    pub(crate) fn absorb(&mut self, other: Trace) {
        self.events.extend(other.events);
        self.counters.add(&other.counters);
    }

    /// Record one mark per demand from final answers: what each demand
    /// produced, with the span when the answer has one.
    pub(crate) fn record_marks(&mut self, demands: &[Demand], answers: &[Answer<'_>]) {
        self.counters.demands = demands.len() as u64;
        for (idx, answer) in answers.iter().enumerate() {
            let (kind, span) = match answer {
                Answer::Document(doc) => (AnswerKind::Document, Some(doc.root())),
                Answer::Columns(_) => (AnswerKind::Columns, None),
                Answer::Oracle(_) => (AnswerKind::Oracle, None),
                Answer::Missing => (AnswerKind::Missing, None),
                Answer::TypeMismatch { .. } => (AnswerKind::Mismatch, None),
            };
            if !matches!(answer, Answer::Missing) {
                self.counters.answered += 1;
            }
            self.events.push(TraceEvent::Mark {
                demand: u32::try_from(idx).expect("demand count fits u32"),
                kind,
                span,
            });
        }
    }
}

/// One coarse walk region: never per-token, so volume tracks demands plus
/// containers rather than input bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TraceEvent {
    /// One demand's final outcome, in demand order.
    Mark {
        /// Demand index.
        demand: u32,
        /// What the demand produced.
        kind: AnswerKind,
        /// Primary span when the answer has one (documents only).
        span: Option<structury::ByteRange>,
    },
    /// A region located but not walked for answers. Every skip site reports,
    /// including values that become answers: a wanted member locates before
    /// the walk descends into it, and a `Whole` mark locates its span before
    /// answering. Byte counts sum passes, so a region re-read at another
    /// check level counts twice. Levels are [`CheckLevel`].
    Skip {
        /// Region start.
        start: usize,
        /// Region end (exclusive).
        end: usize,
        /// Check the region was read under.
        check: CheckLevel,
    },
    /// The all-slice early stop fired: every demand was a bounded slice on
    /// one array, so the walk ended here instead of at the array end.
    Stop {
        /// Walk position at the stop.
        offset: usize,
    },
}

/// Check level a skipped region was read under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CheckLevel {
    /// Structure only: the region's end was located, its values not checked.
    Locate,
    /// Every byte of the region was value-checked.
    Values,
}

/// What one demand produced. Mirrors [`Answer`] without its payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AnswerKind {
    /// Located document.
    Document,
    /// Column batch.
    Columns,
    /// Walk-filled oracle.
    Oracle,
    /// Path absent (not null).
    Missing,
    /// Demand met the wrong kind.
    Mismatch,
}

/// Always-counted walk quantities.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct TraceCounters {
    /// Demands in the request.
    pub demands: u64,
    /// Marks that are not missing.
    pub answered: u64,
    /// Skip regions recorded.
    pub skips: u64,
    /// Bytes located but not walked.
    pub skipped_bytes: u64,
    /// Control polls executed.
    pub polls: u64,
    /// Early stops fired.
    pub stops: u64,
}

impl TraceCounters {
    /// Merge an extra walker's counters. The request-wide `demands` and
    /// `answered` fields describe the final marks, which [`Trace::record_marks`]
    /// records after the merge, so only the walk-local counts move here.
    fn add(&mut self, other: &TraceCounters) {
        self.skips += other.skips;
        self.skipped_bytes += other.skipped_bytes;
        self.polls += other.polls;
        self.stops += other.stops;
    }
}

/// Check level as a trace payload.
pub(crate) fn check_level(check: Check) -> CheckLevel {
    match check {
        Check::Locate => CheckLevel::Locate,
        Check::Values => CheckLevel::Values,
    }
}

/// Where the walk reports what it does. Mirrors
/// [`RecordSink`](crate::facts::RecordSink): monomorphized, so [`NoTrace`]
/// carries no state and folds away.
pub(crate) trait TraceSink: Sized {
    /// Whether this instantiation records. Constant per monomorphization.
    const RECORDS: bool;

    /// Empty sink.
    fn new() -> Self;
    /// One located-but-unwalked region. Empty regions are dropped.
    fn note_skip(&mut self, start: usize, end: usize, check: CheckLevel);
    /// One control poll executed.
    fn note_poll(&mut self);
    /// One early stop at `offset`.
    fn note_stop(&mut self, offset: usize);
    /// Drain the collected trace.
    fn take_trace(&mut self) -> Trace;
}

/// Tracing off: every operation folds away.
#[derive(Clone, Copy, Default)]
pub(crate) struct NoTrace;

impl TraceSink for NoTrace {
    const RECORDS: bool = false;

    #[inline]
    fn new() -> Self {
        Self
    }

    #[inline]
    fn note_skip(&mut self, _start: usize, _end: usize, _check: CheckLevel) {}

    #[inline]
    fn note_poll(&mut self) {}

    #[inline]
    fn note_stop(&mut self, _offset: usize) {}

    #[inline]
    fn take_trace(&mut self) -> Trace {
        Trace::default()
    }
}

/// Tracing on: events append, counters always count.
pub(crate) struct Recorder {
    trace: Trace,
}

impl TraceSink for Recorder {
    const RECORDS: bool = true;

    #[inline]
    fn new() -> Self {
        Self {
            trace: Trace::default(),
        }
    }

    fn note_skip(&mut self, start: usize, end: usize, check: CheckLevel) {
        if end > start {
            self.trace.counters.skips += 1;
            self.trace.counters.skipped_bytes += (end - start) as u64;
            self.trace.events.push(TraceEvent::Skip { start, end, check });
        }
    }

    #[inline]
    fn note_poll(&mut self) {
        self.trace.counters.polls += 1;
    }

    fn note_stop(&mut self, offset: usize) {
        self.trace.counters.stops += 1;
        self.trace.events.push(TraceEvent::Stop { offset });
    }

    fn take_trace(&mut self) -> Trace {
        core::mem::take(&mut self.trace)
    }
}
