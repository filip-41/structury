use super::{
    Answer, ByteRange, ColumnCell, Columns, Control, Demand, Dialect, DialectScan, Fact, Hit, NO_CONTROL, NoRec,
    NoTrace, RecordSink, RfcScan, RowLaw, RowShape, Scan, SmallErr, Strictness, String, Trace, TraceSink, Vec, Walker,
    Wanted, error, flatten_here, hits_for, push_absent_row, row_fields, row_shape,
};

/// One walk over a run of element values, as the body of an array with no
/// brackets. `None` when a demand is not a root Project/Filter.
#[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
pub(crate) fn scan_element_run<'src>(
    bytes: &'src [u8],
    range: ByteRange,
    demands: &[Demand],
    strictness: Strictness,
    max: u32,
    dialect: Dialect,
    facts: &[Fact<'src>],
    rows_hint: Option<usize>,
) -> Result<Option<Vec<Answer<'src>>>, SmallErr> {
    if dialect == Dialect::Rfc8259 {
        scan_element_run_with::<RfcScan>(bytes, range, demands, strictness, max, dialect, facts, rows_hint)
    } else {
        scan_element_run_with::<DialectScan>(bytes, range, demands, strictness, max, dialect, facts, rows_hint)
    }
}

#[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
fn scan_element_run_with<'src, S: Scan>(
    bytes: &'src [u8],
    range: ByteRange,
    demands: &[Demand],
    strictness: Strictness,
    max: u32,
    dialect: Dialect,
    facts: &[Fact<'src>],
    rows_hint: Option<usize>,
) -> Result<Option<Vec<Answer<'src>>>, SmallErr> {
    if !demands.iter().all(is_row_view) {
        return Ok(None);
    }
    let retained = matches!(strictness, Strictness::Strict);
    let mut walker = Walker::<S, false, NoRec>::new(
        bytes,
        range.start(),
        strictness,
        max,
        dialect,
        facts,
        retained,
        &NO_CONTROL,
        NoTrace,
    )?;
    let mut marks = alloc::vec![Answer::Missing; demands.len()];
    let hits = hits_for(demands);
    if let Some(rows) = rows_hint {
        // A projected row per element accumulates in one batch; reserving once
        // keeps the walk from regrowing it per row. A filtered batch stays
        // `Missing` until it matches, so a sparse filter reserves nothing.
        for h in &hits {
            if let Some(RowShape::Projected(fields)) = row_shape(h.view) {
                let mut columns = Columns::new(bytes, fields.to_vec());
                columns.reserve_rows(rows);
                marks[h.idx] = Answer::Columns(columns);
            }
        }
    }
    walker.walk_element_run(range, &hits, &mut marks)?;
    Ok(Some(marks))
}

fn is_row_view(demand: &Demand) -> bool {
    matches!(
        demand,
        Demand::Project { path, .. } | Demand::Filter { path, .. } if path.steps.is_empty()
    )
}

/// Persistent per-record walk state for a framed stream, one consumer per
/// [`Scan`] strategy. Allocations scale with the demand set, not the record
/// count.
pub(crate) enum StreamWalker<'src, 'c, 'd, 'f, const CONTROLLED: bool, T: TraceSink = NoTrace> {
    /// RFC 8259 byte-level strategy.
    Rfc(StreamState<'src, 'c, 'd, 'f, RfcScan, CONTROLLED, T>),
    /// Dialect (lexer) strategy.
    Dialect(StreamState<'src, 'c, 'd, 'f, DialectScan, CONTROLLED, T>),
}

/// The per-strategy state behind [`StreamWalker`].
pub(crate) struct StreamState<'src, 'c, 'd, 'f, S, const CONTROLLED: bool, T: TraceSink = NoTrace> {
    walker: Walker<'src, 'c, 'f, S, CONTROLLED, NoRec, T>,
    hits: Vec<Hit<'d>>,
    active_hits: Vec<Hit<'d>>,
    /// The one row law per per-record demand, derived from the array-level
    /// demand the record mapping came from (not the unwrapped element demand).
    laws: Vec<Option<RowLaw>>,
    wanted: Wanted<'d>,
    owners: Vec<usize>,
    marks: Vec<Answer<'src>>,
}

impl<'src, 'c, 'd, 'f, const CONTROLLED: bool, T: TraceSink> StreamWalker<'src, 'c, 'd, 'f, CONTROLLED, T> {
    #[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
    pub(crate) fn new(
        bytes: &'src [u8],
        demands: &'d [Demand],
        laws: Vec<Option<RowLaw>>,
        owners: Vec<usize>,
        strictness: Strictness,
        max: u32,
        dialect: Dialect,
        facts: &'f [Fact<'src>],
        control: &'c Control,
        trace: T,
    ) -> Result<Self, SmallErr> {
        let hits = hits_for(demands);
        // `walk_object`/`keep_array` flatten hits before matching members, so the
        // cached key set is the flattened one.
        let flat: Vec<Hit<'d>> = hits.iter().copied().map(flatten_here).collect();
        let wanted = Wanted::gather(&flat);
        let retained = matches!(strictness, Strictness::Strict);
        if dialect == Dialect::Rfc8259 {
            Ok(Self::Rfc(StreamState::new(
                bytes, hits, laws, wanted, owners, strictness, max, dialect, facts, retained, control, trace,
            )?))
        } else {
            Ok(Self::Dialect(StreamState::new(
                bytes, hits, laws, wanted, owners, strictness, max, dialect, facts, retained, control, trace,
            )?))
        }
    }

    pub(crate) fn record(
        &mut self,
        start: usize,
        index: usize,
        windows: &[Option<core::ops::Range<usize>>],
    ) -> Result<(), SmallErr> {
        match self {
            Self::Rfc(state) => state.record(start, index, windows),
            Self::Dialect(state) => state.record(start, index, windows),
        }
    }

    pub(crate) fn fold(&mut self, acc: &mut [Answer<'src>]) {
        match self {
            Self::Rfc(state) => state.fold(acc),
            Self::Dialect(state) => state.fold(acc),
        }
    }

    /// Drain this walker's trace (stream extras merge it into the main trace).
    pub(crate) fn take_trace(&mut self) -> Trace {
        match self {
            Self::Rfc(state) => state.walker.trace.take_trace(),
            Self::Dialect(state) => state.walker.trace.take_trace(),
        }
    }
}

impl<'src, 'c, 'd, 'f, S: Scan, const CONTROLLED: bool, T: TraceSink> StreamState<'src, 'c, 'd, 'f, S, CONTROLLED, T> {
    #[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
    fn new(
        bytes: &'src [u8],
        hits: Vec<Hit<'d>>,
        laws: Vec<Option<RowLaw>>,
        wanted: Wanted<'d>,
        owners: Vec<usize>,
        strictness: Strictness,
        max: u32,
        dialect: Dialect,
        facts: &'f [Fact<'src>],
        retained: bool,
        control: &'c Control,
        trace: T,
    ) -> Result<Self, SmallErr> {
        let walker = Walker::<S, CONTROLLED, NoRec, T>::new(
            bytes, 0, strictness, max, dialect, facts, retained, control, trace,
        )?;
        let marks = laws.iter().map(|law| seed_mark(bytes, law.as_ref())).collect();
        Ok(Self {
            walker,
            active_hits: Vec::with_capacity(hits.len()),
            hits,
            laws,
            wanted,
            owners,
            marks,
        })
    }

    fn record(
        &mut self,
        start: usize,
        index: usize,
        windows: &[Option<core::ops::Range<usize>>],
    ) -> Result<(), SmallErr> {
        self.active_hits.clear();
        self.active_hits.extend(self.hits.iter().copied().filter(|hit| {
            windows[self.owners[hit.idx]]
                .as_ref()
                .is_none_or(|window| window.contains(&index))
        }));
        let bytes = self.walker.bytes;
        // Reseed each active row buffer: a record walk may replace the seed
        // (a scalar dead-end, a stale answer), and the next record must start
        // from the same empty batch the walk expects.
        for hit in &self.active_hits {
            let seeded = matches!(&self.marks[hit.idx], Answer::Columns(c) if c.cells().is_empty());
            if !seeded {
                self.marks[hit.idx] = seed_mark(bytes, self.laws[hit.idx].as_ref());
            }
        }
        let dialect = self.walker.dialect;
        self.walker.pos = S::skip_trivia(bytes, start, dialect)?;
        self.walker.depth = 0;
        self.walker.stop = false;
        self.walker.replaying = false;
        // A record is one element of the virtual array, not an array to map:
        // mark it as an element so a flat projection over an array record is
        // one absent row, exactly as it is on a text array.
        self.walker.repeat_depth = 1;
        self.walker.allow_stop = false;
        if self.walker.pos >= bytes.len() {
            return Err(error::expected_value(self.walker.pos));
        }
        self.walker.poll(self.walker.pos)?;
        let wanted = (self.active_hits.len() == self.hits.len()).then_some(&self.wanted);
        self.walker.walk_node(&self.active_hits, wanted, &mut self.marks)
    }

    fn fold(&mut self, acc: &mut [Answer<'src>]) {
        let bytes = self.walker.bytes;
        for hit in &self.active_hits {
            let k = hit.idx;
            let slot = self.owners[k];
            // One row per element: a projected record that produced no cells is
            // an absent row, exactly as the same demand answers on a text array.
            if let Some(RowLaw::Projected(fields)) = self.laws[k].as_ref()
                && let Answer::Columns(c) = &self.marks[k]
                && c.cells().is_empty()
            {
                push_absent_row(&mut acc[slot], fields, bytes);
                continue;
            }
            fold_stream_mark(&mut acc[slot], &mut self.marks[k]);
        }
    }
}

/// The accumulator a row slot starts each record with: the exact `Columns` the
/// walk would have built on its first row, so an object record appends rather
/// than allocating. No law leaves `Missing` for a non-row demand.
fn seed_mark<'src>(bytes: &'src [u8], law: Option<&RowLaw>) -> Answer<'src> {
    match law {
        Some(law) => Answer::Columns(Columns::new(bytes, row_fields(law.as_shape()))),
        None => Answer::Missing,
    }
}

/// Fold one record's walk answer into the stream accumulator, leaving
/// `incoming` reusable for the next record. A `Columns` batch keeps its field
/// set and cell capacity.
pub(crate) fn fold_stream_mark<'src>(slot: &mut Answer<'src>, incoming: &mut Answer<'src>) {
    let next = core::mem::replace(incoming, Answer::Missing);
    match (core::mem::replace(slot, Answer::Missing), next) {
        (Answer::Missing, Answer::Columns(mut next)) => {
            let mut acc = Columns::new(next.source(), next.fields().to_vec());
            acc.absorb(&mut next);
            *slot = Answer::Columns(acc);
            *incoming = Answer::Columns(next);
        }
        (Answer::Missing, next) => *slot = next,
        (Answer::Columns(mut acc), Answer::Columns(mut next)) => {
            acc.absorb(&mut next);
            *slot = Answer::Columns(acc);
            *incoming = Answer::Columns(next);
        }
        (Answer::Columns(mut acc), next) => {
            if let Answer::Document(d) = &next {
                acc.push(ColumnCell::Span(d.root()));
            }
            *slot = Answer::Columns(acc);
        }
        (Answer::Document(d), Answer::Columns(mut next)) => {
            let mut acc = Columns::new(d.source(), alloc::vec![String::from("$")]);
            acc.push(ColumnCell::Span(d.root()));
            acc.absorb(&mut next);
            *slot = Answer::Columns(acc);
            *incoming = Answer::Columns(next);
        }
        (_prev, Answer::Columns(next)) => *slot = Answer::Columns(next),
        (Answer::Document(a), Answer::Document(b)) => {
            let mut acc = Columns::new(a.source(), alloc::vec![String::from("$")]);
            acc.push(ColumnCell::Span(a.root()));
            acc.push(ColumnCell::Span(b.root()));
            *slot = Answer::Columns(acc);
        }
        (prev, _next) => *slot = prev,
    }
}

/// Comma law for an element run: comma between values; RFC trailing comma
/// before `]` is an error; comma then end-of-window separates non-final packs.
pub(crate) fn run_next_value(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
    dialect: Dialect,
    first: bool,
) -> Result<bool, SmallErr> {
    if dialect == Dialect::Rfc8259 {
        next_run_value::<RfcScan>(bytes, pos, end, dialect, first)
    } else {
        next_run_value::<DialectScan>(bytes, pos, end, dialect, first)
    }
}

pub(super) fn next_run_value<S: Scan>(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
    dialect: Dialect,
    first: bool,
) -> Result<bool, SmallErr> {
    *pos = S::skip_trivia(bytes, *pos, dialect)?;
    if *pos >= end {
        return Ok(false);
    }
    let byte = bytes[*pos];
    if byte == b']' {
        *pos += 1;
        return Ok(false);
    }
    if !first {
        if byte != b',' {
            return Err(error::expected_comma_array(*pos));
        }
        *pos += 1;
        *pos = S::skip_trivia(bytes, *pos, dialect)?;
        if *pos >= end {
            return Ok(false);
        }
        if bytes.get(*pos) == Some(&b']') {
            if dialect.trailing_commas() {
                *pos += 1;
                return Ok(false);
            }
            return Err(error::trailing_comma(*pos));
        }
    }
    Ok(true)
}

pub(super) fn replay_element<'src, S: Scan, R: RecordSink, const CONTROLLED: bool, T: TraceSink>(
    walker: &mut Walker<'src, '_, '_, S, CONTROLLED, R, T>,
    span: ByteRange,
    hit: Hit<'_>,
    marks: &mut [Answer<'src>],
) -> Result<(), SmallErr> {
    let saved = walker.pos;
    let replaying = walker.replaying;
    walker.replaying = true;
    walker.pos = span.start();
    let result = walker.walk_node(&[hit], None, marks);
    walker.pos = saved;
    walker.replaying = replaying;
    result
}
