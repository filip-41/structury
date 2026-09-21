//! Scan entries: [`validate`], [`scan`], [`scan_each`], [`scan_traced`].
//!
//! Trailing non-whitespace after one text value is `trailing-content`.
//! A stream is a virtual array of its records.

use alloc::vec::Vec;

use structury::{Answer, ColumnCell, Demand, Fact, Predicate, ScanResult, Strictness, Value};

use crate::dialect::Dialect;
use crate::error;
use crate::lex;
use crate::lex::MAX_NESTING;
use crate::trace::{NoTrace, Recorder, Trace, TraceSink, check_level};
use crate::walk;

/// How a source buffer holds JSON values. Format-owned: arrangement is codec identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum JsonInput {
    /// One complete JSON value. Trailing RFC 8259 whitespace is allowed.
    Text,
    /// Adjacent JSON values; leftover after one value is the next.
    Adjacent,
    /// Newline-delimited JSON values.
    Ndjson,
    /// RFC 7464 JSON text sequences (`0x1E` record separator).
    JsonSeq,
}

/// Scan request.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct ScanRequest<'a> {
    /// How the buffer holds values.
    pub input: JsonInput,
    /// Demanded work. One mark per entry.
    pub demands: &'a [Demand],
    /// Semantic dial. Default is Structural.
    pub strictness: Strictness,
    /// Container nesting bound for this scan. Defaults to [`MAX_NESTING`].
    pub max_nesting: u32,
    /// Grammar the buffer is read under. Default is RFC 8259.
    pub dialect: Dialect,
    /// Collect comment facts onto each built `Document`. Default `false`.
    ///
    /// `edit_document` implies it. Glyph spans are retained only under Strict.
    /// A commenting dialect with this off pays nothing.
    pub facts: bool,
}

impl Default for ScanRequest<'_> {
    /// [`JsonInput::Text`], no demands, [`Strictness::Structural`],
    /// [`MAX_NESTING`], [`Dialect::Rfc8259`], no facts.
    fn default() -> Self {
        Self {
            input: JsonInput::Text,
            demands: &[],
            strictness: Strictness::Structural,
            max_nesting: MAX_NESTING,
            dialect: Dialect::Rfc8259,
            facts: false,
        }
    }
}

impl<'a> ScanRequest<'a> {
    /// A request for `demands` over `input`, every other knob at its default.
    #[must_use]
    pub const fn new(input: JsonInput, demands: &'a [Demand]) -> Self {
        Self {
            input,
            demands,
            strictness: Strictness::Structural,
            max_nesting: MAX_NESTING,
            dialect: Dialect::Rfc8259,
            facts: false,
        }
    }

    /// This request with `strictness`.
    #[must_use]
    pub const fn with_strictness(mut self, strictness: Strictness) -> Self {
        self.strictness = strictness;
        self
    }

    /// This request with a `max_nesting` bound.
    #[must_use]
    pub const fn with_max_nesting(mut self, max_nesting: u32) -> Self {
        self.max_nesting = max_nesting;
        self
    }

    /// This request reading `dialect`.
    #[must_use]
    pub const fn with_dialect(mut self, dialect: Dialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// This request collecting comment facts (or not).
    #[must_use]
    pub const fn with_facts(mut self, facts: bool) -> Self {
        self.facts = facts;
        self
    }
}

/// Collect request facts over `[from, to)` when the request asks.
pub(crate) fn collect_facts<'src>(
    bytes: &'src [u8],
    from: usize,
    to: usize,
    req: &ScanRequest<'_>,
    out: &mut Vec<Fact<'src>>,
) {
    if req.facts {
        crate::facts::collect(
            bytes,
            from,
            to,
            req.dialect,
            matches!(req.strictness, Strictness::Strict),
            out,
        );
    }
}

/// Refuse a filter whose equality operand is a container.
/// The walk compares source spans without materializing values, so only scalar
/// operands apply. Every public scan and plan entry calls this once.
pub(crate) fn refuse_container_predicates(demands: &[Demand]) -> Result<(), structury::Error> {
    for demand in demands {
        match demand {
            Demand::Path { nested, .. } | Demand::Collection { nested, .. } | Demand::Slice { nested, .. } => {
                if let Some(nested) = nested {
                    refuse_container_predicates(core::slice::from_ref(nested))?;
                }
            }
            Demand::Filter { predicate, .. } => refuse_container_predicate(predicate)?,
            Demand::Whole | Demand::Project { .. } | Demand::Oracle(_) => {}
        }
    }
    Ok(())
}

fn refuse_container_predicate(predicate: &Predicate) -> Result<(), structury::Error> {
    match predicate {
        Predicate::Eq { value, .. } | Predicate::Ne { value, .. } => match value {
            Value::Array(_) | Value::Object(_) => Err(error::predicate_operand(0)),
            _ => Ok(()),
        },
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            refuse_container_predicate(a)?;
            refuse_container_predicate(b)
        }
        Predicate::Not(inner) => refuse_container_predicate(inner),
        Predicate::Gt { .. } | Predicate::Lt { .. } | Predicate::Ge { .. } | Predicate::Le { .. } => Ok(()),
    }
}

/// Value check selected by `strictness`. Strict validates values.
pub(crate) fn check_of(strictness: Strictness) -> lex::Check {
    match strictness {
        Strictness::Strict => lex::Check::Values,
        Strictness::Lazy | Strictness::Structural => lex::Check::Locate,
    }
}

/// Ops: strict-validate every byte of one text value under `dialect`.
///
/// # Errors
///
/// Grammar refusal.
pub fn validate(src: &[u8], dialect: Dialect) -> Result<(), structury::Error> {
    let req = ScanRequest {
        input: JsonInput::Text,
        demands: &[Demand::Whole],
        strictness: Strictness::Strict,
        max_nesting: MAX_NESTING,
        dialect,
        facts: false,
    };
    scan(src, &req).map(|_| ())
}

/// One-pass scan; `answers.len() == demands.len()`. Adjacent / NDJSON / JSON-seq
/// are a virtual array of top-level values.
///
/// ```
/// use structury::Demand;
/// use structury_json::{JsonInput, ScanRequest, scan};
///
/// let demands = [Demand::Whole];
/// let request = ScanRequest::new(JsonInput::Text, &demands);
/// let result = scan(br#"{"a":1}"#, &request).expect("valid");
/// assert_eq!(result.answers.len(), 1);
/// ```
///
/// # Errors
///
/// Grammar, shape, or empty input.
pub fn scan<'src>(src: &'src [u8], req: &ScanRequest<'_>) -> Result<ScanResult<'src>, structury::Error> {
    dispatch::<false, NoTrace>(src, req, &walk::NO_CONTROL, NoTrace).map(|(result, _)| result)
}

/// One-pass scan with a walk trace: what each demand produced, what was
/// skipped, and where the walk stopped early. Tracing the same request twice
/// yields the same events.
///
/// On any refusal nothing is returned, including no partial trace; the
/// refusal offset lives on the error.
///
/// ```
/// use structury::Demand;
/// use structury_json::{JsonInput, ScanRequest, scan_traced};
///
/// let demands = [Demand::Whole];
/// let request = ScanRequest::new(JsonInput::Text, &demands);
/// let (result, trace) = scan_traced(br#"{"a":1}"#, &request).expect("valid");
/// assert_eq!(result.answers.len(), 1);
/// assert_eq!(trace.counters.demands, 1);
/// ```
///
/// # Errors
///
/// Grammar, shape, or empty input.
pub fn scan_traced<'src>(
    src: &'src [u8],
    req: &ScanRequest<'_>,
) -> Result<(ScanResult<'src>, Trace), structury::Error> {
    let (result, mut trace) = dispatch::<false, Recorder>(src, req, &walk::NO_CONTROL, Recorder::new())?;
    trace.record_marks(req.demands, &result.answers);
    Ok((result, trace))
}

/// One-pass scan under a host [`structury::Control`]. The handle is polled once
/// per container child and once per stream record (never per byte); a stop
/// returns a [`structury::ErrorClass::Control`] error and no partial answer.
///
/// # Errors
///
/// Grammar, shape, empty input, or a control stop.
pub fn scan_controlled<'src>(
    src: &'src [u8],
    req: &ScanRequest<'_>,
    control: &structury::Control,
) -> Result<ScanResult<'src>, structury::Error> {
    dispatch::<true, NoTrace>(src, req, control, NoTrace).map(|(result, _)| result)
}

/// [`scan_controlled`] with a walk trace. See [`scan_traced`].
///
/// On any refusal nothing is returned, including no partial trace; the
/// refusal offset lives on the error.
///
/// # Errors
///
/// Grammar, shape, empty input, or a control stop.
pub fn scan_traced_controlled<'src>(
    src: &'src [u8],
    req: &ScanRequest<'_>,
    control: &structury::Control,
) -> Result<(ScanResult<'src>, Trace), structury::Error> {
    let (result, mut trace) = dispatch::<true, Recorder>(src, req, control, Recorder::new())?;
    trace.record_marks(req.demands, &result.answers);
    Ok((result, trace))
}

/// Shared dispatch for the four scan entries: `CONTROLLED` chooses the control
/// polls, `T` the trace sink.
fn dispatch<'src, const CONTROLLED: bool, T: TraceSink>(
    src: &'src [u8],
    req: &ScanRequest<'_>,
    control: &structury::Control,
    trace: T,
) -> Result<(ScanResult<'src>, Trace), structury::Error> {
    refuse_container_predicates(req.demands)?;
    match req.input {
        JsonInput::Text => scan_text::<CONTROLLED, T>(src, req, control, trace),
        JsonInput::Adjacent | JsonInput::Ndjson | JsonInput::JsonSeq => {
            scan_stream::<CONTROLLED, T>(src, req, control, trace)
        }
    }
}

/// Visit each framed value. One-text visits once. Streams visit per record.
/// A malformed final record is dropped. Use [`scan_each_with_issues`] to see it.
///
/// # Errors
///
/// Same class as [`scan`].
pub fn scan_each<'src>(
    src: &'src [u8],
    req: &ScanRequest<'_>,
    visit: impl FnMut(Answer<'src>),
) -> Result<(), structury::Error> {
    scan_each_with_issues(src, req, visit).map(|_| ())
}

/// Visit each framed value, returning the recovered per-record issues.
/// A malformed final record is reported here instead of dropped.
///
/// # Errors
///
/// Same class as [`scan`].
pub fn scan_each_with_issues<'src>(
    src: &'src [u8],
    req: &ScanRequest<'_>,
    mut visit: impl FnMut(Answer<'src>),
) -> Result<Vec<structury::Issue>, structury::Error> {
    refuse_container_predicates(req.demands)?;
    match req.input {
        JsonInput::Text => {
            let (result, _) = scan_text::<false, NoTrace>(src, req, &walk::NO_CONTROL, NoTrace)?;
            for answer in result.answers {
                visit(answer);
            }
            Ok(Vec::new())
        }
        JsonInput::Adjacent | JsonInput::Ndjson | JsonInput::JsonSeq => {
            let (ranges, mut issues) = frames_recovering(src, req.input, req.max_nesting, req.dialect)?;
            let mut facts = Vec::new();
            collect_facts(src, 0, src.len(), req, &mut facts);
            // The virtual-array mapping `scan` uses. A non-object record visits
            // the absent row (or nothing for a filter). An `Index`-scoped
            // demand visits once, on its record.
            let plan = StreamPlan::compile(req.demands, ranges.len());
            let mut cursor = 0usize;
            let mut stream = if plan.every.is_empty() {
                None
            } else {
                Some(walk::StreamWalker::<false>::new(
                    src,
                    &plan.every,
                    plan.every_laws.clone(),
                    plan.every_owners.clone(),
                    req.strictness,
                    req.max_nesting,
                    req.dialect,
                    &facts,
                    &walk::NO_CONTROL,
                    NoTrace,
                )?)
            };
            let check = check_of(req.strictness);
            let mut scratch: Vec<Answer<'src>> = alloc::vec![Answer::Missing; req.demands.len()];
            for (i, frame) in ranges.iter().enumerate() {
                if let Some(extra) = plan.extra_at(i, &mut cursor) {
                    let mut walker = walk::StreamWalker::<false>::new(
                        src,
                        &extra.demands,
                        extra.laws.clone(),
                        extra.owners.clone(),
                        req.strictness,
                        req.max_nesting,
                        req.dialect,
                        &facts,
                        &walk::NO_CONTROL,
                        NoTrace,
                    )?;
                    match walker.record(frame.start(), i, &plan.windows) {
                        Ok(()) => {
                            scratch.fill(Answer::Missing);
                            walker.fold(&mut scratch);
                            visit_slots(&mut scratch, &extra.owners, &mut visit);
                        }
                        Err(e) if i + 1 == ranges.len() => {
                            issues.push(structury::Error::from(e).into());
                        }
                        Err(e) => return Err(e.into()),
                    }
                } else if let Some(stream) = stream.as_mut() {
                    match stream.record(frame.start(), i, &plan.windows) {
                        Ok(()) => {
                            scratch.fill(Answer::Missing);
                            stream.fold(&mut scratch);
                            visit_slots(&mut scratch, &plan.every_owners, &mut visit);
                        }
                        Err(e) if i + 1 == ranges.len() => {
                            issues.push(structury::Error::from(e).into());
                        }
                        Err(e) => return Err(e.into()),
                    }
                } else {
                    // No every-record row demand: strict validation still sees
                    // every value.
                    match crate::lex::skip_value(src, frame.start(), check, req.max_nesting, req.dialect) {
                        Ok(_) => {}
                        Err(e) if i + 1 == ranges.len() => {
                            issues.push(structury::Error::from(e).into());
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            Ok(issues)
        }
    }
}

/// Visit one record's answers in demand order, taking each mark out of the scratch buffer.
fn visit_slots<'src>(scratch: &mut [Answer<'src>], owners: &[usize], visit: &mut impl FnMut(Answer<'src>)) {
    for &owner in owners {
        let answer = core::mem::replace(&mut scratch[owner], Answer::Missing);
        visit(answer);
    }
}

/// Whether facts fuse into the validating walk.
/// True when the request asks for facts on a commenting dialect with a Whole demand.
fn fuses_facts(req: &ScanRequest<'_>) -> bool {
    req.facts && req.dialect.has_comments() && req.demands.iter().any(|d| matches!(d, Demand::Whole))
}

fn scan_text<'src, const CONTROLLED: bool, T: TraceSink>(
    src: &'src [u8],
    req: &ScanRequest<'_>,
    control: &structury::Control,
    trace: T,
) -> Result<(ScanResult<'src>, Trace), structury::Error> {
    let bom = strip_bom(src);
    let pos = lex::skip_trivia(src, bom, req.dialect)?;
    if pos >= src.len() {
        return Err(error::expected_value(pos));
    }
    if fuses_facts(req) {
        return scan_text_fused::<CONTROLLED, T>(src, bom, pos, req, control, trace);
    }
    let mut facts = Vec::new();
    collect_facts(src, 0, src.len(), req, &mut facts);
    // Structural and Lazy stop only when every demand is a bounded `Slice` on one
    // array. Strict never stops early.
    let allow_stop = !matches!(req.strictness, Strictness::Strict);
    let (answers, end, stopped, trace) = walk::scan_root::<CONTROLLED, T>(
        src,
        pos,
        req.demands,
        req.strictness,
        req.max_nesting,
        req.dialect,
        &facts,
        allow_stop,
        control,
        trace,
    )?;
    if !stopped {
        let tail = lex::skip_trivia(src, end, req.dialect)?;
        if tail < src.len() {
            return Err(error::trailing_content(tail));
        }
    }
    debug_assert_eq!(answers.len(), req.demands.len());
    Ok((ScanResult::new(answers, Vec::new()), trace))
}

/// Exhaustive facts path: the walk records each comment as it skips trivia.
/// The pre-root and trailing gaps are recorded here around it.
fn scan_text_fused<'src, const CONTROLLED: bool, T: TraceSink>(
    src: &'src [u8],
    bom: usize,
    root_start: usize,
    req: &ScanRequest<'_>,
    control: &structury::Control,
    trace: T,
) -> Result<(ScanResult<'src>, Trace), structury::Error> {
    let (mut answers, end, stopped, inner, trace) = walk::scan_root_fused::<CONTROLLED, T>(
        src,
        root_start,
        req.demands,
        req.strictness,
        req.max_nesting,
        req.dialect,
        false,
        control,
        trace,
    )?;
    // The root is the `Whole` demand's answer, not the first document answer.
    // A keyed sibling document can precede it. `fuses_facts` guarantees a
    // `Whole` demand is present.
    let whole = req.demands.iter().position(|demand| matches!(demand, Demand::Whole));
    let root = whole
        .and_then(|at| answers.get(at))
        .and_then(|answer| match answer {
            Answer::Document(doc) => Some(doc.root()),
            _ => None,
        })
        .or_else(|| {
            answers.iter().find_map(|answer| match answer {
                Answer::Document(doc) => Some(doc.root()),
                _ => None,
            })
        });
    let Some(root) = root else {
        debug_assert_eq!(answers.len(), req.demands.len());
        return Ok((ScanResult::new(answers, Vec::new()), trace));
    };
    let tail = if stopped {
        end
    } else {
        lex::skip_trivia(src, end, req.dialect)?
    };
    if tail < src.len() {
        return Err(error::trailing_content(tail));
    }
    let retained = matches!(req.strictness, Strictness::Strict);
    let trailing = crate::facts::trailing_facts(src, end, tail, root, retained);
    let mut facts = crate::facts::leading_facts(src, bom, root_start, root, retained);
    // One reservation for the whole authored batch.
    facts.reserve(inner.len() + trailing.len());
    facts.extend(inner);
    facts.extend(trailing);
    attach_facts(&mut answers, facts);
    debug_assert_eq!(answers.len(), req.demands.len());
    Ok((ScanResult::new(answers, Vec::new()), trace))
}

/// Attach fused facts to every document answer.
/// Moves the batch into the last document.
fn attach_facts<'src>(answers: &mut [Answer<'src>], facts: Vec<Fact<'src>>) {
    if facts.is_empty() {
        return;
    }
    let documents = answers
        .iter()
        .filter(|answer| matches!(answer, Answer::Document(_)))
        .count();
    let mut remaining = documents;
    let mut batch = facts;
    for answer in answers.iter_mut() {
        if let Answer::Document(doc) = answer {
            remaining -= 1;
            // Fused facts are authored in ascending glyph order; `set_facts`
            // only rejects an unordered set, which the recorder cannot build.
            let facts = if remaining == 0 {
                core::mem::take(&mut batch)
            } else {
                batch.clone()
            };
            let _attached = doc.set_facts(facts);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one framed-record loop: skip / index-scoped / persistent-row arms share the plan cursor"
)]
fn scan_stream<'src, const CONTROLLED: bool, T: TraceSink>(
    src: &'src [u8],
    req: &ScanRequest<'_>,
    control: &structury::Control,
    trace: T,
) -> Result<(ScanResult<'src>, Trace), structury::Error> {
    if req.demands.iter().all(|d| matches!(d, Demand::Whole)) {
        return scan_stream_whole::<CONTROLLED, T>(src, req, control, trace);
    }
    let (ranges, mut issues) = frames_recovering(src, req.input, req.max_nesting, req.dialect)?;
    let mut facts = Vec::new();
    collect_facts(src, 0, src.len(), req, &mut facts);
    // The row demand set is compiled once. Allocations scale with the demand
    // set, not the record count.
    let plan = StreamPlan::compile(req.demands, ranges.len());
    let mut acc: Vec<Answer<'src>> = req.demands.iter().map(|_| Answer::Missing).collect();
    let mut cursor = 0usize;
    let mut records = 0u64;
    let check = check_of(req.strictness);
    let mut trace = Some(trace);
    let mut stream = if plan.every.is_empty() {
        None
    } else {
        Some(walk::StreamWalker::<CONTROLLED, T>::new(
            src,
            &plan.every,
            plan.every_laws.clone(),
            plan.every_owners.clone(),
            req.strictness,
            req.max_nesting,
            req.dialect,
            &facts,
            control,
            trace.take().expect("stream trace held"),
        )?)
    };
    let mut extra_traces = Vec::new();
    for (i, range) in ranges.iter().enumerate() {
        if let Some(extra) = plan.extra_at(i, &mut cursor) {
            // An `Index`-scoped demand applies to one record.
            let mut walker = walk::StreamWalker::<CONTROLLED, T>::new(
                src,
                &extra.demands,
                extra.laws.clone(),
                extra.owners.clone(),
                req.strictness,
                req.max_nesting,
                req.dialect,
                &facts,
                control,
                T::new(),
            )?;
            match walker.record(range.start(), i, &plan.windows) {
                Ok(()) => {
                    records += 1;
                    walker.fold(&mut acc);
                    if T::RECORDS {
                        extra_traces.push(walker.take_trace());
                    }
                }
                Err(e) if i + 1 == ranges.len() => {
                    issues.push(structury::Error::from(e).into());
                    break;
                }
                Err(e) => return Err(e.into()),
            }
            continue;
        }
        if let Some(stream) = stream.as_mut() {
            match stream.record(range.start(), i, &plan.windows) {
                Ok(()) => {
                    records += 1;
                    stream.fold(&mut acc);
                }
                Err(e) if i + 1 == ranges.len() => {
                    issues.push(structury::Error::from(e).into());
                    break;
                }
                Err(e) => return Err(e.into()),
            }
            continue;
        }
        // No every-record row demand: strict validation still sees every value.
        if CONTROLLED {
            if let Some(trace) = trace.as_mut() {
                trace.note_poll();
            }
            walk::check_control(control, range.start())?;
        }
        match crate::lex::skip_value(src, range.start(), check, req.max_nesting, req.dialect) {
            Ok(end) => {
                records += 1;
                if let Some(trace) = trace.as_mut() {
                    trace.note_skip(range.start(), end, check_level(check));
                }
            }
            Err(e) if i + 1 == ranges.len() => {
                issues.push(structury::Error::from(e).into());
                break;
            }
            Err(e) => return Err(e.into()),
        }
    }
    // A stream is a virtual array. A row demand no record answered gives the
    // empty batch, as on an empty text array.
    for ((row, law), &owner) in plan.every.iter().zip(&plan.every_laws).zip(&plan.every_owners) {
        if matches!(acc[owner], Answer::Missing) {
            acc[owner] = empty_stream_batch(src, row, law.as_ref());
        }
    }
    for (di, demand) in req.demands.iter().enumerate() {
        match demand {
            Demand::Oracle(
                structury::Oracle::Count | structury::Oracle::DescendCount | structury::Oracle::MemberCount,
            ) => {
                // A stream is a virtual array: its size is the record count.
                acc[di] = Answer::Oracle(structury::OracleAnswer::Count(records));
            }
            Demand::Oracle(structury::Oracle::Kind) => {
                acc[di] = Answer::Oracle(structury::OracleAnswer::Kind(structury::ValueKind::Array));
            }
            Demand::Oracle(structury::Oracle::MemberNames | structury::Oracle::HasKey { .. }) => {
                // Array keys are indices, not spellings; same as JSON on an array.
                acc[di] = Answer::TypeMismatch {
                    actual: structury::ValueKind::Array,
                };
            }
            // A key-scoped path on the virtual array is a type error, as on a text array.
            _ if matches!(first_step(demand), Some(structury::Step::Key(_))) => {
                acc[di] = Answer::TypeMismatch {
                    actual: structury::ValueKind::Array,
                };
            }
            _ => {}
        }
    }
    let mut trace = match stream.as_mut() {
        Some(stream) => stream.take_trace(),
        None => trace.take().map(|mut t| t.take_trace()).unwrap_or_default(),
    };
    for extra in extra_traces {
        trace.absorb(extra);
    }
    Ok((ScanResult::new(acc, issues), trace))
}

/// Per-record demand set for a stream scan, compiled once.
struct StreamPlan {
    /// Row demand for every record, in demand order.
    every: Vec<Demand>,
    /// The one row law of `every[k]`, from the array-level demand.
    every_laws: Vec<Option<walk::RowLaw>>,
    /// Answer slot of `every[k]`.
    every_owners: Vec<usize>,
    /// Records that additionally carry an `Index`-scoped demand, sorted by `at`.
    extras: Vec<RecordExtra>,
    /// Input-record windows, indexed by original answer slot.
    windows: Vec<Option<core::ops::Range<usize>>>,
}

struct RecordExtra {
    at: usize,
    demands: Vec<Demand>,
    laws: Vec<Option<walk::RowLaw>>,
    owners: Vec<usize>,
}

type PlanEntry = (usize, Demand, Option<walk::RowLaw>);

/// The `Index`-scoped demands that target one record, before merging.
struct RecordGroup {
    at: usize,
    entries: Vec<PlanEntry>,
}

impl StreamPlan {
    fn compile(demands: &[Demand], len: usize) -> Self {
        let mut every = Vec::new();
        let mut every_laws = Vec::new();
        let mut every_owners = Vec::new();
        let mut at: Vec<(usize, usize, Demand, Option<walk::RowLaw>)> = Vec::new();
        for (di, demand) in demands.iter().enumerate() {
            match classify_stream_row(demand, len) {
                Slot::Never => {}
                Slot::Every(row) => {
                    let law = walk::stream_every_law(demand, &row);
                    every_owners.push(di);
                    every_laws.push(law);
                    every.push(row);
                }
                Slot::At { at: target, row } => {
                    let law = walk::element_row_law(&row);
                    at.push((target, di, row, law));
                }
            }
        }
        at.sort_by_key(|entry| entry.0);
        // Group the `at` demands by target record, then merge each group into the
        // every-record set in demand order, so `scan_each` visits the `scan` order.
        let mut grouped: Vec<RecordGroup> = Vec::new();
        for (target, owner, row, law) in at {
            if grouped.last().is_some_and(|group| group.at == target) {
                grouped
                    .last_mut()
                    .expect("just checked")
                    .entries
                    .push((owner, row, law));
            } else {
                grouped.push(RecordGroup {
                    at: target,
                    entries: alloc::vec![(owner, row, law)],
                });
            }
        }
        let extras = grouped
            .into_iter()
            .map(|group| {
                let mut entries: Vec<PlanEntry> = every_owners
                    .iter()
                    .copied()
                    .zip(every.iter().cloned())
                    .zip(every_laws.iter().cloned())
                    .map(|((owner, demand), law)| (owner, demand, law))
                    .collect();
                entries.extend(group.entries);
                entries.sort_by_key(|&(owner, _, _)| owner);
                let mut owners = Vec::with_capacity(entries.len());
                let mut demands = Vec::with_capacity(entries.len());
                let mut laws = Vec::with_capacity(entries.len());
                for (owner, demand, law) in entries {
                    owners.push(owner);
                    demands.push(demand);
                    laws.push(law);
                }
                RecordExtra {
                    at: group.at,
                    demands,
                    laws,
                    owners,
                }
            })
            .collect();
        Self {
            every,
            every_laws,
            every_owners,
            extras,
            windows: demands
                .iter()
                .map(|demand| match demand {
                    Demand::Slice { range, .. } => Some(range.window(len)),
                    _ => None,
                })
                .collect(),
        }
    }

    /// The extra row set for record `i`. `cursor` tracks the ascending `extras` position.
    fn extra_at(&self, i: usize, cursor: &mut usize) -> Option<&RecordExtra> {
        if self.extras.get(*cursor).is_some_and(|extra| extra.at == i) {
            let extra = &self.extras[*cursor];
            *cursor += 1;
            Some(extra)
        } else {
            None
        }
    }
}

/// Compiled stream row mapping: applies to every record, or to one `Step::Index` target.
enum Slot {
    Never,
    Every(Demand),
    At { at: usize, row: Demand },
}

/// The leading path step of a demand whose path can select a record.
fn first_step(demand: &Demand) -> Option<&structury::Step> {
    match demand {
        Demand::Path { steps, .. }
        | Demand::Project {
            path: structury::Path { steps },
            ..
        }
        | Demand::Filter {
            path: structury::Path { steps },
            ..
        } => steps.first(),
        _ => None,
    }
}

/// Map a demand onto one record after its leading `Step::Index` is consumed.
fn record_demand(demand: &Demand) -> Demand {
    match demand {
        Demand::Path { steps, nested } => row_demand(&Demand::Path {
            steps: steps[1..].to_vec(),
            nested: nested.clone(),
        }),
        Demand::Project { path, fields } => row_demand(&Demand::Project {
            path: structury::Path {
                steps: path.steps[1..].to_vec(),
            },
            fields: fields.clone(),
        }),
        Demand::Filter {
            path,
            predicate,
            project,
        } => row_demand(&Demand::Filter {
            path: structury::Path {
                steps: path.steps[1..].to_vec(),
            },
            predicate: predicate.clone(),
            project: project.clone(),
        }),
        other => row_demand(other),
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "one temporary per demand at compile time, never held in bulk"
)]
fn classify_stream_row(demand: &Demand, len: usize) -> Slot {
    if matches!(demand, Demand::Oracle(_)) {
        return Slot::Never;
    }
    match first_step(demand) {
        Some(structury::Step::Index(index)) => {
            structury::resolve_index(len, *index).map_or(Slot::Never, |at| Slot::At {
                at,
                row: record_demand(demand),
            })
        }
        Some(structury::Step::Key(_)) => Slot::Never,
        _ => match demand {
            Demand::Path { nested, .. } | Demand::Slice { nested, .. } => {
                Slot::Every(row_demand(nested.as_deref().unwrap_or(&Demand::Whole)))
            }
            other => Slot::Every(row_demand(other)),
        },
    }
}

/// Map a virtual-array demand onto one record (path not yet consumed).
pub(crate) fn row_demand(demand: &Demand) -> Demand {
    row_demand_of(demand, false)
}

/// Map a demand whose array path the shard plan already consumed onto one
/// record. A non-empty `Project`/`Filter` path is dropped.
pub(crate) fn consumed_row_demand(demand: &Demand) -> Demand {
    row_demand_of(demand, true)
}

/// Shared row mapping. `path_consumed` drops a `Project`/`Filter` path.
fn row_demand_of(demand: &Demand, path_consumed: bool) -> Demand {
    match demand {
        Demand::Collection {
            fields: Some(fields),
            nested: None,
        } => Demand::Project {
            path: structury::Path::root(),
            fields: fields.clone(),
        },
        Demand::Collection {
            nested: Some(nested), ..
        } => row_demand_of(nested, false),
        Demand::Collection {
            fields: None,
            nested: None,
        }
        | Demand::Whole => Demand::Whole,
        Demand::Project { path, fields } if path_consumed || path.steps.is_empty() => Demand::Project {
            path: structury::Path::root(),
            fields: fields.clone(),
        },
        Demand::Filter {
            path,
            predicate,
            project,
        } if path_consumed || path.steps.is_empty() => Demand::Filter {
            path: structury::Path::root(),
            predicate: predicate.clone(),
            project: project.clone(),
        },
        Demand::Path { steps, nested: None } if path_consumed || steps.is_empty() => Demand::Whole,
        Demand::Path { nested, .. } if path_consumed => {
            row_demand_of(nested.as_deref().unwrap_or(&Demand::Whole), false)
        }
        other => other.clone(),
    }
}

/// Empty batch for one per-record row demand on a zero-record stream.
fn empty_stream_batch<'src>(src: &'src [u8], row: &Demand, law: Option<&walk::RowLaw>) -> Answer<'src> {
    let fields = match law {
        Some(law) => walk::row_fields(law.as_shape()),
        None => match row {
            Demand::Project { fields, .. } => fields.clone(),
            Demand::Filter { project, .. } if !project.is_empty() => project.clone(),
            _ => alloc::vec![alloc::string::String::from("$")],
        },
    };
    Answer::Columns(structury::Columns::new(src, fields))
}

fn frames_recovering(
    src: &[u8],
    input: JsonInput,
    max: u32,
    dialect: Dialect,
) -> Result<(Vec<structury::ByteRange>, Vec<structury::Issue>), structury::Error> {
    match input {
        JsonInput::Text => unreachable!("text is not framed"),
        JsonInput::Adjacent => {
            // A partial tail joins as one final range. The per-record walk
            // refuses it, and the last-record recovery reports the issue.
            let (mut ranges, tail) = crate::framing::adjacent_split(src, max, dialect);
            if tail < src.len()
                && let Some(span) = structury::ByteRange::try_new(tail, src.len())
            {
                ranges.push(span);
            }
            Ok((ranges, Vec::new()))
        }
        JsonInput::Ndjson => crate::framing::ndjson_payloads(src, dialect),
        JsonInput::JsonSeq => Ok((crate::framing::json_seq_ranges(src, max, dialect, false)?, Vec::new())),
    }
}

fn scan_stream_whole<'src, const CONTROLLED: bool, T: TraceSink>(
    src: &'src [u8],
    req: &ScanRequest<'_>,
    control: &structury::Control,
    mut trace: T,
) -> Result<(ScanResult<'src>, Trace), structury::Error> {
    let check = check_of(req.strictness);
    let level = check_level(check);
    let (ranges, mut issues) = frames_recovering(src, req.input, req.max_nesting, req.dialect)?;
    let mut columns = structury::Columns::new(src, alloc::vec![alloc::string::String::from("$")]);
    columns.reserve(ranges.len().min(1024));
    for (i, range) in ranges.iter().enumerate() {
        if CONTROLLED {
            trace.note_poll();
            walk::check_control(control, range.start())?;
        }
        match crate::lex::skip_value(src, range.start(), check, req.max_nesting, req.dialect) {
            Ok(end) => {
                trace.note_skip(range.start(), end, level);
                if let Some(span) = structury::ByteRange::try_new(range.start(), end) {
                    columns.push(ColumnCell::Span(span));
                }
            }
            Err(e) if i + 1 == ranges.len() => {
                issues.push(structury::Error::from(e).into());
            }
            Err(e) => return Err(e.into()),
        }
    }
    // Every demand of an all-Whole stream gets the same virtual-array columns.
    // `Columns` shares its cells across clones.
    let n = req.demands.len();
    let answers = (0..n).map(|_| Answer::Columns(columns.clone())).collect();
    Ok((ScanResult::new(answers, issues), trace.take_trace()))
}

pub(crate) fn strip_bom(src: &[u8]) -> usize {
    if src.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 }
}
