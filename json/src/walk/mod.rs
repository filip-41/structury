//! One validating walk fills N marks.
//!
//! Byte-level scanning goes through the [`scan::Scan`] seam. RFC reads
//! [`rfc::RfcScan`]. Other dialects read the lexer. Unread regions skip at the
//! strictness dial. Overlapping demands keep separate marks.

#![expect(
    clippy::inline_always,
    reason = "the per-token scan must fold into the walk; a call per token is the residual this removes"
)]

mod dialect;
mod member;
mod rfc;
mod rows;
mod scan;
mod stream;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use structury::{
    Answer, ByteRange, ColumnCell, Columns, Control, Demand, Document, Fact, Name, Oracle, OracleAnswer, Predicate,
    Range, Step, Strictness, Value, ValueKind,
};

use crate::dialect::Dialect;
use crate::error::small::{self as error, SmallErr};
use crate::facts::{NoRec, RecordSink, Recorder};
use crate::lex::string::plain_double_quoted;
use crate::lex::{self, Check};

use self::dialect::DialectScan;
use self::member::{Members, RowLayout, Wanted, member_bytes, member_name, pred_on_members, read_key};
use self::rfc::RfcScan;
use self::rows::{
    RowShape, apply_object_row, assign_document, flat_index_row, flatten_view, push_absent_row, row_shape,
};
use self::scan::Scan;
use self::stream::{next_run_value, replay_element};

#[cfg(test)]
use self::member::{Head, head_prefix_eq};
pub(crate) use self::rows::{RowLaw, element_row_law, row_fields, stream_every_law};
pub(crate) use self::stream::{StreamWalker, run_next_value, scan_element_run};

const fn no_clock() -> u64 {
    0
}

/// The zero control: a handle that never stops. The `CONTROLLED = false` walk
/// never reads a field, so this handle and every poll fold away.
pub(crate) static NO_CONTROL: Control = Control {
    stop: core::sync::atomic::AtomicU8::new(0),
    used: core::sync::atomic::AtomicU64::new(0),
    ceiling: u64::MAX,
    deadline: None,
    now: no_clock,
};

/// Poll a host control at `at`. A stop is a control refusal at `at`.
#[inline(always)]
pub(crate) fn check_control(control: &Control, at: usize) -> Result<(), SmallErr> {
    use core::sync::atomic::Ordering;
    if control.stop.load(Ordering::Acquire) != 0 {
        return Err(error::cancelled(at));
    }
    if let Some(deadline) = control.deadline
        && (control.now)() >= deadline
    {
        return Err(error::deadline_exceeded(at));
    }
    if control.used.load(Ordering::Relaxed) >= control.ceiling {
        return Err(error::memory_exceeded(at));
    }
    Ok(())
}

/// Stack capacity for one container's child hits. A narrow demand is served
/// without a per-element heap allocation; wider demand sets spill to the heap.
const CHILD_HITS: usize = 8;

pub(crate) struct Walker<'src, 'c, 'f, S, const CONTROLLED: bool, R = NoRec> {
    pub bytes: &'src [u8],
    pub pos: usize,
    pub strictness: Strictness,
    max: u32,
    depth: u32,
    dialect: Dialect,
    facts: &'f [Fact<'src>],
    /// Host control. Read only when `CONTROLLED`; the no-control instantiation
    /// never touches it, so this word and every poll fold away.
    control: &'c Control,
    scratch: String,
    members: Members,
    member_pool: Vec<Members>,
    row_layout: RowLayout,
    repeat_depth: u32,
    /// Active collection row accumulators, one per row hit of the innermost
    /// element loop. The element's own answer writes to its mark while its
    /// accumulator waits here, so a terminal element answer (`TypeMismatch`, a
    /// bare `Document`, `Missing`) cannot retract rows already folded. LIFO: a
    /// nested element loop pushes above its parent, so an outer accumulator is
    /// unreachable from the nested walk.
    rows: Vec<Answer<'src>>,
    allow_stop: bool,
    stop: bool,
    /// A `replay_element` re-walk; it records nothing so facts are not doubled.
    replaying: bool,
    scan: core::marker::PhantomData<S>,
    rec: R,
}

#[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
pub(crate) fn scan_root<'src, const CONTROLLED: bool>(
    bytes: &'src [u8],
    start: usize,
    demands: &[Demand],
    strictness: Strictness,
    max: u32,
    dialect: Dialect,
    facts: &[Fact<'src>],
    allow_stop: bool,
    control: &Control,
) -> Result<(Vec<Answer<'src>>, usize, bool), SmallErr> {
    let retained = matches!(strictness, Strictness::Strict);
    let (answers, end, stop, _) = if dialect == Dialect::Rfc8259 {
        scan_root_with::<RfcScan, NoRec, CONTROLLED>(
            bytes, start, demands, strictness, max, dialect, facts, allow_stop, retained, control,
        )?
    } else {
        scan_root_with::<DialectScan, NoRec, CONTROLLED>(
            bytes, start, demands, strictness, max, dialect, facts, allow_stop, retained, control,
        )?
    };
    Ok((answers, end, stop))
}

/// [`scan_root`] fused with the comment recorder: the walk records each trivia
/// gap it skips and materializes the standalone collector's facts.
#[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
pub(crate) fn scan_root_fused<'src, const CONTROLLED: bool>(
    bytes: &'src [u8],
    start: usize,
    demands: &[Demand],
    strictness: Strictness,
    max: u32,
    dialect: Dialect,
    allow_stop: bool,
    control: &Control,
) -> Result<(Vec<Answer<'src>>, usize, bool, Vec<Fact<'src>>), SmallErr> {
    let retained = matches!(strictness, Strictness::Strict);
    scan_root_with::<DialectScan, Recorder, CONTROLLED>(
        bytes,
        start,
        demands,
        strictness,
        max,
        dialect,
        &[],
        allow_stop,
        retained,
        control,
    )
}

#[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
fn scan_root_with<'src, S: Scan, R: RecordSink, const CONTROLLED: bool>(
    bytes: &'src [u8],
    start: usize,
    demands: &[Demand],
    strictness: Strictness,
    max: u32,
    dialect: Dialect,
    facts: &[Fact<'src>],
    allow_stop: bool,
    retain_glyphs: bool,
    control: &Control,
) -> Result<(Vec<Answer<'src>>, usize, bool, Vec<Fact<'src>>), SmallErr> {
    let mut walker =
        Walker::<S, CONTROLLED, R>::new(bytes, start, strictness, max, dialect, facts, retain_glyphs, control)?;
    walker.allow_stop = allow_stop;
    if walker.pos >= bytes.len() {
        return Err(error::expected_value(walker.pos));
    }
    walker.poll(walker.pos)?;
    let mut marks = alloc::vec![Answer::Missing; demands.len()];
    let hits = hits_for(demands);
    walker.walk_node(&hits, None, &mut marks)?;
    let facts = walker.rec.take_facts(bytes);
    Ok((marks, walker.pos, walker.stop, facts))
}

fn hits_for(demands: &[Demand]) -> Vec<Hit<'_>> {
    demands
        .iter()
        .enumerate()
        .map(|(idx, demand)| Hit {
            idx,
            view: View::from_demand(demand),
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Hit<'a> {
    idx: usize,
    view: View<'a>,
}

#[derive(Clone, Copy)]
enum View<'a> {
    Whole,
    /// A synthetic child of a `Whole` under the fused facts walk: descend the
    /// subtree to record its trivia, but answer no mark.
    Record,
    Path {
        steps: &'a [Step],
        nested: Option<&'a Demand>,
    },
    Collection {
        keys: Option<&'a [Name]>,
        nested: Option<&'a Demand>,
    },
    Slice {
        range: Range,
        nested: Option<&'a Demand>,
    },
    Project {
        path: &'a [Step],
        fields: &'a [Name],
    },
    Filter {
        path: &'a [Step],
        predicate: &'a Predicate,
        project: &'a [Name],
    },
    Oracle(&'a Oracle),
}

impl<'a> View<'a> {
    fn from_demand(demand: &'a Demand) -> Self {
        match demand {
            Demand::Whole => Self::Whole,
            Demand::Path { steps, nested } => Self::Path {
                steps,
                nested: nested.as_deref(),
            },
            Demand::Collection { fields, nested } => Self::Collection {
                keys: fields.as_deref(),
                nested: nested.as_deref(),
            },
            Demand::Slice { range, nested } => Self::Slice {
                range: *range,
                nested: nested.as_deref(),
            },
            Demand::Project { path, fields } => Self::Project {
                path: &path.steps,
                fields,
            },
            Demand::Filter {
                path,
                predicate,
                project,
            } => Self::Filter {
                path: &path.steps,
                predicate,
                project,
            },
            Demand::Oracle(o) => Self::Oracle(o),
        }
    }

    fn from_nested(nested: Option<&'a Demand>) -> Self {
        match nested {
            Some(d) => Self::from_demand(d),
            None => Self::Whole,
        }
    }
}

impl<'src, 'c, 'f, S: Scan, R: RecordSink, const CONTROLLED: bool> Walker<'src, 'c, 'f, S, CONTROLLED, R> {
    #[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
    fn new(
        bytes: &'src [u8],
        start: usize,
        strictness: Strictness,
        max: u32,
        dialect: Dialect,
        facts: &'f [Fact<'src>],
        retain_glyphs: bool,
        control: &'c Control,
    ) -> Result<Self, SmallErr> {
        let mut walker = Self {
            bytes,
            pos: start,
            strictness,
            max,
            depth: 0,
            dialect,
            facts,
            control,
            scratch: String::new(),
            members: Members::default(),
            member_pool: Vec::new(),
            row_layout: RowLayout::default(),
            repeat_depth: 0,
            rows: Vec::new(),
            allow_stop: false,
            stop: false,
            replaying: false,
            scan: core::marker::PhantomData,
            rec: R::new(retain_glyphs),
        };
        walker.pos = S::skip_trivia(bytes, start, dialect)?;
        Ok(walker)
    }

    /// The host control poll: container and record boundaries only, never per
    /// token. `CONTROLLED = false` folds this to `Ok(())` and drops the borrow.
    #[inline(always)]
    fn poll(&self, at: usize) -> Result<(), SmallErr> {
        if CONTROLLED {
            check_control(self.control, at)
        } else {
            Ok(())
        }
    }

    /// Skip trivia at `pos`, recording any comment spans. A replay re-walk
    /// records nothing: its spans are already recorded.
    #[inline(always)]
    fn trivia(&mut self) -> Result<(), SmallErr> {
        if R::RECORDING && !self.replaying {
            let start = self.pos;
            let rec = &mut self.rec;
            self.pos = lex::skip_trivia_spans(self.bytes, start, self.dialect, |begin, end| {
                rec.note_comment(begin, end);
            })?;
        } else {
            self.pos = S::skip_trivia(self.bytes, self.pos, self.dialect)?;
        }
        Ok(())
    }

    fn unread(&self) -> Check {
        match self.strictness {
            Strictness::Strict => Check::Values,
            Strictness::Structural | Strictness::Lazy => Check::Locate,
        }
    }

    fn demanded(&self) -> Check {
        match self.strictness {
            Strictness::Lazy => Check::Locate,
            Strictness::Structural | Strictness::Strict => Check::Values,
        }
    }

    fn is_fully_validated(&self) -> bool {
        matches!(self.strictness, Strictness::Strict)
    }

    /// A `Document` answer for `span`. Only facts whose glyph (or owner, when
    /// glyph-less) lies inside the span attach, keeping the clone proportional
    /// to the answer rather than the whole collector batch.
    fn document(&self, span: ByteRange) -> Document<'src> {
        let mut document = if self.is_fully_validated() {
            Document::from_span_validated(self.bytes, span, self.dialect.grammar_tag())
        } else {
            Document::from_span(self.bytes, span)
        };
        if !self.facts.is_empty() {
            let facts: Vec<Fact<'src>> = self
                .facts
                .iter()
                .filter(|fact| crate::encode::within(fact, span.start(), span.end()))
                .cloned()
                .collect();
            document
                .set_facts(facts)
                .expect("walk facts are authored in ascending glyph order");
        }
        document
    }

    /// Read an object key at `self.pos`, advancing past it. A key that needs
    /// decoding is decoded: key decode is demand matching, not a Lazy value
    /// check.
    #[inline]
    fn take_key(&mut self) -> Result<(Option<String>, Option<ByteRange>), SmallErr> {
        let (owned, inner, end) = read_key::<S>(self.bytes, self.pos, self.dialect)?;
        self.pos = end;
        Ok((owned, inner))
    }

    fn skip_unread(&mut self) -> Result<(), SmallErr> {
        self.pos = S::skip_value_at(self.bytes, self.pos, self.unread(), self.depth, self.max, self.dialect)?;
        Ok(())
    }

    fn enter_container(&mut self, at: usize) -> Result<(), SmallErr> {
        if self.depth >= self.max {
            return Err(error::limit(at));
        }
        self.depth += 1;
        Ok(())
    }

    /// Skip or walk one child of a container.
    /// A Whole mark value-checks unread siblings under Structural and Strict.
    /// A Whole mark never enters as a child hit.
    fn walk_or_skip_child(
        &mut self,
        child: &[Hit<'_>],
        hits: &[Hit<'_>],
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        if !child.is_empty() {
            // One container-consuming hit on an object takes the object path directly.
            if let [h] = child
                && container_child(h.view)
                && self.bytes.get(self.pos) == Some(&b'{')
            {
                return self.walk_object(self.pos, child, None, marks);
            }
            // A Record-only child of a scalar skips without value dispatch.
            if R::RECORDING
                && child.iter().all(|h| matches!(h.view, View::Record))
                && !matches!(self.bytes.get(self.pos), Some(b'[' | b'{'))
            {
                self.pos = skip_scalar::<S>(self.bytes, self.pos, self.demanded(), self.max, self.dialect)?;
                return Ok(());
            }
            return self.walk_node(child, None, marks);
        }
        let whole_demands_values = hits
            .iter()
            .any(|h| matches!(h.view, View::Whole) && !matches!(self.strictness, Strictness::Lazy));
        let check = if whole_demands_values {
            self.demanded()
        } else {
            self.unread()
        };
        self.pos = S::skip_value_at(self.bytes, self.pos, check, self.depth, self.max, self.dialect)?;
        Ok(())
    }

    fn walk_node(
        &mut self,
        hits: &[Hit<'_>],
        wanted: Option<&Wanted<'_>>,
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        self.pos = S::skip_trivia(self.bytes, self.pos, self.dialect)?;
        let start = self.pos;
        let Some(&byte) = self.bytes.get(start) else {
            return Err(error::expected_value(start));
        };

        let mut here_buf = [Hit {
            idx: 0,
            view: View::Whole,
        }; CHILD_HITS];
        let here_heap: Vec<Hit<'_>>;
        let here: &[Hit<'_>] = if hits.len() <= here_buf.len() {
            for (i, h) in hits.iter().enumerate() {
                here_buf[i] = flatten_here(*h);
            }
            &here_buf[..hits.len()]
        } else {
            here_heap = hits.iter().copied().map(flatten_here).collect();
            &here_heap
        };

        if here.is_empty() {
            return self.skip_unread();
        }

        // Facts need every container descended so the walk skips all trivia in
        // one pass; a root `Whole` is the exhaustive request that permits it.
        let record_all = R::RECORDING && !self.replaying && here.iter().any(|h| matches!(h.view, View::Whole));
        let need_enter = record_all
            || here.iter().any(|h| needs_container(h.view))
            || (byte == b'{' && here.iter().any(|h| matches!(h.view, View::Oracle(Oracle::Count))));
        let kind = match byte {
            b'n' => ValueKind::Null,
            b't' | b'f' => ValueKind::Bool,
            b'-' | b'0'..=b'9' => ValueKind::Number,
            b'+' | b'.' | b'I' | b'N' if self.dialect.json5() => ValueKind::Number,
            b'"' => ValueKind::String,
            b'\'' if self.dialect.json5() => ValueKind::String,
            b'[' => ValueKind::Array,
            b'{' => ValueKind::Object,
            _ => return Err(error::expected_value(start)),
        };

        for h in here {
            if let View::Oracle(o) = h.view {
                self.fill_oracle(h.idx, o, kind, start, need_enter, marks)?;
            }
        }

        if !need_enter && here.iter().all(|h| matches!(h.view, View::Oracle(_) | View::Whole)) {
            let check = if here.iter().any(|h| matches!(h.view, View::Whole)) {
                self.demanded()
            } else {
                self.unread()
            };
            // Count/DescendCount may have already Locate-walked. Reuse that end
            // only when this skip is also Locate; a co-demand Whole still has
            // to value-check.
            let end = if self.pos > start && check == self.unread() {
                self.pos
            } else {
                match kind {
                    ValueKind::Array | ValueKind::Object => {
                        S::skip_value_at(self.bytes, start, check, self.depth, self.max, self.dialect)?
                    }
                    _ => skip_scalar::<S>(self.bytes, start, check, self.max, self.dialect)?,
                }
            };
            let span = ByteRange::try_new(start, end).expect("ordered");
            for h in here {
                if matches!(h.view, View::Whole) {
                    assign_document(&mut marks[h.idx], self.document(span));
                }
            }
            self.pos = end;
            return Ok(());
        }

        match kind {
            // An array that is an element of a row loop answers the row law: a
            // projection/filter is one row (or nothing), a whole element keeps
            // the array's own span, and only a nested collection/slice maps its
            // inner elements. The decision is the row law in element position,
            // not the sibling hit mix. A container-count oracle is answered for
            // the array itself and must not turn a sibling projection into a map
            // of the array's inner elements; a count-only
            // hit stays on the `need_enter == false` path below.
            ValueKind::Array
                if self.repeat_depth > 0
                    && here.iter().any(|h| element_terminal(h.view))
                    && here.iter().all(|h| {
                        element_terminal(h.view) || matches!(h.view, View::Oracle(Oracle::Count | Oracle::DescendCount))
                    }) =>
            {
                let end = S::skip_value_at(self.bytes, start, self.demanded(), self.depth, self.max, self.dialect)?;
                for h in here {
                    if let View::Oracle(oracle) = h.view {
                        self.fill_oracle(h.idx, oracle, kind, start, false, marks)?;
                    }
                }
                self.finish_terminal(here, start, end, kind, marks);
                Ok(())
            }
            ValueKind::Array => self.walk_array(start, here, wanted, marks),
            ValueKind::Object => self.walk_object(start, here, wanted, marks),
            _ => {
                let check = self.demanded();
                let end = skip_scalar::<S>(self.bytes, start, check, self.max, self.dialect)?;
                self.finish_terminal(here, start, end, kind, marks);
                Ok(())
            }
        }
    }

    /// Fold a terminal element answer (a scalar, or an array/object the element
    /// law treats as one) into this node's marks.
    fn finish_terminal(
        &mut self,
        here: &[Hit<'_>],
        start: usize,
        end: usize,
        kind: ValueKind,
        marks: &mut [Answer<'src>],
    ) {
        let span = ByteRange::try_new(start, end).expect("ordered");
        for h in here {
            match h.view {
                View::Whole => assign_document(&mut marks[h.idx], self.document(span)),
                // A projection/filter element contributes no cells here. The
                // enclosing element loop's `fold_row` turns the empty element
                // batch into an absent row, so the batch must survive: it is the
                // element's answer, not a mismatch.
                _ if matches!(marks[h.idx], Answer::Columns(_)) => {}
                View::Oracle(_) | View::Record => {}
                _ => marks[h.idx] = Answer::TypeMismatch { actual: kind },
            }
        }
        self.pos = end;
    }

    fn fill_oracle(
        &mut self,
        idx: usize,
        oracle: &Oracle,
        kind: ValueKind,
        start: usize,
        entering: bool,
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        let check = self.demanded();
        match oracle {
            Oracle::Kind => {
                marks[idx] = Answer::Oracle(OracleAnswer::Kind(kind));
            }
            Oracle::Count => match kind {
                ValueKind::Array | ValueKind::Object => {
                    // An entering walk fills Count from the container it builds;
                    // counting here as well would walk the container twice.
                    if entering {
                        return Ok(());
                    }
                    // Counting demands structure, not the member/element values;
                    // value-checking them here would validate the whole subtree.
                    let count_check = self.unread();
                    let control = if CONTROLLED { Some(self.control) } else { None };
                    let (end, n) = count_container::<S>(
                        self.bytes,
                        start,
                        count_check,
                        self.depth,
                        self.max,
                        self.dialect,
                        control,
                    )?;
                    self.pos = end;
                    marks[idx] = Answer::Oracle(OracleAnswer::Count(n));
                }
                // A container count: a scalar has no child or member count, so it
                // is a type mismatch (this matches the oracle's contract and the
                // upstream "not string length" rule).
                _ => marks[idx] = Answer::TypeMismatch { actual: kind },
            },
            Oracle::DescendCount => {
                let (end, n) = S::skip_value_counting(self.bytes, start, check, self.max, self.dialect)?;
                self.pos = end;
                marks[idx] = Answer::Oracle(OracleAnswer::Count(n));
            }
            Oracle::StringByteLength => {
                if kind != ValueKind::String {
                    marks[idx] = Answer::TypeMismatch { actual: kind };
                    return Ok(());
                }
                self.scratch.clear();
                let _end = S::parse_string_into(self.bytes, start, &mut self.scratch, self.dialect)?;
                marks[idx] = Answer::Oracle(OracleAnswer::StringByteLength(self.scratch.len() as u64));
            }
            Oracle::HasKey { .. } | Oracle::MemberNames | Oracle::MemberCount => {
                // Filled in walk_object; arrays decline to type mismatch.
                if kind != ValueKind::Object {
                    marks[idx] = Answer::TypeMismatch { actual: kind };
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // single-pass element loop; splitting would obscure the mark fold
    fn walk_array(
        &mut self,
        start: usize,
        hits: &[Hit<'_>],
        wanted: Option<&Wanted<'_>>,
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        self.enter_container(start)?;
        if hits
            .iter()
            .all(|h| matches!(h.view, View::Project { path: [], .. } | View::Filter { path: [], .. }))
        {
            let r = self.keep_array(start, hits, wanted, marks);
            self.depth = self.depth.saturating_sub(1);
            return r;
        }
        self.pos = start + 1;
        let need_spans = hits.iter().any(|h| {
            matches!(
                h.view,
                View::Path { steps, .. } if matches!(steps.first(), Some(Step::Index(i)) if *i < 0)
            ) || matches!(
                h.view,
                View::Project { path, .. } | View::Filter { path, .. }
                    if matches!(path.first(), Some(Step::Index(i)) if *i < 0)
            ) || slice_replays(h.view)
        });
        // A window that does not need the array length can stop after its last
        // element. An enclosing element loop is a repetition, so a nested window
        // must finish its array to keep the outer walk in sync.
        //
        // Stopping unwinds the whole walk, so it is only sound when this array
        // answers every demand: one hit per mark means no sibling demand is
        // left unresolved past the stop point. `prefix_window` then requires
        // each of those hits to be a bounded `Slice`.
        let prefix = if self.allow_stop && self.repeat_depth == 0 && hits.len() == marks.len() {
            prefix_window(hits)
        } else {
            None
        };
        let mut elements: Vec<ByteRange> = Vec::new();
        let mut previous: Option<ByteRange> = None;
        // The row accumulators wait on the walker's stack while each element
        // writes its own answer, so an element answer cannot retract folded rows.
        let rows_base = self.open_rows(hits, marks);
        let bytes = self.bytes;
        let mut idx = 0usize;
        loop {
            if self.stop {
                break;
            }
            if let Some(need) = prefix
                && idx >= need
            {
                self.stop = true;
                break;
            }
            self.poll(self.pos)?;
            self.rec.set_previous(previous);
            self.trivia()?;
            let Some(&byte) = self.bytes.get(self.pos) else {
                self.depth = self.depth.saturating_sub(1);
                return Err(error::expected_comma_array(self.pos));
            };
            if byte == b']' {
                self.pos += 1;
                self.rec.trailer(ByteRange::try_new(start, self.pos).expect("ordered"));
                break;
            }
            if idx > 0 {
                if byte != b',' {
                    self.depth = self.depth.saturating_sub(1);
                    return Err(error::expected_comma_array(self.pos));
                }
                self.pos += 1;
                self.trivia()?;
                if self.bytes.get(self.pos) == Some(&b']') {
                    if self.dialect.trailing_commas() {
                        self.pos += 1;
                        self.rec.trailer(ByteRange::try_new(start, self.pos).expect("ordered"));
                        break;
                    }
                    self.depth = self.depth.saturating_sub(1);
                    return Err(error::trailing_comma(self.pos));
                }
            }
            let elem_start = self.pos;
            let mut child_buf = [Hit {
                idx: 0,
                view: View::Whole,
            }; CHILD_HITS];
            let child_heap: Vec<Hit<'_>>;
            let child: &[Hit<'_>] = if hits.len() <= CHILD_HITS {
                let mut n = 0;
                for h in hits {
                    if let Some(c) = enter_array::<R>(*h, idx) {
                        child_buf[n] = c;
                        n += 1;
                    }
                }
                &child_buf[..n]
            } else {
                child_heap = hits.iter().copied().filter_map(|h| enter_array::<R>(h, idx)).collect();
                &child_heap
            };
            let gap = self.rec.begin_gap();
            self.repeat_depth += 1;
            let walked = self.walk_or_skip_child(child, hits, marks);
            self.repeat_depth -= 1;
            walked?;
            let elem_end = self.pos;
            let span = ByteRange::try_new(elem_start, elem_end).expect("ordered");
            self.rec.fill_next(gap, span);
            if need_spans {
                elements.push(span);
            }
            self.fold_row(rows_base, hits, marks, bytes, span);
            previous = Some(span);
            idx += 1;
        }
        self.depth = self.depth.saturating_sub(1);
        self.close_rows(rows_base, hits, marks);
        let end = self.pos;
        let span = ByteRange::try_new(start, end).expect("ordered");
        self.finish_array(span, &elements, idx, hits, marks)
    }

    /// One element of a `keep_array`/`walk_element_run` loop: an object goes
    /// through `keep_object`; anything else is skipped and folded to an absent
    /// row for a projection.
    fn walk_kept_element(
        &mut self,
        wanted: &Wanted<'_>,
        hits: &[Hit<'_>],
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        if self.bytes.get(self.pos) == Some(&b'{') {
            self.keep_object(wanted, hits, marks)
        } else {
            self.pos = S::skip_value_at(self.bytes, self.pos, self.unread(), self.depth, self.max, self.dialect)?;
            for h in hits {
                if let Some(RowShape::Projected(fields)) = row_shape(h.view) {
                    push_absent_row(&mut marks[h.idx], fields, self.bytes);
                }
            }
            Ok(())
        }
    }

    /// Array of objects whose hits are already Project/Filter at this level:
    /// one member loop per object, only demanded keys stored.
    fn keep_array(
        &mut self,
        start: usize,
        hits: &[Hit<'_>],
        wanted_override: Option<&Wanted<'_>>,
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        // Caller already entered the array container.
        self.pos = start + 1;
        // The retained shape is keyed by demanded member, so it is only valid for
        // one array's `Wanted`; a sibling array with a different field set must
        // start cold or it reuses a stale slot (a wanted key read as absent).
        self.row_layout.reset();
        let gathered;
        let wanted = if let Some(wanted) = wanted_override {
            wanted
        } else {
            gathered = Wanted::gather(hits);
            &gathered
        };
        // This loop's element walk is `keep_object`, whose only write is
        // `apply_object_row`: it appends, so it can never retract the
        // accumulator. The accumulator therefore stays in `marks` and rows are
        // appended in place (no per-element batch copy); a non-object element is
        // folded to an absent row for a projection and nothing for a filter.
        let mut first = true;
        loop {
            self.poll(self.pos)?;
            self.pos = S::skip_trivia(self.bytes, self.pos, self.dialect)?;
            let Some(&byte) = self.bytes.get(self.pos) else {
                return Err(error::expected_comma_array(self.pos));
            };
            if byte == b']' {
                self.pos += 1;
                break;
            }
            if !first {
                if byte != b',' {
                    return Err(error::expected_comma_array(self.pos));
                }
                self.pos += 1;
                self.pos = S::skip_trivia(self.bytes, self.pos, self.dialect)?;
                if self.bytes.get(self.pos) == Some(&b']') {
                    if self.dialect.trailing_commas() {
                        self.pos += 1;
                        break;
                    }
                    return Err(error::trailing_comma(self.pos));
                }
            }
            first = false;
            self.walk_kept_element(wanted, hits, marks)?;
        }
        self.fill_missing_row_marks(hits, marks);
        Ok(())
    }

    /// Every row view that matched nothing answers the empty batch, the way the
    /// serial fold does; a `Missing` mark would otherwise leak out of a run.
    fn fill_missing_row_marks(&mut self, hits: &[Hit<'_>], marks: &mut [Answer<'src>]) {
        for h in hits {
            if matches!(marks[h.idx], Answer::Missing) {
                let fields = match h.view {
                    View::Project { fields, .. } => fields.to_vec(),
                    View::Filter { project, .. } if !project.is_empty() => project.to_vec(),
                    _ => alloc::vec![alloc::string::String::from("$")],
                };
                marks[h.idx] = Answer::Columns(Columns::new(self.bytes, fields));
            }
        }
    }

    /// [`Self::keep_array`]'s element loop over a run with no surrounding
    /// brackets: successive values accumulated into one row batch.
    fn walk_element_run(
        &mut self,
        range: ByteRange,
        hits: &[Hit<'_>],
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        self.pos = range.start();
        // One run, one demanded key set: see [`keep_array`].
        self.row_layout.reset();
        let end = range.end().min(self.bytes.len());
        let wanted = Wanted::gather(hits);
        // Same direct append as [`Self::keep_array`]: the element walk is
        // `keep_object`, which only appends.
        let bytes = self.bytes;
        let mut first = true;
        loop {
            self.poll(self.pos)?;
            if !next_run_value::<S>(bytes, &mut self.pos, end, self.dialect, first)? {
                break;
            }
            first = false;
            self.walk_kept_element(&wanted, hits, marks)?;
        }
        self.fill_missing_row_marks(hits, marks);
        Ok(())
    }

    /// The array Index-spine answer for a `Project`/`Filter` whose first step is
    /// an `Index`: replay the selected element in element position, or answer the
    /// flat one-row law.
    #[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
    fn finish_index_spine<'v>(
        &mut self,
        path: &'v [Step],
        child: impl Fn(&'v [Step]) -> View<'v>,
        len: usize,
        elements: &[ByteRange],
        idx: usize,
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        match path.first() {
            Some(Step::Index(i)) => match structury::resolve_index(len, *i) {
                Some(at) if path.len() == 1 => {
                    // Flat projection on one selected element: one row. A
                    // recorded span (a negative index) is replayed in element
                    // position; the rest answers an absent row.
                    if at < elements.len() {
                        let depth = core::mem::replace(&mut self.repeat_depth, 1);
                        let replayed = replay_element(
                            self,
                            elements[at],
                            Hit {
                                idx,
                                view: child(&path[1..]),
                            },
                            marks,
                        );
                        self.repeat_depth = depth;
                        replayed?;
                    }
                    if !matches!(marks[idx], Answer::Columns(_)) {
                        marks[idx] = flat_index_row(self.bytes, child(&[]));
                    }
                }
                Some(at) if at < elements.len() => {
                    replay_element(
                        self,
                        elements[at],
                        Hit {
                            idx,
                            view: child(&path[1..]),
                        },
                        marks,
                    )?;
                }
                _ => marks[idx] = Answer::Missing,
            },
            _ => {
                marks[idx] = Answer::TypeMismatch {
                    actual: ValueKind::Array,
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // single-pass container finish; splitting would obscure the mark fold
    fn finish_array(
        &mut self,
        span: ByteRange,
        elements: &[ByteRange],
        len: usize,
        hits: &[Hit<'_>],
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        for h in hits {
            match h.view {
                View::Whole => assign_document(&mut marks[h.idx], self.document(span)),
                View::Path { steps, nested } => {
                    if !matches!(marks[h.idx], Answer::Missing) {
                        continue;
                    }
                    match steps.first() {
                        Some(Step::Index(i)) => match structury::resolve_index(len, *i) {
                            Some(at) if at < elements.len() => {
                                let nested_view = if steps.len() == 1 {
                                    View::from_nested(nested)
                                } else {
                                    View::Path {
                                        steps: &steps[1..],
                                        nested,
                                    }
                                };
                                replay_element(
                                    self,
                                    elements[at],
                                    Hit {
                                        idx: h.idx,
                                        view: nested_view,
                                    },
                                    marks,
                                )?;
                            }
                            _ => marks[h.idx] = Answer::Missing,
                        },
                        Some(Step::Key(_)) => {
                            marks[h.idx] = Answer::TypeMismatch {
                                actual: ValueKind::Array,
                            }
                        }
                        None => {}
                    }
                }
                View::Collection { .. } => {
                    if matches!(marks[h.idx], Answer::Columns(_)) {
                        continue;
                    }
                    marks[h.idx] =
                        Answer::Columns(Columns::new(self.bytes, alloc::vec![alloc::string::String::from("$")]));
                }
                View::Slice { range, nested } => {
                    if slice_replays(h.view) {
                        // The accumulated batch is not one row per element, so
                        // the window must select **elements**, then apply the
                        // nested demand. Replay the windowed spans through the
                        // same row law the element loop uses, so every shape
                        // folds identically.
                        let window = range.window(len);
                        let view = View::from_nested(nested);
                        // A bounded window stops the array loop, and the replay
                        // walks the element in element position (an array element
                        // is one value, not a map): mirror the main loop's latches.
                        let stopped = core::mem::replace(&mut self.stop, false);
                        let repeat = core::mem::replace(&mut self.repeat_depth, 1);
                        let replay = [Hit { idx: 0, view: h.view }];
                        let mut acc = [Answer::Missing];
                        let base = self.open_rows(&replay, &mut acc);
                        for &span in elements.iter().take(window.end).skip(window.start) {
                            replay_element(self, span, Hit { idx: 0, view }, &mut acc)?;
                            self.fold_row(base, &replay, &mut acc, self.bytes, span);
                        }
                        self.close_rows(base, &replay, &mut acc);
                        self.repeat_depth = repeat;
                        self.stop = stopped;
                        marks[h.idx] = core::mem::replace(&mut acc[0], Answer::Missing);
                    } else if let Answer::Columns(batch) = &mut marks[h.idx] {
                        batch.retain_rows(range.window(len));
                    } else {
                        marks[h.idx] =
                            Answer::Columns(Columns::new(self.bytes, alloc::vec![alloc::string::String::from("$")]));
                    }
                }
                View::Project { path: [], fields } => {
                    if matches!(marks[h.idx], Answer::Columns(_)) {
                        continue;
                    }
                    marks[h.idx] = Answer::Columns(Columns::new(self.bytes, fields.to_vec()));
                }
                View::Filter { path: [], project, .. } => {
                    if matches!(marks[h.idx], Answer::Columns(_)) {
                        continue;
                    }
                    let fields = if project.is_empty() {
                        alloc::vec![alloc::string::String::from("$")]
                    } else {
                        project.to_vec()
                    };
                    marks[h.idx] = Answer::Columns(Columns::new(self.bytes, fields));
                }
                // The element loop answers a positive `Step::Index` directly
                // (`need_spans` only records negative ones), so an already
                // answered mark must not be overwritten here. A selected element
                // the element loop could not answer (a non-object under a flat
                // projection) answers the one-row law instead.
                View::Project { path, fields }
                    if !path.is_empty() && matches!(marks[h.idx], Answer::Missing | Answer::TypeMismatch { .. }) =>
                {
                    self.finish_index_spine(path, |p| View::Project { path: p, fields }, len, elements, h.idx, marks)?;
                }
                View::Filter {
                    path,
                    predicate,
                    project,
                } if !path.is_empty() && matches!(marks[h.idx], Answer::Missing | Answer::TypeMismatch { .. }) => {
                    self.finish_index_spine(
                        path,
                        |p| View::Filter {
                            path: p,
                            predicate,
                            project,
                        },
                        len,
                        elements,
                        h.idx,
                        marks,
                    )?;
                }
                View::Oracle(Oracle::Count) => {
                    marks[h.idx] = Answer::Oracle(OracleAnswer::Count(len as u64));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn walk_object(
        &mut self,
        start: usize,
        hits: &[Hit<'_>],
        wanted: Option<&Wanted<'_>>,
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        let collect_all = hits.iter().any(|h| {
            matches!(
                h.view,
                View::Oracle(Oracle::HasKey { .. } | Oracle::MemberNames | Oracle::MemberCount | Oracle::Count)
            )
        });
        // A caller that already gathered the demanded key set and whose hits are
        // all flat rows is the repeated-object shape a row loop serves: the
        // retained row layout skips key decoding and demand matching on every
        // object after the first. A stream record is exactly that shape, one
        // object at a time, so its records share the array lane's fast path.
        if !collect_all
            && !R::RECORDING
            && let Some(wanted) = wanted
            && hits
                .iter()
                .all(|h| matches!(h.view, View::Project { path: [], .. } | View::Filter { path: [], .. }))
        {
            return self.keep_object(wanted, hits, marks);
        }
        self.enter_container(start)?;
        self.pos = start + 1;
        // Oracles need every member; a Project/Filter needs only its fields.
        // One pooled `Members` per nesting level, reused across rows.
        let indexed = hits
            .iter()
            .any(|h| matches!(h.view, View::Path { .. } | View::Project { .. } | View::Filter { .. }));
        let slot = self.depth as usize;
        // A Spine-only object builds no member map. Its miss arm is `Missing`.
        if !needs_member_map(collect_all, hits) {
            return self.walk_object_members(start, false, false, None, hits, Some(&Wanted::NONE), marks);
        }
        if self.member_pool.len() <= slot {
            self.member_pool.resize_with(slot + 1, Members::default);
        }
        let mut members = core::mem::take(&mut self.member_pool[slot]);
        members.clear();
        if !indexed {
            members.index = None;
        }
        let outcome = self.walk_object_members(start, collect_all, indexed, Some(&mut members), hits, wanted, marks);
        self.member_pool[slot] = members;
        outcome
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the walk's inputs are all independent; one member loop shares its retained-shape locals"
    )]
    fn walk_object_members(
        &mut self,
        start: usize,
        collect_all: bool,
        indexed: bool,
        mut members: Option<&mut Members>,
        hits: &[Hit<'_>],
        wanted_override: Option<&Wanted<'_>>,
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        let gathered;
        let wanted = if let Some(wanted) = wanted_override {
            wanted
        } else {
            gathered = Wanted::gather(hits);
            &gathered
        };
        let mut seen_member = false;
        let mut previous: Option<ByteRange> = None;
        loop {
            if self.stop {
                break;
            }
            self.poll(self.pos)?;
            self.rec.set_previous(previous);
            self.trivia()?;
            let Some(&byte) = self.bytes.get(self.pos) else {
                return Err(error::expected_key(self.pos));
            };
            if byte == b'}' {
                self.pos += 1;
                self.rec.trailer(ByteRange::try_new(start, self.pos).expect("ordered"));
                break;
            }
            if seen_member {
                if byte != b',' {
                    return Err(error::expected_comma_object(self.pos));
                }
                self.pos += 1;
                self.trivia()?;
                if self.bytes.get(self.pos) == Some(&b'}') {
                    if self.dialect.trailing_commas() {
                        self.pos += 1;
                        self.rec.trailer(ByteRange::try_new(start, self.pos).expect("ordered"));
                        break;
                    }
                    return Err(error::trailing_comma(self.pos));
                }
            }
            // A Record-only descent answers no keyed demand. Skip the key token without decoding it.
            let skip_key = R::RECORDING && !collect_all && wanted.is_empty() && !wants_key(hits);
            let owned_key: Option<String>;
            let key_inner: Option<ByteRange>;
            if skip_key {
                // Check the skipped key at the same dial as its sibling values.
                self.pos = S::skip_key(self.bytes, self.pos, self.demanded(), self.dialect)?;
                owned_key = None;
                key_inner = None;
            } else {
                let (owned, inner) = self.take_key()?;
                owned_key = owned;
                key_inner = inner;
            }
            let key_bytes: &[u8] = match (&owned_key, key_inner) {
                (_, Some(span)) => self.bytes.get(span.start()..span.end()).unwrap_or(&[]),
                (Some(s), None) => s.as_bytes(),
                (None, None) => &[],
            };
            self.trivia()?;
            if self.bytes.get(self.pos) != Some(&b':') {
                return Err(error::expected_colon(self.pos));
            }
            self.pos += 1;
            self.trivia()?;
            let val_start = self.pos;
            let mut child_buf = [Hit {
                idx: 0,
                view: View::Whole,
            }; CHILD_HITS];
            let child_heap: Vec<Hit<'_>>;
            let child: &[Hit<'_>] = if hits.len() <= CHILD_HITS {
                let mut n = 0;
                for h in hits {
                    if let Some(c) = enter_object_bytes::<R>(*h, key_bytes) {
                        child_buf[n] = c;
                        n += 1;
                    }
                }
                &child_buf[..n]
            } else {
                child_heap = hits
                    .iter()
                    .copied()
                    .filter_map(|h| enter_object_bytes::<R>(h, key_bytes))
                    .collect();
                &child_heap
            };
            // Each matching member replaces the located answer, including a
            // missing descendant. An empty batch is the seeded row buffer the
            // element/stream fold reads, not a located answer, so it survives.
            for hit in child {
                if !matches!(hit.view, View::Record)
                    && !matches!(&marks[hit.idx], Answer::Columns(columns) if columns.rows() == 0)
                {
                    marks[hit.idx] = Answer::Missing;
                }
            }
            let gap = self.rec.begin_gap();
            // A member value is a located node, not an element of any enclosing
            // element loop: zero the element depth so a spine that lands on an
            // array maps it, and a nested element loop counts from zero.
            let element_depth = core::mem::replace(&mut self.repeat_depth, 0);
            let walked = self.walk_or_skip_child(child, hits, marks);
            self.repeat_depth = element_depth;
            walked?;
            let val_end = self.pos;
            let span = ByteRange::try_new(val_start, val_end).expect("ordered");
            self.rec.fill_next(gap, span);
            seen_member = true;
            previous = Some(span);
            if let Some(members) = members.as_deref_mut()
                && (collect_all || wanted.contains(key_bytes, hits))
            {
                let name = member_name(key_inner, key_bytes);
                let _ = members.insert(self.bytes, name, span, indexed);
            }
        }
        self.depth = self.depth.saturating_sub(1);
        let span = ByteRange::try_new(start, self.pos).expect("ordered");
        self.finish_object(span, members.as_deref(), hits, wanted, marks)
    }

    /// The object key-spine answer for a `Project`/`Filter` whose first step is a
    /// `Key`: replay the member value, or answer `Missing`/`TypeMismatch`.
    fn finish_object_key_spine<'v>(
        &mut self,
        path: &'v [Step],
        child: impl Fn(&'v [Step]) -> View<'v>,
        members: Option<&Members>,
        idx: usize,
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        if !matches!(marks[idx], Answer::Missing) {
            return Ok(());
        }
        if let Some(Step::Key(name)) = path.first() {
            if let Some(rng) = members.and_then(|m| m.get(self.bytes, name.as_bytes())) {
                replay_element(
                    self,
                    rng,
                    Hit {
                        idx,
                        view: child(&path[1..]),
                    },
                    marks,
                )?;
            } else {
                marks[idx] = Answer::Missing;
            }
        } else {
            marks[idx] = Answer::TypeMismatch {
                actual: ValueKind::Object,
            };
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // single-pass container finish; splitting would obscure the mark fold
    fn finish_object(
        &mut self,
        span: ByteRange,
        members: Option<&Members>,
        hits: &[Hit<'_>],
        wanted: &Wanted<'_>,
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        for h in hits {
            match h.view {
                View::Whole => assign_document(&mut marks[h.idx], self.document(span)),
                View::Path { steps, nested } => {
                    self.finish_object_key_spine(steps, |p| View::Path { steps: p, nested }, members, h.idx, marks)?;
                }
                View::Oracle(Oracle::HasKey { key }) => {
                    marks[h.idx] = Answer::Oracle(OracleAnswer::HasKey(
                        members.and_then(|m| m.get(self.bytes, key.as_bytes())).is_some(),
                    ));
                }
                View::Oracle(Oracle::MemberNames) => {
                    marks[h.idx] = Answer::Oracle(OracleAnswer::MemberNames(
                        members
                            .map(|m| {
                                m.keys(self.bytes)
                                    .map(|k| String::from_utf8_lossy(k).into_owned())
                                    .collect()
                            })
                            .unwrap_or_default(),
                    ));
                }
                View::Oracle(Oracle::MemberCount | Oracle::Count) => {
                    marks[h.idx] = Answer::Oracle(OracleAnswer::Count(members.map_or(0, |m| m.len() as u64)));
                }
                View::Collection { .. } | View::Slice { .. } => {
                    marks[h.idx] = Answer::TypeMismatch {
                        actual: ValueKind::Object,
                    };
                }
                View::Project { path: [], .. } | View::Filter { path: [], .. } => {
                    // A flat row consumer is the one hit `needs_member_map`
                    // admits, so `members` is present here; the empty fallback
                    // keeps the shared row law's signature.
                    let empty = Members::default();
                    apply_object_row(
                        &mut marks[h.idx],
                        h.view,
                        wanted.resolved(h.idx),
                        members.unwrap_or(&empty),
                        self.bytes,
                        span,
                        self.dialect,
                        &mut self.scratch,
                    );
                }
                View::Project { path, fields } => {
                    self.finish_object_key_spine(path, |p| View::Project { path: p, fields }, members, h.idx, marks)?;
                }
                View::Filter {
                    path,
                    predicate,
                    project,
                } => {
                    self.finish_object_key_spine(
                        path,
                        |p| View::Filter {
                            path: p,
                            predicate,
                            project,
                        },
                        members,
                        h.idx,
                        marks,
                    )?;
                }
                View::Oracle(_) | View::Record => {}
            }
        }
        Ok(())
    }
}

fn flatten_here(mut h: Hit<'_>) -> Hit<'_> {
    // A spent `Path{[]}` is its nested demand; unwrap to
    // a fixed point, since the nested demand may itself be a spent spine.
    while let View::Path { steps: [], nested } = h.view {
        h = Hit {
            idx: h.idx,
            view: View::from_nested(nested),
        };
    }
    h
}

/// Enter an array element for `h`, or `None` when `h` does not reach `index`.
/// Under the fused facts walk a `Whole` becomes a [`View::Record`] child so the
/// element is descended for recording; otherwise `Whole` stays out.
fn enter_array<R: RecordSink>(h: Hit<'_>, index: usize) -> Option<Hit<'_>> {
    if R::RECORDING && matches!(h.view, View::Whole) {
        return Some(Hit {
            idx: h.idx,
            view: View::Record,
        });
    }
    match h.view {
        View::Record => Some(h),
        View::Collection { keys, nested } => match (keys, nested) {
            (_, Some(d)) => Some(Hit {
                idx: h.idx,
                view: View::from_demand(d),
            }),
            (Some(k), None) => Some(Hit {
                idx: h.idx,
                view: View::Project { path: &[], fields: k },
            }),
            (None, None) => None,
        },
        View::Slice { nested, .. } => Some(Hit {
            idx: h.idx,
            view: View::from_nested(nested),
        }),
        View::Project { path: [], fields } => Some(Hit {
            idx: h.idx,
            view: View::Project { path: &[], fields },
        }),
        View::Filter {
            path: [],
            predicate,
            project,
        } => Some(Hit {
            idx: h.idx,
            view: View::Filter {
                path: &[],
                predicate,
                project,
            },
        }),
        View::Path { steps, nested } => match steps.first() {
            Some(Step::Index(i)) if *i >= 0 && usize::try_from(*i).ok() == Some(index) => Some(Hit {
                idx: h.idx,
                view: if steps.len() == 1 {
                    View::from_nested(nested)
                } else {
                    View::Path {
                        steps: &steps[1..],
                        nested,
                    }
                },
            }),
            _ => None,
        },
        View::Project { path, fields } => match path.first() {
            Some(Step::Index(i)) if *i >= 0 && usize::try_from(*i).ok() == Some(index) => Some(Hit {
                idx: h.idx,
                view: View::Project {
                    path: &path[1..],
                    fields,
                },
            }),
            _ => None,
        },
        View::Filter {
            path,
            predicate,
            project,
        } => match path.first() {
            Some(Step::Index(i)) if *i >= 0 && usize::try_from(*i).ok() == Some(index) => Some(Hit {
                idx: h.idx,
                view: View::Filter {
                    path: &path[1..],
                    predicate,
                    project,
                },
            }),
            _ => None,
        },
        _ => None,
    }
}

/// Whether an object's hits need the pooled member map at all: only a hit that
/// consumes the object as a row (a flat `Project`/`Filter`, an oracle, the facts
/// recorder). Descending hits answer from the child walk and miss as `Missing`.
fn needs_member_map(collect_all: bool, hits: &[Hit<'_>]) -> bool {
    collect_all
        || hits.iter().any(|h| {
            matches!(
                h.view,
                View::Project { path: [], .. } | View::Filter { path: [], .. } | View::Oracle(_)
            )
        })
}

/// [`enter_array`] for object members: under the fused facts walk every member
/// value is descended for recording.
#[inline]
fn enter_object_bytes<'a, R: RecordSink>(h: Hit<'a>, key: &[u8]) -> Option<Hit<'a>> {
    if R::RECORDING && matches!(h.view, View::Whole) {
        return Some(Hit {
            idx: h.idx,
            view: View::Record,
        });
    }
    match h.view {
        View::Record => Some(h),
        View::Path { steps, nested } => match steps.first() {
            Some(Step::Key(name)) if name.as_bytes() == key => Some(Hit {
                idx: h.idx,
                view: if steps.len() == 1 {
                    View::from_nested(nested)
                } else {
                    View::Path {
                        steps: &steps[1..],
                        nested,
                    }
                },
            }),
            _ => None,
        },
        View::Project { path, fields } => match path.first() {
            Some(Step::Key(name)) if name.as_bytes() == key => Some(Hit {
                idx: h.idx,
                view: View::Project {
                    path: &path[1..],
                    fields,
                },
            }),
            _ => None,
        },
        View::Filter {
            path,
            predicate,
            project,
        } => match path.first() {
            Some(Step::Key(name)) if name.as_bytes() == key => Some(Hit {
                idx: h.idx,
                view: View::Filter {
                    path: &path[1..],
                    predicate,
                    project,
                },
            }),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a single hit at a container is answered by descending into the
/// container's own walk (`walk_object`/`walk_array`), with no `walk_node`
/// scalar/oracle arm to run first. `Path{}` is excluded because it still has to
/// be flattened; `Whole`, oracles and `Record` are excluded because
/// `walk_node` answers them without entering.
fn container_child(view: View<'_>) -> bool {
    match view {
        View::Path { steps, .. } => !steps.is_empty(),
        View::Project { .. } | View::Filter { .. } | View::Collection { .. } | View::Slice { .. } => true,
        View::Whole | View::Oracle(_) | View::Record => false,
    }
}

fn needs_container(view: View<'_>) -> bool {
    match view {
        View::Path { steps, .. } => !steps.is_empty(),
        View::Collection { .. }
        | View::Slice { .. }
        | View::Project { .. }
        | View::Filter { .. }
        | View::Oracle(Oracle::HasKey { .. } | Oracle::MemberNames | Oracle::MemberCount)
        // Record always descends: recording needs every nested gap.
        | View::Record => true,
        // Whole is skip_value of the entire node; DescendCount is skip_value_counting.
        View::Whole | View::Oracle(_) => false,
    }
}

/// Whether any hit at this object matches a member key: a `Path`/`Project`/
/// `Filter` whose next step is a key. This is the one key-want decision the
/// member loop, the fused key skip, and [`Wanted`] agree on.
fn wants_key(hits: &[Hit<'_>]) -> bool {
    hits.iter().any(|h| {
        matches!(
            h.view,
            View::Path { steps, .. } | View::Project { path: steps, .. } | View::Filter { path: steps, .. }
                if matches!(steps.first(), Some(Step::Key(_)))
        )
    })
}

/// Whether a view at an array-element position answers from the array's own
/// span instead of mapping its inner elements. This is the row law in element
/// position: a nested collection/slice descends (so a count oracle is `false`
/// here and is admitted beside a terminal hit by the element-loop's
/// count-oracle test). A `Record` descends so the fused facts walk still
/// records inside.
fn element_terminal(view: View<'_>) -> bool {
    !matches!(
        view,
        View::Collection { .. }
            | View::Slice { .. }
            | View::Record
            | View::Oracle(Oracle::Count | Oracle::DescendCount)
    )
}

/// Skip trivia at `pos` only when the byte there can start trivia. The RFC
/// tokenizer's [`Scan::trivia_starts`] is false for every token byte, and the
/// dialect gate is whitespace plus `/`, so a dense member step avoids the
/// whitespace scan.
#[allow(clippy::inline_always)] // hot member-loop path: forced inline is intentional
#[inline(always)]
fn skip_trivia_here<S: Scan>(bytes: &[u8], pos: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    match bytes.get(pos) {
        Some(&byte) if !S::trivia_starts(byte) => Ok(pos),
        _ => S::skip_trivia(bytes, pos, dialect),
    }
}

#[inline(always)]
fn skip_scalar<S: Scan>(
    bytes: &[u8],
    start: usize,
    check: Check,
    max: u32,
    dialect: Dialect,
) -> Result<usize, SmallErr> {
    match bytes.get(start) {
        Some(b'"') => S::skip_string(bytes, start, check, dialect),
        Some(b'\'') if dialect.json5() => S::skip_string(bytes, start, check, dialect),
        _ => S::skip_present_at(bytes, start, check, 0, max, dialect),
    }
}

/// Element count of an array at `start`. An object never reaches here: a
/// `Count` oracle at `{` always sets `need_enter` in `walk_node`, so
/// `fill_oracle` answers from the object walk instead.
#[inline(always)]
fn count_container<S: Scan>(
    bytes: &[u8],
    start: usize,
    check: Check,
    depth: u32,
    max: u32,
    dialect: Dialect,
    control: Option<&Control>,
) -> Result<(usize, u64), SmallErr> {
    if depth >= max {
        return Err(error::limit(start));
    }
    let child_depth = depth + 1;
    let mut pos = start + 1;
    let mut n = 0u64;
    loop {
        pos = S::skip_trivia(bytes, pos, dialect)?;
        let Some(&byte) = bytes.get(pos) else {
            return Err(error::expected_comma_array(pos));
        };
        if byte == b']' {
            return Ok((pos + 1, n));
        }
        if n > 0 {
            if byte != b',' {
                return Err(error::expected_comma_array(pos));
            }
            pos += 1;
            pos = S::skip_trivia(bytes, pos, dialect)?;
            if bytes.get(pos) == Some(&b']') {
                if dialect.trailing_commas() {
                    return Ok((pos + 1, n));
                }
                return Err(error::trailing_comma(pos));
            }
        }
        if let Some(control) = control {
            check_control(control, pos)?;
        }
        pos = S::skip_value_at(bytes, pos, check, child_depth, max, dialect)?;
        n += 1;
    }
}

fn span_eq_value(got: &[u8], expect: &Value, dialect: Dialect, scratch: &mut String) -> bool {
    match expect {
        Value::Bool(true) => got == b"true",
        Value::Bool(false) => got == b"false",
        Value::Null => got == b"null",
        Value::Number(n) => {
            let Ok(text) = core::str::from_utf8(got) else {
                return false;
            };
            parse_src_number(text, dialect, scratch).is_some_and(|g| g.numeric_eq(n))
        }
        Value::Str(s) => string_span_eq(got, s, dialect),
        _ => false,
    }
}

fn string_span_eq(got: &[u8], expect: &str, dialect: Dialect) -> bool {
    if let Some((inner, _)) = plain_double_quoted(got, 0) {
        return inner == expect.as_bytes();
    }
    let mut decoded = String::new();
    if crate::lex::parse_string_into(got, 0, &mut decoded, dialect).is_err() {
        return false;
    }
    decoded == expect
}

/// A predicate with each ordering leaf's operand resolved once. Built from a
/// [`Predicate`] when a row loop gathers its demanded keys, so the per-row walk
/// never re-parses the operand's spelling.
enum ResolvedPredicate<'a> {
    And(Box<ResolvedPredicate<'a>>, Box<ResolvedPredicate<'a>>),
    Or(Box<ResolvedPredicate<'a>>, Box<ResolvedPredicate<'a>>),
    Not(Box<ResolvedPredicate<'a>>),
    Eq { field: &'a Name, value: &'a Value },
    Ne { field: &'a Name, value: &'a Value },
    Order { field: &'a Name, cmp: Cmp, want: Want<'a> },
}

impl<'a> ResolvedPredicate<'a> {
    fn resolve(predicate: &'a Predicate) -> Self {
        match predicate {
            Predicate::And(a, b) => Self::And(Box::new(Self::resolve(a)), Box::new(Self::resolve(b))),
            Predicate::Or(a, b) => Self::Or(Box::new(Self::resolve(a)), Box::new(Self::resolve(b))),
            Predicate::Not(p) => Self::Not(Box::new(Self::resolve(p))),
            Predicate::Eq { field, value } => Self::Eq { field, value },
            Predicate::Ne { field, value } => Self::Ne { field, value },
            Predicate::Gt { field, value } => Self::Order {
                field,
                cmp: Cmp::Gt,
                want: Want::resolve(value),
            },
            Predicate::Lt { field, value } => Self::Order {
                field,
                cmp: Cmp::Lt,
                want: Want::resolve(value),
            },
            Predicate::Ge { field, value } => Self::Order {
                field,
                cmp: Cmp::Ge,
                want: Want::resolve(value),
            },
            Predicate::Le { field, value } => Self::Order {
                field,
                cmp: Cmp::Le,
                want: Want::resolve(value),
            },
        }
    }
}

/// An ordering operand with its `i128` fast form precomputed. The decimal and
/// JSON5 paths still need the original value, so it is borrowed alongside.
enum Want<'a> {
    Number {
        value: &'a structury::Number,
        /// [`plain_int`] of `value`; `None` sends the compare to `numeric_cmp`.
        int: Option<i128>,
    },
    Bool {
        flag: bool,
        int: i128,
    },
    /// A value the ordering arms never match (`null`, string, array, object).
    Other,
}

impl<'a> Want<'a> {
    fn resolve(value: &'a Value) -> Self {
        match value {
            Value::Number(number) => Self::Number {
                value: number,
                int: plain_int(number),
            },
            Value::Bool(flag) => Self::Bool {
                flag: *flag,
                int: i128::from(*flag),
            },
            _ => Self::Other,
        }
    }
}

fn num_cmp(
    bytes: &[u8],
    members: &Members,
    field: &str,
    cmp: Cmp,
    want: &Want<'_>,
    dialect: Dialect,
    scratch: &mut String,
) -> bool {
    let Some(got) = member_bytes(bytes, members, field) else {
        return false;
    };
    // The same `i128` fast form `cmp_spelling` takes, one level earlier: a plain
    // RFC integer spelling compares from its bytes, so the row never pays UTF-8
    // validation or a `Number` for the common integer operand.
    if !dialect.json5()
        && let Want::Number { int: Some(wanted), .. } = want
        && let Some(located) = plain_int_bytes(got)
    {
        return ordering_holds(cmp, Some(located.cmp(wanted)));
    }
    let Ok(text) = core::str::from_utf8(got) else {
        return false;
    };
    let ordering = cmp_spelling(text, want, dialect, scratch);
    ordering_holds(cmp, ordering)
}

/// Whether `cmp` holds for an ordering against the operand; `None` never matches.
fn ordering_holds(cmp: Cmp, ordering: Option<core::cmp::Ordering>) -> bool {
    match ordering {
        Some(core::cmp::Ordering::Greater) => matches!(cmp, Cmp::Gt | Cmp::Ge),
        Some(core::cmp::Ordering::Less) => matches!(cmp, Cmp::Lt | Cmp::Le),
        Some(core::cmp::Ordering::Equal) => matches!(cmp, Cmp::Ge | Cmp::Le),
        None => false,
    }
}

fn parse_src_number(text: &str, dialect: Dialect, scratch: &mut String) -> Option<structury::Number> {
    if dialect.json5() {
        let normalized = lex::number::normalize_json5_into(text, scratch);
        if let Some(value) = lex::number::non_finite_value(normalized) {
            return Some(structury::Number::NonFinite(value));
        }
        structury::Number::parse(normalized).ok()
    } else {
        structury::Number::parse(text).ok()
    }
}

/// `i128` for a plain RFC integer spelling without building a `Number`.
///
/// `None` for anything the exact-decimal path must own: a fraction or exponent,
/// `-0` (which orders below `0`), or a magnitude past `i128`. Leading zeroes are
/// accepted the way locate mode accepts them.
fn plain_int_spelling(text: &str) -> Option<i128> {
    plain_int_bytes(text.as_bytes())
}

/// [`plain_int_spelling`] over the source bytes, so an integer row compares
/// without first validating UTF-8.
fn plain_int_bytes(bytes: &[u8]) -> Option<i128> {
    let (negative, digits) = match bytes.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, bytes),
    };
    if digits.is_empty() {
        return None;
    }
    let mut magnitude: i128 = 0;
    for &byte in digits {
        let digit = i128::from(byte.wrapping_sub(b'0'));
        if !(0..=9).contains(&digit) {
            return None;
        }
        magnitude = magnitude.checked_mul(10)?.checked_add(digit)?;
    }
    if negative {
        if magnitude == 0 {
            return None;
        }
        magnitude.checked_neg()
    } else {
        Some(magnitude)
    }
}

/// Exact compare of a number spelling against a predicate operand. Plain
/// integers compare as `i128` with no `Number` formatting; anything else takes
/// the exact-decimal path. `want` carries the operand resolved once.
fn cmp_spelling(text: &str, want: &Want<'_>, dialect: Dialect, scratch: &mut String) -> Option<core::cmp::Ordering> {
    if !dialect.json5()
        && let Want::Number { int: Some(wanted), .. } = want
        && let Some(located) = plain_int_spelling(text)
    {
        return Some(located.cmp(wanted));
    }
    let located = parse_src_number(text, dialect, scratch)?;
    match want {
        Want::Number { value, int } => match (plain_int(&located), *int) {
            (Some(a), Some(b)) => Some(a.cmp(&b)),
            _ => located.numeric_cmp(value),
        },
        Want::Bool { flag, int } => match plain_int(&located) {
            Some(a) => Some(a.cmp(int)),
            None => located.numeric_cmp(&structury::Number::parse(if *flag { "1" } else { "0" }).ok()?),
        },
        Want::Other => None,
    }
}

/// `i128` for an integer that is not negative zero: `-0` orders below `0` in
/// [`structury::Number::numeric_cmp`], so it must stay on that path.
fn plain_int(n: &structury::Number) -> Option<i128> {
    let value = n.to_i128()?;
    if value == 0 && n.spelling().starts_with('-') {
        None
    } else {
        Some(value)
    }
}

/// Whether a `Slice` hit's element batch must be rebuilt by replaying the
/// windowed element spans. Only a flat projection (or the bare element) keeps
/// exactly one row per element, so only those can be windowed in place; a
/// nested `Collection`/`Slice`/spent spine, a `Project` with a non-empty path,
/// and a filtered nested can contribute several rows (or none) per element.
fn slice_replays(view: View<'_>) -> bool {
    let View::Slice { nested, .. } = view else {
        return false;
    };
    !matches!(
        flatten_view(View::from_nested(nested)),
        View::Whole | View::Project { path: [], .. }
    )
}

/// Elements a windowed array must consume before every hit at this level is
/// answered: the largest bounded end. `None` when any hit is not a `Slice` or
/// its window needs the array length (a negative or open bound), which forces
/// the full walk.
fn prefix_window(hits: &[Hit<'_>]) -> Option<usize> {
    let mut needed: Option<usize> = None;
    for h in hits {
        let View::Slice { range, .. } = h.view else {
            return None;
        };
        let end = range.end?;
        if end < 0 || range.start.is_some_and(|start| start < 0) {
            return None;
        }
        let end = usize::try_from(end).unwrap_or(usize::MAX);
        needed = Some(needed.map_or(end, |most| most.max(end)));
    }
    needed
}

#[derive(Clone, Copy)]
enum Cmp {
    Gt,
    Lt,
    Ge,
    Le,
}

#[cfg(test)]
mod rfc_scan_equivalence;

#[cfg(test)]
mod stop_window;
