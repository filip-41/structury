//! Host re-entry parallelism, owned by one [`Plan`]. The codec starts no thread.
//!
//! [`Plan`] owns the quote-aware cut and the per-part scan; [`structury::Drive`] runs the parts (hitch → serial fallback).

use alloc::vec::Vec;

use structury::ByteRange;
use structury::byte_scan::prefix_len;
use structury::{
    Answer, ColumnCell, Columns, Control, Demand, Error, Oracle, OracleAnswer, ScanResult, Shard, Step, Strictness,
};

use crate::dialect::Dialect;
use crate::error;
use crate::framing;
use crate::lex;
use crate::lex::stop_sets::{NdjsonFrame, SingleStringEnd, Star, StringEnd};
use crate::scan::{JsonInput, ScanRequest, collect_facts};

/// First-cut morsel size.
pub const FIRST_SHARD_BYTES: usize = 256 * 1024;

/// A shard plan for one request over one source. Built by a serial cut of the
/// array body ([`Plan::build`]); the host re-enters [`Plan::scan`] per morsel. A
/// plan that names fewer than two elements, or a non-eligible request, is one
/// whole-document range.
#[derive(Clone, Debug)]
pub struct Plan {
    parts: Parts,
    len: usize,
    /// The request the plan was built for, so [`Plan::scan`] can re-enter
    /// without the host passing it again and without rebuilding per morsel.
    demands: Vec<Demand>,
    strictness: Strictness,
    max_nesting: u32,
    dialect: Dialect,
    input: JsonInput,
    facts: bool,
    window: Window,
}

/// The per-morsel demand projection, compiled once so [`Plan::scan`] does not
/// re-clone every `Project`/`Filter` field and predicate per morsel.
#[derive(Clone, Debug)]
struct Window {
    /// One flag per original demand: a `Sum` demand is answered by the window's
    /// value count, not by a mapped record demand.
    sums: Vec<bool>,
    /// Every non-`Sum` demand mapped onto one record (array path consumed).
    mapped: Vec<Demand>,
}

impl Window {
    fn compile(req: &ScanRequest<'_>) -> Self {
        let mut sums = alloc::vec![false; req.demands.len()];
        let mut mapped: Vec<Demand> = Vec::new();
        for (i, demand) in req.demands.iter().enumerate() {
            if demand.shard() == Shard::Sum {
                sums[i] = true;
            } else {
                mapped.push(crate::scan::consumed_row_demand(demand));
            }
        }
        Self { sums, mapped }
    }
}

/// The plan's morsel source: array element spans, or packed ranges.
#[derive(Clone, Debug)]
enum Parts {
    /// Text array: top-level element spans and the offset just past `]`; morsels
    /// are packed from these at [`Plan::ranges`] time.
    Elements { spans: Vec<ByteRange>, close_end: usize },
    /// A stream partition or a whole-document fallback: already morselized.
    Packed(Vec<ByteRange>),
}

impl Plan {
    /// Plan a request: the serial cut for a text array, the frame partition for a
    /// stream, or one whole range when the request is not shard-eligible.
    ///
    /// # Errors
    ///
    /// Grammar while locating a root/path array.
    pub fn build(src: &[u8], req: &ScanRequest<'_>) -> Result<Self, Error> {
        crate::scan::refuse_container_predicates(req.demands)?;
        if !shard_eligible(req) {
            return Ok(Self::packed(src, Vec::new(), req));
        }
        match req.input {
            JsonInput::Ndjson => Ok(Self::packed(
                src,
                framing::partition_ndjson(src, FIRST_SHARD_BYTES),
                req,
            )),
            JsonInput::JsonSeq => Ok(Self::packed(
                src,
                framing::partition_json_seq(src, FIRST_SHARD_BYTES),
                req,
            )),
            JsonInput::Adjacent => Ok(Self::packed(
                src,
                framing::partition_adjacent(src, FIRST_SHARD_BYTES),
                req,
            )),
            JsonInput::Text => {
                let Some(steps) = shared_path(req) else {
                    return Ok(Self::packed(src, Vec::new(), req));
                };
                let Some(mut start) = resolve_array_start_first(src, steps, req.max_nesting, req.dialect)? else {
                    return Ok(Self::packed(src, Vec::new(), req));
                };
                let (mut spans, mut close_end) = element_ranges(src, start + 1)?;
                if !matches!(
                    shadowed_after(src, steps, close_end, req.max_nesting, req.dialect),
                    Shadow::Clear
                ) {
                    // The eager last-match walk is authoritative.
                    let Some(last) = resolve_array_start_last(src, steps, req.max_nesting, req.dialect)? else {
                        return Ok(Self::packed(src, Vec::new(), req));
                    };
                    if last != start {
                        start = last;
                        (spans, close_end) = element_ranges(src, start + 1)?;
                    }
                }
                if spans.len() < 2 {
                    return Ok(Self::packed(src, Vec::new(), req));
                }
                validate_tail(src, req, close_end)?;
                Ok(Self::from_elements(src, spans, close_end, req))
            }
        }
    }

    /// One plan from already-indexed element spans: one whole range when fewer
    /// than two elements were named.
    fn from_elements(src: &[u8], spans: Vec<ByteRange>, close_end: usize, req: &ScanRequest<'_>) -> Self {
        let mut plan = Self::packed(src, Vec::new(), req);
        if spans.len() >= 2 {
            plan.parts = Parts::Elements { spans, close_end };
        }
        plan
    }

    /// The retained plan the host summaries describe, so planning is paid once
    /// and later queries are index lookup + scan. The summaries must cover the
    /// array as [`Plan::host_ranges`] requires. A non-eligible request, or one
    /// whose tail fails the trailing-content check, falls back to one
    /// whole-document range (which [`Plan::scan`] validates).
    #[must_use]
    pub fn from_summaries(src: &[u8], start: usize, summaries: &[CutSummary], req: &ScanRequest<'_>) -> Self {
        // A container operand cannot be served; the fallback plan's scan refuses
        // it with the shape error, and this entry cannot return one itself.
        if crate::scan::refuse_container_predicates(req.demands).is_err() {
            return Self::packed(src, Vec::new(), req);
        }
        if !shard_eligible(req) {
            return Self::packed(src, Vec::new(), req);
        }
        let (spans, close_end) = elements_from_summaries(src, start, summaries);
        if spans.len() >= 2 {
            let shadowed = shared_path(req).is_some_and(|steps| {
                !matches!(
                    shadowed_after(src, steps, close_end, req.max_nesting, req.dialect),
                    Shadow::Clear
                )
            });
            if shadowed || validate_tail(src, req, close_end).is_err() {
                return Self::packed(src, Vec::new(), req);
            }
        }
        Self::from_elements(src, spans, close_end, req)
    }

    /// Host protocol entry: morsel ranges from host-built block summaries. The
    /// host partitions the array body into contiguous blocks, summarizes each
    /// with [`Plan::cut`], and passes them in ascending order. Streams and
    /// non-array shapes fall back to [`Plan::build`].
    ///
    /// # Errors
    ///
    /// Grammar while resolving the array path.
    pub fn host_ranges(
        src: &[u8],
        req: &ScanRequest<'_>,
        summaries: &[CutSummary],
        target: usize,
    ) -> Result<Vec<ByteRange>, Error> {
        crate::scan::refuse_container_predicates(req.demands)?;
        if !matches!(req.input, JsonInput::Text) || !shard_eligible(req) {
            return Ok(Self::build(src, req)?.ranges(target));
        }
        let Some(steps) = shared_path(req) else {
            return Ok(one_range_len(src.len()));
        };
        match resolve_array_start_first(src, steps, req.max_nesting, req.dialect)? {
            Some(start) => {
                let (spans, close_end) = elements_from_summaries(src, start, summaries);
                if !matches!(
                    shadowed_after(src, steps, close_end, req.max_nesting, req.dialect),
                    Shadow::Clear
                ) {
                    // The eager walk owns the last match, and the serial cut
                    // must revalidate the body it skips here.
                    return Ok(Self::build(src, req)?.ranges(target));
                }
                if let Some(error) = summary_fault(src, summaries, close_end) {
                    return Err(error);
                }
                if spans.len() >= 2 {
                    validate_tail(src, req, close_end)?;
                }
                Ok(Self::from_elements(src, spans, close_end, req).ranges(target))
            }
            None => Ok(one_range_len(src.len())),
        }
    }

    /// The first `[` the request's array path names, without scanning its body.
    ///
    /// A later duplicate member can take the path instead; the planning entries
    /// ([`Plan::build`], [`Plan::host_ranges`], [`Plan::from_summaries`]) resolve
    /// that shadow and fall back to the serial walk when it needs decoded names.
    /// Hosts use this entry to bound the region they summarize.
    ///
    /// # Errors
    ///
    /// Grammar while resolving the path.
    pub fn array_start(src: &[u8], req: &ScanRequest<'_>) -> Result<Option<usize>, Error> {
        match shared_path(req) {
            Some(steps) => resolve_array_start_first(src, steps, req.max_nesting, req.dialect),
            None => Ok(None),
        }
    }

    /// One host block's cut summary under the outside state.
    #[must_use]
    pub fn cut(src: &[u8], block: ByteRange) -> CutSummary {
        CutSummary {
            block,
            scan: scan_block(src, block, ScanState::Normal),
        }
    }

    /// Morsel ranges of about `target` bytes, or one full range.
    ///
    /// # Panics
    ///
    /// Never: ranges are built from slice bounds.
    #[must_use]
    pub fn ranges(&self, target: usize) -> Vec<ByteRange> {
        match &self.parts {
            Parts::Elements { spans, close_end } => {
                let packed = pack_elements(spans, target, *close_end);
                if packed.len() <= 1 {
                    one_range_len(self.len)
                } else {
                    packed
                }
            }
            Parts::Packed(ranges) => ranges.clone(),
        }
    }

    /// Indexed top-level element count (0 for a stream or a whole-range plan).
    #[must_use]
    pub fn elements(&self) -> usize {
        match &self.parts {
            Parts::Elements { spans, .. } => spans.len(),
            Parts::Packed(_) => 0,
        }
    }

    /// Scan one planned range: a slice of root-array elements (no brackets) or a
    /// full document. Spans stay in `src` coordinates; the request and demand
    /// projection were compiled at build time, so re-entry allocates only answers.
    ///
    /// # Errors
    ///
    /// Grammar inside the range.
    pub fn scan<'src>(&self, src: &'src [u8], range: ByteRange) -> Result<ScanResult<'src>, Error> {
        self.scan_with::<false>(src, range, &crate::walk::NO_CONTROL)
    }

    /// [`Plan::scan`] under a host [`Control`], polled per window value and at the
    /// walk's container boundaries.
    ///
    /// # Errors
    ///
    /// Grammar inside the range, or a control stop.
    pub fn scan_controlled<'src>(
        &self,
        src: &'src [u8],
        range: ByteRange,
        control: &Control,
    ) -> Result<ScanResult<'src>, Error> {
        self.scan_with::<true>(src, range, control)
    }

    fn scan_with<'src, const CONTROLLED: bool>(
        &self,
        src: &'src [u8],
        range: ByteRange,
        control: &Control,
    ) -> Result<ScanResult<'src>, Error> {
        let slice_end = range.end().min(src.len());
        if range.start() > slice_end {
            return Err(error::shape("the range starts past the source", range.start()));
        }
        if range.start() == 0 && slice_end == src.len() {
            return if CONTROLLED {
                crate::scan_controlled(src, &self.request(), control)
            } else {
                crate::scan(src, &self.request())
            };
        }
        let mut adj = self.request();
        adj.input = JsonInput::Adjacent;
        // The element spans name how many rows this range can produce, so a
        // projected accumulator can reserve once instead of growing per row.
        let rows_hint = match &self.parts {
            Parts::Elements { spans, .. } => {
                let from = spans.partition_point(|span| span.end() <= range.start());
                let to = spans.partition_point(|span| span.start() < slice_end);
                Some(to.saturating_sub(from))
            }
            Parts::Packed(_) => None,
        };
        // A Text range is root-array elements, so one array-body walk answers it.
        // A stream range is whole records: the serial per-record mapping is not
        // the array-body mapping, so streams keep the per-element route.
        scan_adjacent_window::<CONTROLLED>(
            src,
            range.start(),
            slice_end,
            &adj,
            &self.window,
            matches!(self.input, JsonInput::Text),
            rows_hint,
            control,
        )
    }

    /// The request this plan was built for, borrowing the stored demands.
    fn request(&self) -> ScanRequest<'_> {
        ScanRequest {
            input: self.input,
            demands: &self.demands,
            strictness: self.strictness,
            max_nesting: self.max_nesting,
            dialect: self.dialect,
            facts: self.facts,
        }
    }

    /// A whole-document fallback plan: one part, no retained elements.
    fn packed(src: &[u8], ranges: Vec<ByteRange>, req: &ScanRequest<'_>) -> Self {
        let ranges = if ranges.is_empty() {
            one_range_len(src.len())
        } else {
            ranges
        };
        Self {
            parts: Parts::Packed(ranges),
            len: src.len(),
            demands: req.demands.to_vec(),
            strictness: req.strictness,
            max_nesting: req.max_nesting,
            dialect: req.dialect,
            input: req.input,
            facts: req.facts,
            window: Window::compile(req),
        }
    }
}

fn one_range_len(len: usize) -> Vec<ByteRange> {
    alloc::vec![ByteRange::try_new(0, len).expect("ordered")]
}

/// One block's cut summary; the block range lets the combiner rescan a mid-string start.
#[derive(Clone, Debug)]
pub struct CutSummary {
    block: ByteRange,
    scan: BlockScan,
}

/// Top-level element spans from ordered block summaries, streamed straight into
/// elements (no comma-offset buffer). Only a block that begins inside a string
/// or a comment is rescanned, under the exact incoming state.
fn elements_from_summaries(src: &[u8], array_start: usize, summaries: &[CutSummary]) -> (Vec<ByteRange>, usize) {
    let mut out = Vec::new();
    let mut depth: i64 = 1;
    let mut state = ScanState::Normal;
    let mut start = array_start + 1;
    for summary in summaries {
        let owned;
        let scan = if state == ScanState::Normal {
            &summary.scan
        } else {
            owned = scan_block(src, summary.block, state);
            &owned
        };
        let close = scan
            .closes
            .iter()
            .find(|close| depth + i64::from(close.local_depth) == 1)
            .map(|close| close.offset);
        for comma in &scan.commas {
            if depth + i64::from(comma.local_depth) == 1 && close.is_none_or(|offset| comma.offset < offset) {
                push_element(&mut out, src, start, comma.offset);
                start = comma.offset + 1;
            }
        }
        if let Some(offset) = close {
            push_element(&mut out, src, start, offset);
            return (out, offset + 1);
        }
        depth += scan.depth_delta;
        state = scan.end;
    }
    let end = summaries.last().map_or(src.len(), |s| s.block.end().min(src.len()));
    push_element(&mut out, src, start, end);
    (out, end)
}

/// The shared `Shard` law plus the cuts a dialect cannot express: the adjacent
/// and JSON-seq framers are RFC-only, so a comment dialect cannot be cut there.
/// The demand whitelist is [`Demand::shard`].
fn shard_eligible(req: &ScanRequest<'_>) -> bool {
    if req.dialect.has_comments() && matches!(req.input, JsonInput::Adjacent | JsonInput::JsonSeq) {
        return false;
    }
    // A stream is a virtual array, not the object containing a text array.
    // Key spines (and wrapped counts) have no record-wise shard law here.
    if req.input != JsonInput::Text
        && req.demands.iter().any(|demand| match demand {
            Demand::Path { .. } => true,
            Demand::Project { path, .. } | Demand::Filter { path, .. } => !path.steps.is_empty(),
            _ => false,
        })
    {
        return false;
    }
    structury::Drive::eligible(req.demands)
}

/// The first byte-level lex fault of a block scan, so the serial planner raises the lexer's error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanFault {
    /// A `/` that starts no comment; offset is the slash.
    Slash(usize),
    /// A string left open at input end; reported at `src.len()`, as the string lexer does.
    String,
    /// A `/*` left open at the end of input; offset is the opening `/`.
    Block(usize),
}

impl ScanFault {
    /// The byte the fault is attributed to, for ordering against the closer.
    fn position(self, len: usize) -> usize {
        match self {
            Self::Slash(at) | Self::Block(at) => at,
            Self::String => len,
        }
    }
}

/// Scanner state carried across a block boundary.
/// A `/` or `'` outside a string is invalid in every dialect the scanner reads.
/// The cut needs no dialect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ScanState {
    #[default]
    Normal,
    /// Inside a string; `quote` is the delimiter (`"` or `'`).
    String { quote: u8, escaped: bool },
    /// Inside a `//` comment, which ends at `\n` or `\r`.
    LineComment,
    /// Inside a `/* */` comment; `star` is whether the previous byte was `*`.
    BlockComment { star: bool },
    /// The block ended on a `/`; the next byte decides whether a comment opens.
    Slash,
}

/// One scan of a block under a fixed start state.
#[derive(Clone, Debug, Default)]
struct BlockScan {
    /// Array-level comma candidates at the block's running minimum local depth.
    commas: Vec<CutComma>,
    /// `]` offsets with their (signed) local depth.
    closes: Vec<CutClose>,
    /// Net bracket depth over the block (string and comment content ignored).
    depth_delta: i64,
    /// State at the block's end.
    end: ScanState,
    /// The first lex fault; the host combiner ignores it, the serial planner raises it.
    fault: Option<ScanFault>,
}

/// One comma candidate.
#[derive(Clone, Copy, Debug)]
struct CutComma {
    offset: usize,
    local_depth: i32,
}

/// One `]` candidate.
#[derive(Clone, Copy, Debug)]
struct CutClose {
    offset: usize,
    local_depth: i32,
}

/// Advance one step for a string- or comment-state byte: the next state and
/// `pos`, or `None` when `state` is `Normal` and the byte needs the structural
/// match. A `Slash` that is not followed by `/` or `*` returns `(Normal, pos)`
/// so the caller reprocesses the byte.
fn advance_trivia(src: &[u8], pos: usize, end: usize, state: ScanState) -> Option<(ScanState, usize)> {
    match state {
        ScanState::String { quote, escaped } => {
            if escaped {
                return Some((ScanState::String { quote, escaped: false }, pos + 1));
            }
            let run = if quote == b'"' {
                prefix_len::<StringEnd>(&src[pos..end])
            } else {
                prefix_len::<SingleStringEnd>(&src[pos..end])
            };
            let next = pos + run;
            if next >= end {
                return Some((state, next));
            }
            let state = if src[next] == quote {
                ScanState::Normal
            } else {
                ScanState::String { quote, escaped: true }
            };
            Some((state, next + 1))
        }
        ScanState::LineComment => {
            let next = pos + prefix_len::<NdjsonFrame>(&src[pos..end]);
            Some((if next >= end { state } else { ScanState::Normal }, next))
        }
        ScanState::BlockComment { star } => {
            if star && src[pos] == b'/' {
                return Some((ScanState::Normal, pos + 1));
            }
            let next = pos + prefix_len::<Star>(&src[pos..end]);
            if next >= end {
                return Some((ScanState::BlockComment { star: false }, next));
            }
            Some((ScanState::BlockComment { star: true }, next + 1))
        }
        ScanState::Slash => Some(match src[pos] {
            b'/' => (ScanState::LineComment, pos + 1),
            b'*' => (ScanState::BlockComment { star: false }, pos + 1),
            _ => (ScanState::Normal, pos),
        }),
        ScanState::Normal => None,
    }
}

fn scan_block(src: &[u8], block: ByteRange, start: ScanState) -> BlockScan {
    let mut scan = BlockScan::default();
    let mut pos = block.start();
    let end = block.end().min(src.len());
    let mut depth: i64 = 0;
    let mut min_depth: i64 = 0;
    let mut state = start;
    let mut open_block = None;
    while pos < end {
        if let Some((next, at)) = advance_trivia(src, pos, end, state) {
            state = next;
            pos = at;
            continue;
        }
        match src[pos] {
            b'"' | b'\'' => {
                state = ScanState::String {
                    quote: src[pos],
                    escaped: false,
                };
                pos += 1;
            }
            b'/' => {
                // The byte after the block is not read: the carry must
                // describe exactly the boundary so the combiner's rescan of
                // the next block starts in the same place.
                if pos + 1 < end {
                    match src[pos + 1] {
                        b'/' => {
                            state = ScanState::LineComment;
                            pos += 2;
                        }
                        b'*' => {
                            state = ScanState::BlockComment { star: false };
                            open_block = Some(pos);
                            pos += 2;
                        }
                        _ => {
                            // A `/` outside a string or comment starts no token in
                            // any dialect; the serial planner reports it, the host
                            // cut ignores it.
                            if scan.fault.is_none() {
                                scan.fault = Some(ScanFault::Slash(pos));
                            }
                            pos += 1;
                        }
                    }
                } else {
                    state = ScanState::Slash;
                    pos += 1;
                }
            }
            b'{' | b'[' => {
                depth += 1;
                pos += 1;
            }
            b'}' => {
                depth -= 1;
                min_depth = min_depth.min(depth);
                pos += 1;
            }
            b']' => {
                scan.closes.push(CutClose {
                    offset: pos,
                    local_depth: i32::try_from(depth).unwrap_or(i32::MAX),
                });
                depth -= 1;
                min_depth = min_depth.min(depth);
                pos += 1;
            }
            b',' => {
                // An array-level comma sits at the block's running minimum local
                // depth: field commas are always one level above it. Recording
                // only those drops the whole field-comma population while the
                // combiner still filters spurious pre-close candidates by global
                // depth.
                if depth == min_depth {
                    scan.commas.push(CutComma {
                        offset: pos,
                        local_depth: i32::try_from(depth).unwrap_or(i32::MIN),
                    });
                }
                pos += 1;
            }
            _ => pos += 1,
        }
    }
    scan.depth_delta = depth;
    scan.end = state;
    // A string or block comment that reaches the end of input is unterminated.
    // A trailing `/` is a lone slash. Both are faults only at the true end of
    // `src`; a host block ending mid-string is a carried state the combiner
    // rescans, not an error.
    if end == src.len() && scan.fault.is_none() {
        scan.fault = match state {
            ScanState::String { .. } => Some(ScanFault::String),
            ScanState::BlockComment { .. } => open_block.map(ScanFault::Block),
            ScanState::Slash => Some(ScanFault::Slash(end - 1)),
            ScanState::Normal | ScanState::LineComment => None,
        };
    }
    scan
}

/// The one array path every demand shares, or `None` for one full range.
fn shared_path<'a>(req: &ScanRequest<'a>) -> Option<&'a [Step]> {
    let mut path: Option<&[Step]> = None;
    for demand in req.demands {
        let this = array_path(demand)?;
        match path {
            None => path = Some(this),
            Some(seen) if seen == this => {}
            Some(_) => return None,
        }
    }
    path
}

/// The array path a shard-eligible demand resolves before its element work.
fn array_path(demand: &Demand) -> Option<&[Step]> {
    match demand {
        // A trailing `Index` selects one element and answers it in element
        // position (one projected/filtered row), not an array to map, so the
        // plan falls back to the serial answer. A `Key` spine locates the array
        // to map.
        Demand::Project { path, .. } | Demand::Filter { path, .. } => match path.steps.last() {
            Some(Step::Index(_)) => None,
            _ => Some(&path.steps),
        },
        Demand::Path { steps, nested: Some(_) } => Some(steps),
        // A bare `Count` observes the root container, like the root row demands.
        Demand::Collection { .. } | Demand::Slice { .. } | Demand::Oracle(Oracle::Count) => Some(&[]),
        _ => None,
    }
}

/// Start offset of the `[` the path's last matching member resolves to, or
/// `None`.
///
/// The eager walk: every value on the way is skipped, so a later duplicate
/// member is found. [`resolve_array_start_first`] is the fast path;
/// [`shadowed_after`] decides when this one must run.
fn resolve_array_start_last(bytes: &[u8], steps: &[Step], max: u32, dialect: Dialect) -> Result<Option<usize>, Error> {
    let mut pos = crate::scan::strip_bom(bytes);
    pos = lex::skip_trivia(bytes, pos, dialect)?;
    for step in steps {
        match step {
            Step::Key(name) => {
                if bytes.get(pos) != Some(&b'{') {
                    return Ok(None);
                }
                pos += 1;
                let mut found = None;
                loop {
                    pos = lex::skip_trivia(bytes, pos, dialect)?;
                    let Some(&byte) = bytes.get(pos) else {
                        return Ok(None);
                    };
                    if byte == b'}' {
                        break;
                    }
                    let Ok(key_end) = lex::skip_key(bytes, pos, lex::Check::Locate, dialect) else {
                        return Ok(None);
                    };
                    let raw = key_text(bytes, pos, key_end);
                    pos = lex::skip_trivia(bytes, key_end, dialect)?;
                    if bytes.get(pos) != Some(&b':') {
                        return Ok(None);
                    }
                    pos += 1;
                    pos = lex::skip_trivia(bytes, pos, dialect)?;
                    // An escaped spelling may shadow an earlier plain key.
                    // Leave decoded-key resolution to the serial walk.
                    if raw.contains(&b'\\') {
                        return Ok(None);
                    }
                    let matches_name = raw == name.as_bytes();
                    if matches_name {
                        found = Some(pos);
                    }
                    // Keep the array planner's lexical diagnostics while looking
                    // beyond a matching array for a later duplicate member.
                    pos = if matches_name && bytes.get(pos) == Some(&b'[') {
                        element_ranges(bytes, pos + 1)?.1
                    } else {
                        lex::skip_value(bytes, pos, lex::Check::Locate, max, dialect)?
                    };
                    pos = lex::skip_trivia(bytes, pos, dialect)?;
                    if bytes.get(pos) == Some(&b',') {
                        pos += 1;
                    } else {
                        break;
                    }
                }
                let Some(at) = found else {
                    return Ok(None);
                };
                pos = at;
            }
            Step::Index(index) => {
                if *index < 0 || bytes.get(pos) != Some(&b'[') {
                    return Ok(None);
                }
                pos += 1;
                let mut idx = 0usize;
                loop {
                    pos = lex::skip_trivia(bytes, pos, dialect)?;
                    if bytes.get(pos) == Some(&b']') {
                        return Ok(None);
                    }
                    if idx > 0 {
                        if bytes.get(pos) != Some(&b',') {
                            return Ok(None);
                        }
                        pos += 1;
                        pos = lex::skip_trivia(bytes, pos, dialect)?;
                    }
                    if idx == usize::try_from(*index).unwrap_or(usize::MAX) {
                        break;
                    }
                    pos = lex::skip_value(bytes, pos, lex::Check::Locate, max, dialect)?;
                    idx += 1;
                }
            }
        }
    }
    Ok((bytes.get(pos) == Some(&b'[')).then_some(pos))
}

/// Start offset of the `[` the path's first matching member resolves to,
/// without scanning the matched array's body.
///
/// Each step takes its first match that can carry the next one: a `Key` step
/// needs an object, the final step an array. A later duplicate member can take
/// the path instead; [`shadowed_after`] decides, and the caller falls back to
/// [`resolve_array_start_last`], whose last-match walk is authoritative.
fn resolve_array_start_first(bytes: &[u8], steps: &[Step], max: u32, dialect: Dialect) -> Result<Option<usize>, Error> {
    let mut pos = crate::scan::strip_bom(bytes);
    pos = lex::skip_trivia(bytes, pos, dialect)?;
    for (index, step) in steps.iter().enumerate() {
        let last = index + 1 == steps.len();
        match step {
            Step::Key(name) => {
                if bytes.get(pos) != Some(&b'{') {
                    return Ok(None);
                }
                pos += 1;
                let wanted = if last { b'[' } else { b'{' };
                let mut found = None;
                loop {
                    pos = lex::skip_trivia(bytes, pos, dialect)?;
                    let Some(&byte) = bytes.get(pos) else {
                        return Ok(None);
                    };
                    if byte == b'}' {
                        break;
                    }
                    let Ok(key_end) = lex::skip_key(bytes, pos, lex::Check::Locate, dialect) else {
                        return Ok(None);
                    };
                    let raw = key_text(bytes, pos, key_end);
                    pos = lex::skip_trivia(bytes, key_end, dialect)?;
                    if bytes.get(pos) != Some(&b':') {
                        return Ok(None);
                    }
                    pos = lex::skip_trivia(bytes, pos + 1, dialect)?;
                    // An escaped spelling may shadow a plain key; leave
                    // decoded-key resolution to the serial walk.
                    if raw.contains(&b'\\') {
                        return Ok(None);
                    }
                    if raw == name.as_bytes() && bytes.get(pos) == Some(&wanted) {
                        found = Some(pos);
                        break;
                    }
                    pos = lex::skip_value(bytes, pos, lex::Check::Locate, max, dialect)?;
                    pos = lex::skip_trivia(bytes, pos, dialect)?;
                    if bytes.get(pos) == Some(&b',') {
                        pos += 1;
                    } else {
                        break;
                    }
                }
                let Some(at) = found else {
                    return Ok(None);
                };
                pos = at;
            }
            Step::Index(index) => {
                if *index < 0 || bytes.get(pos) != Some(&b'[') {
                    return Ok(None);
                }
                pos += 1;
                let mut idx = 0usize;
                loop {
                    pos = lex::skip_trivia(bytes, pos, dialect)?;
                    if bytes.get(pos) == Some(&b']') {
                        return Ok(None);
                    }
                    if idx > 0 {
                        if bytes.get(pos) != Some(&b',') {
                            return Ok(None);
                        }
                        pos += 1;
                        pos = lex::skip_trivia(bytes, pos, dialect)?;
                    }
                    if idx == usize::try_from(*index).unwrap_or(usize::MAX) {
                        break;
                    }
                    pos = lex::skip_value(bytes, pos, lex::Check::Locate, max, dialect)?;
                    idx += 1;
                }
            }
        }
    }
    Ok((bytes.get(pos) == Some(&b'[')).then_some(pos))
}

/// What the walk after the first path match found.
enum Shadow {
    /// No later member lands on the path: the first match is the last.
    Clear,
    /// A later member lands on the path: [`resolve_array_start_last`] decides.
    Later,
    /// The walk met a spelling the first-match resolver cannot judge (an
    /// escaped key, a trailing comma, bytes off the member grammar); the eager
    /// walk, which decodes names serially, judges instead.
    Unresolved,
}

/// Walk outward from `end` (just past the value the first path match resolved
/// to) and report whether a later member takes the path.
///
/// Every container on the resolved chain is walked from there to its closer, so
/// the matched array's body is never entered; the cost is the members after the
/// resolved chain, not the chain itself.
fn shadowed_after(bytes: &[u8], steps: &[Step], end: usize, max: u32, dialect: Dialect) -> Shadow {
    walk_for_shadow(bytes, steps, end, lex::Check::Locate, max, dialect).unwrap_or(Shadow::Unresolved)
}

fn walk_for_shadow(
    bytes: &[u8],
    steps: &[Step],
    end: usize,
    check: lex::Check,
    max: u32,
    dialect: Dialect,
) -> Result<Shadow, Error> {
    let mut pos = end;
    for step in steps.iter().rev() {
        match step {
            Step::Key(name) => loop {
                pos = lex::skip_trivia(bytes, pos, dialect)?;
                match bytes.get(pos) {
                    Some(b'}') => {
                        pos += 1;
                        break;
                    }
                    Some(b',') => {
                        pos = lex::skip_trivia(bytes, pos + 1, dialect)?;
                        let key_end = lex::skip_key(bytes, pos, check, dialect)?;
                        let raw = key_text(bytes, pos, key_end);
                        if raw.contains(&b'\\') {
                            return Ok(Shadow::Unresolved);
                        }
                        pos = lex::skip_trivia(bytes, key_end, dialect)?;
                        if bytes.get(pos) != Some(&b':') {
                            return Ok(Shadow::Unresolved);
                        }
                        pos = lex::skip_trivia(bytes, pos + 1, dialect)?;
                        if raw == name.as_bytes() {
                            return Ok(Shadow::Later);
                        }
                        pos = lex::skip_value(bytes, pos, check, max, dialect)?;
                    }
                    _ => return Ok(Shadow::Unresolved),
                }
            },
            Step::Index(_) => loop {
                pos = lex::skip_trivia(bytes, pos, dialect)?;
                match bytes.get(pos) {
                    Some(b']') => {
                        pos += 1;
                        break;
                    }
                    Some(b',') => {
                        pos = lex::skip_trivia(bytes, pos + 1, dialect)?;
                        pos = lex::skip_value(bytes, pos, check, max, dialect)?;
                    }
                    _ => return Ok(Shadow::Unresolved),
                }
            },
        }
    }
    Ok(Shadow::Clear)
}

/// The spelling bytes of one key token, quotes stripped.
fn key_text(bytes: &[u8], start: usize, key_end: usize) -> &[u8] {
    match bytes.get(start) {
        Some(&b'"' | &b'\'') => bytes.get(start + 1..key_end.saturating_sub(1)).unwrap_or(&[]),
        _ => bytes.get(start..key_end).unwrap_or(&[]),
    }
}

/// The first lexical fault the summaries recorded before the array closer, if
/// any: `host_ranges` skips the serial body scan, so it restates the check
/// [`element_ranges`] runs.
fn summary_fault(src: &[u8], summaries: &[CutSummary], close_end: usize) -> Option<Error> {
    for summary in summaries {
        if summary.block.start() >= close_end {
            break;
        }
        if let Some(fault) = summary.scan.fault
            && fault.position(src.len()) < close_end
        {
            return Some(fault_error(src, fault));
        }
    }
    None
}

/// Top-level element spans of an array, scanned from just after `[`.
fn element_ranges(bytes: &[u8], body_start: usize) -> Result<(Vec<ByteRange>, usize), Error> {
    let end = bytes.len();
    let block = ByteRange::try_new(body_start, end).expect("ordered");
    let scan = scan_block(bytes, block, ScanState::Normal);
    let close = scan
        .closes
        .iter()
        .find(|close| i64::from(close.local_depth) == 0)
        .map(|close| close.offset);
    // Raise lexer faults before the closer, so [`Plan::build`] keeps its refusal.
    if let Some(fault) = scan.fault
        && close.is_none_or(|offset| fault.position(end) < offset)
    {
        return Err(fault_error(bytes, fault));
    }
    let mut out = Vec::new();
    let mut start = body_start;
    for comma in &scan.commas {
        if i64::from(comma.local_depth) == 0 && close.is_none_or(|offset| comma.offset < offset) {
            push_element(&mut out, bytes, start, comma.offset);
            start = comma.offset + 1;
        }
    }
    let Some(offset) = close else {
        return Err(error::expected_comma_array(end));
    };
    push_element(&mut out, bytes, start, offset);
    Ok((out, offset + 1))
}

/// Raise a scan fault through the lexer's constructors so code and offset match.
fn fault_error(bytes: &[u8], fault: ScanFault) -> Error {
    match fault {
        ScanFault::Slash(at) => error::invalid_comment(at),
        ScanFault::Block(at) => error::unterminated_comment(at),
        ScanFault::String => error::unterminated_string(bytes.len()),
    }
}

/// The trailing-content check `scan_text` runs, for a split plan: the array
/// closer is known (`close_end`), so the opened containers are closed in O(tail)
/// and the byte after the root value must be trivia to EOF.
fn validate_tail(src: &[u8], req: &ScanRequest<'_>, close_end: usize) -> Result<(), Error> {
    let Some(steps) = req.demands.first().and_then(array_path) else {
        return Ok(());
    };
    let check = crate::scan::check_of(req.strictness);
    let mut pos = close_end;
    for step in steps.iter().rev() {
        pos = match step {
            Step::Key(_) => skip_object_tail(src, pos, check, req.max_nesting, req.dialect)?,
            Step::Index(_) => skip_array_tail(src, pos, check, req.max_nesting, req.dialect)?,
        };
    }
    let tail = lex::skip_trivia(src, pos, req.dialect)?;
    if tail < src.len() {
        return Err(error::trailing_content(tail));
    }
    Ok(())
}

/// The rest of an object after one of its member values: `, member…` then `}`.
/// The key/value walk is the lexer's, so only the comma law is restated here.
fn skip_object_tail(src: &[u8], pos: usize, check: lex::Check, max: u32, dialect: Dialect) -> Result<usize, Error> {
    let mut pos = pos;
    loop {
        pos = lex::skip_trivia(src, pos, dialect)?;
        match src.get(pos) {
            Some(b'}') => return Ok(pos + 1),
            Some(b',') => {
                pos = lex::skip_trivia(src, pos + 1, dialect)?;
                if src.get(pos) == Some(&b'}') {
                    if dialect.trailing_commas() {
                        return Ok(pos + 1);
                    }
                    return Err(error::trailing_comma(pos));
                }
                pos = lex::skip_key(src, pos, check, dialect)?;
                pos = lex::skip_trivia(src, pos, dialect)?;
                if src.get(pos) != Some(&b':') {
                    return Err(error::expected_colon(pos));
                }
                pos = lex::skip_trivia(src, pos + 1, dialect)?;
                pos = lex::skip_value(src, pos, check, max, dialect)?;
            }
            // The walk's object loop reports a missing closer at EOF as an
            // expected key, not a comma.
            None => return Err(error::expected_key(pos)),
            Some(_) => return Err(error::expected_comma_object(pos)),
        }
    }
}

/// The rest of an array after one of its element values: `, value…` then `]`.
fn skip_array_tail(src: &[u8], pos: usize, check: lex::Check, max: u32, dialect: Dialect) -> Result<usize, Error> {
    let mut pos = pos;
    loop {
        pos = lex::skip_trivia(src, pos, dialect)?;
        match src.get(pos) {
            Some(b']') => return Ok(pos + 1),
            Some(b',') => {
                pos = lex::skip_trivia(src, pos + 1, dialect)?;
                if src.get(pos) == Some(&b']') {
                    if dialect.trailing_commas() {
                        return Ok(pos + 1);
                    }
                    return Err(error::trailing_comma(pos));
                }
                pos = lex::skip_value(src, pos, check, max, dialect)?;
            }
            _ => return Err(error::expected_comma_array(pos)),
        }
    }
}

fn push_element(out: &mut Vec<ByteRange>, bytes: &[u8], start: usize, end: usize) {
    let start = lex::ws::skip_ws(bytes, start);
    let mut end = end.min(bytes.len());
    while end > start && matches!(bytes[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
        end -= 1;
    }
    if let Some(range) = ByteRange::try_new(start, end)
        && !range.is_empty()
    {
        out.push(range);
    }
}

fn pack_elements(elements: &[ByteRange], target: usize, close_end: usize) -> Vec<ByteRange> {
    framing::pack_runs(elements, target, true, close_end)
}

#[allow(clippy::too_many_arguments)] // window geometry + request + rows hint + control
fn scan_adjacent_window<'src, const CONTROLLED: bool>(
    src: &'src [u8],
    start: usize,
    end: usize,
    req: &ScanRequest<'_>,
    window: &Window,
    array_body: bool,
    rows_hint: Option<usize>,
    control: &Control,
) -> Result<ScanResult<'src>, Error> {
    // The per-element demand projection is compiled once in `Plan`.
    // A `Sum` demand is answered by the window's value count.
    let sums = &window.sums;
    let mapped = &window.mapped;
    if !sums.iter().any(|&sum| sum) {
        return scan_concat_window::<CONTROLLED>(src, start, end, req, array_body, mapped, rows_hint, control);
    }
    let count = count_window::<CONTROLLED>(src, start, end, req, array_body, control)?;
    if mapped.is_empty() {
        // Every demand is an element count: no per-value work.
        let answers = req
            .demands
            .iter()
            .map(|_| Answer::Oracle(OracleAnswer::Count(count)))
            .collect();
        return Ok(ScanResult::new(answers, Vec::new()));
    }
    let part = scan_concat_window::<CONTROLLED>(src, start, end, req, array_body, mapped, rows_hint, control)?;
    let mut source = part.answers.into_iter();
    let answers = sums
        .iter()
        .map(|&sum| {
            if sum {
                Answer::Oracle(OracleAnswer::Count(count))
            } else {
                source.next().unwrap_or(Answer::Missing)
            }
        })
        .collect();
    Ok(ScanResult::new(answers, part.issues))
}

/// A `Sum` demand's per-range count; the check follows request strictness.
fn count_window<const CONTROLLED: bool>(
    src: &[u8],
    start: usize,
    end: usize,
    req: &ScanRequest<'_>,
    array_body: bool,
    control: &Control,
) -> Result<u64, Error> {
    let check = crate::scan::check_of(req.strictness);
    let mut pos = start;
    let mut count = 0u64;
    let mut first = true;
    while next_window_value(src, &mut pos, end, req.dialect, array_body, &mut first)? {
        if CONTROLLED {
            crate::walk::check_control(control, pos)?;
        }
        pos = lex::skip_value(src, pos, check, req.max_nesting, req.dialect)?;
        count += 1;
    }
    Ok(count)
}

/// Advance `pos` to the next value of a window, or `false` at its end.
fn next_window_value(
    src: &[u8],
    pos: &mut usize,
    end: usize,
    dialect: Dialect,
    array_body: bool,
    first: &mut bool,
) -> Result<bool, Error> {
    if array_body {
        if !crate::walk::run_next_value(src, pos, end, dialect, *first)? {
            return Ok(false);
        }
        *first = false;
    } else {
        *pos = lex::skip_trivia(src, *pos, dialect)?;
        if *pos >= end {
            return Ok(false);
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)] // window geometry + request + projection + control
fn scan_concat_window<'src, const CONTROLLED: bool>(
    src: &'src [u8],
    start: usize,
    end: usize,
    req: &ScanRequest<'_>,
    array_body: bool,
    mapped: &[Demand],
    rows_hint: Option<usize>,
    control: &Control,
) -> Result<ScanResult<'src>, Error> {
    let mut facts = Vec::new();
    collect_facts(src, start, end, req, &mut facts);
    let window = ByteRange::try_new(start, end).expect("ordered");
    // The element-run fast path is uncontrolled; a controlled window takes the
    // per-value route so it can poll.
    if !CONTROLLED
        && array_body
        && let Some(answers) = crate::walk::scan_element_run(
            src,
            window,
            mapped,
            req.strictness,
            req.max_nesting,
            req.dialect,
            &facts,
            rows_hint,
        )?
    {
        return Ok(ScanResult::new(answers, Vec::new()));
    }
    let mut pos = start;
    let mut first = true;
    let mut per_value: Vec<Vec<Answer<'src>>> = Vec::new();
    while next_window_value(src, &mut pos, end, req.dialect, array_body, &mut first)? {
        if CONTROLLED {
            crate::walk::check_control(control, pos)?;
        }
        let is_object = src.get(pos) == Some(&b'{');
        let (mut answers, next, _) = crate::walk::scan_root::<CONTROLLED>(
            src,
            pos,
            mapped,
            req.strictness,
            req.max_nesting,
            req.dialect,
            &facts,
            false,
            control,
        )?;
        apply_row_law(src, mapped, &mut answers, is_object);
        per_value.push(answers);
        pos = next;
    }
    let parts: Vec<ScanResult<'src>> = per_value
        .into_iter()
        .map(|answers| ScanResult::new(answers, Vec::new()))
        .collect();
    let mut result = structury::stitch(parts);
    finish_row_law(src, mapped, &mut result);
    Ok(result)
}

/// Element row law for a window value that is not an object.
/// A projected demand contributes one absent row. A filtered demand
/// contributes nothing.
fn apply_row_law<'src>(src: &'src [u8], mapped: &[Demand], answers: &mut [Answer<'src>], is_object: bool) {
    if is_object {
        return;
    }
    for (demand, answer) in mapped.iter().zip(answers.iter_mut()) {
        match demand {
            Demand::Project { path, fields } if path.steps.is_empty() => {
                *answer = absent_row(src, fields);
            }
            Demand::Filter { path, .. } if path.steps.is_empty() => {
                *answer = Answer::Missing;
            }
            _ => {}
        }
    }
}

/// Window half of the element law. Parts that stayed `Missing` answer the empty batch.
fn finish_row_law<'src>(src: &'src [u8], mapped: &[Demand], result: &mut ScanResult<'src>) {
    if result.answers.is_empty() {
        result.answers = mapped
            .iter()
            .map(|demand| empty_row(src, demand).unwrap_or(Answer::Missing))
            .collect();
        return;
    }
    for (demand, answer) in mapped.iter().zip(result.answers.iter_mut()) {
        if matches!(answer, Answer::Missing)
            && let Some(empty) = empty_row(src, demand)
        {
            *answer = empty;
        }
    }
}

/// One absent projection row over `fields`.
fn absent_row<'src>(src: &'src [u8], fields: &[alloc::string::String]) -> Answer<'src> {
    let mut columns = Columns::new(src, fields.to_vec());
    for _ in fields {
        columns.push(ColumnCell::Absent);
    }
    Answer::Columns(columns)
}

/// The empty row batch a window answers when a row view found nothing.
fn empty_row<'src>(src: &'src [u8], demand: &Demand) -> Option<Answer<'src>> {
    match demand {
        Demand::Project { path, fields } if path.steps.is_empty() => {
            Some(Answer::Columns(Columns::new(src, fields.clone())))
        }
        Demand::Filter { path, project, .. } if path.steps.is_empty() => {
            let fields = if project.is_empty() {
                alloc::vec![alloc::string::String::from("$")]
            } else {
                project.clone()
            };
            Some(Answer::Columns(Columns::new(src, fields)))
        }
        _ => None,
    }
}
