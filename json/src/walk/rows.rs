use super::{
    Answer, ByteRange, ColumnCell, Columns, Demand, Dialect, Document, Hit, Members, Name, RecordSink,
    ResolvedPredicate, Scan, String, TraceSink, Vec, View, Walker, flatten_here, pred_on_members,
};

impl<'src, S: Scan, R: RecordSink, const CONTROLLED: bool, T: TraceSink> Walker<'src, '_, '_, S, CONTROLLED, R, T> {
    /// Begin a row-accumulating element loop: move each row hit's accumulator
    /// onto `rows` and seed its slot for the element's own answer.
    pub(super) fn open_rows(&mut self, hits: &[Hit<'_>], marks: &mut [Answer<'src>]) -> usize {
        let base = self.rows.len();
        for h in hits {
            let Some(shape) = row_shape(h.view) else {
                continue;
            };
            let acc = core::mem::replace(&mut marks[h.idx], Answer::Missing);
            self.rows.push(match acc {
                Answer::Missing => Answer::Columns(Columns::new(self.bytes, row_fields(shape))),
                other => other,
            });
            // A projected/filtered row appends into a reusable element batch; a
            // whole-element row keeps the element's own answer. Reserve the one
            // row up front so the `push` growth floor does not dominate the peak.
            if !matches!(shape, RowShape::Whole) {
                let mut buffer = Columns::new(self.bytes, row_fields(shape));
                buffer.reserve(row_width(shape));
                marks[h.idx] = Answer::Columns(buffer);
            }
        }
        base
    }

    /// Fold one element's own answer into its accumulator by the row law, then
    /// leave the element slot ready for the next element. This is the only place
    /// a collection row is created.
    pub(super) fn fold_row(
        &mut self,
        base: usize,
        hits: &[Hit<'_>],
        marks: &mut [Answer<'src>],
        source: &'src [u8],
        span: ByteRange,
    ) {
        let mut slot = base;
        for h in hits {
            let Some(shape) = row_shape(h.view) else {
                continue;
            };
            let acc = &mut self.rows[slot];
            slot += 1;
            let elem = core::mem::replace(&mut marks[h.idx], Answer::Missing);
            match shape {
                RowShape::Projected(fields) => match elem {
                    Answer::Columns(mut buf) => {
                        if buf.cells().is_empty() {
                            push_absent_row(acc, fields, source);
                        } else {
                            absorb_cells(acc, &mut buf);
                        }
                        marks[h.idx] = Answer::Columns(buf);
                    }
                    _ => push_absent_row(acc, fields, source),
                },
                RowShape::Filtered(_) => {
                    if let Answer::Columns(mut buf) = elem {
                        absorb_cells(acc, &mut buf);
                        marks[h.idx] = Answer::Columns(buf);
                    }
                }
                RowShape::Whole => match elem {
                    Answer::Document(d) => push_span_row(acc, d.source(), d.root()),
                    Answer::Columns(mut buf) => {
                        // The element's batch may be a nested collection with
                        // its own field set; while the accumulator is still
                        // empty it adopts that field set, so a multi-cell inner
                        // row stays one row instead of being reinterpreted as
                        // several one-cell rows.
                        if let Answer::Columns(columns) = acc
                            && columns.rows() == 0
                            && columns.fields() != buf.fields()
                        {
                            *columns = Columns::new(buf.source(), buf.fields().to_vec());
                        }
                        absorb_cells(acc, &mut buf);
                    }
                    Answer::Missing => push_span_row(acc, source, span),
                    _ => {}
                },
            }
        }
    }

    /// End a row-accumulating element loop: write each accumulator back to its
    /// mark and pop the level, before `finish_array` windows the batch.
    pub(super) fn close_rows(&mut self, base: usize, hits: &[Hit<'_>], marks: &mut [Answer<'src>]) {
        let mut slot = base;
        for h in hits {
            if row_shape(h.view).is_none() {
                continue;
            }
            marks[h.idx] = core::mem::replace(&mut self.rows[slot], Answer::Missing);
            slot += 1;
        }
        self.rows.truncate(base);
    }
}

/// Fold one located value into a collection accumulator: columns append the
/// span, a bare slot takes the document.
pub(super) fn assign_document<'a>(mark: &mut Answer<'a>, doc: Document<'a>) {
    match mark {
        Answer::Columns(columns) => columns.push(ColumnCell::Span(doc.root())),
        _ => *mark = Answer::Document(doc),
    }
}

/// Append a whole batch's cells to an accumulator, leaving `other` empty with its
/// capacity retained for the next element; both name the same source.
pub(super) fn absorb_cells<'a>(acc: &mut Answer<'a>, other: &mut Columns<'a>) {
    if let Answer::Columns(columns) = acc {
        columns.absorb(other);
    } else {
        let mut columns = Columns::new(other.source(), other.fields().to_vec());
        columns.absorb(other);
        *acc = Answer::Columns(columns);
    }
}

fn push_span_row<'a>(acc: &mut Answer<'a>, source: &'a [u8], span: ByteRange) {
    if let Answer::Columns(columns) = acc {
        columns.push(ColumnCell::Span(span));
    } else {
        let mut columns = Columns::new(source, alloc::vec![alloc::string::String::from("$")]);
        columns.push(ColumnCell::Span(span));
        *acc = Answer::Columns(columns);
    }
}

#[allow(clippy::too_many_arguments, reason = "the walk's inputs are all independent")]
pub(super) fn apply_object_row<'a>(
    acc: &mut Answer<'a>,
    view: View<'_>,
    resolved: Option<&ResolvedPredicate<'_>>,
    members: &Members,
    bytes: &'a [u8],
    span: ByteRange,
    dialect: Dialect,
    scratch: &mut String,
) {
    match view {
        View::Project { path: [], fields } => {
            push_project_row(acc, fields, members, bytes);
        }
        View::Filter {
            path: [],
            predicate,
            project,
        } => {
            // The row loop resolved this predicate once; a lone object (no row
            // loop) resolves on the spot, which is not a per-row cost.
            let owned;
            let predicate = if let Some(resolved) = resolved {
                resolved
            } else {
                owned = ResolvedPredicate::resolve(predicate);
                &owned
            };
            if pred_on_members(bytes, members, predicate, dialect, scratch) {
                if project.is_empty() {
                    push_span_row(acc, bytes, span);
                } else {
                    push_project_row(acc, project, members, bytes);
                }
            } else if !matches!(acc, Answer::Columns(_)) {
                *acc = Answer::Columns(Columns::new(
                    bytes,
                    if project.is_empty() {
                        alloc::vec![alloc::string::String::from("$")]
                    } else {
                        project.to_vec()
                    },
                ));
            }
        }
        _ => {}
    }
}

fn push_project_row<'a>(acc: &mut Answer<'a>, fields: &[Name], members: &Members, bytes: &'a [u8]) {
    let push = |columns: &mut Columns<'_>| {
        for name in fields {
            if let Some(rng) = members.get(bytes, name.as_bytes()) {
                columns.push(ColumnCell::Span(rng));
            } else {
                columns.push(ColumnCell::Absent);
            }
        }
    };
    if let Answer::Columns(columns) = acc {
        push(columns);
    } else {
        let mut columns = Columns::new(bytes, fields.to_vec());
        push(&mut columns);
        *acc = Answer::Columns(columns);
    }
}

/// Append an all-`Absent` projection row. An element whose demanded fields are
/// all missing still contributes one row (`{}`), matching the flat `Project`.
pub(super) fn push_absent_row<'a>(acc: &mut Answer<'a>, fields: &[Name], source: &'a [u8]) {
    if let Answer::Columns(columns) = acc {
        for _ in fields {
            columns.push(ColumnCell::Absent);
        }
    } else {
        let mut columns = Columns::new(source, fields.to_vec());
        for _ in fields {
            columns.push(ColumnCell::Absent);
        }
        *acc = Answer::Columns(columns);
    }
}

/// The row a collection element contributes, keyed by the demand shape. This is
/// the single "one row per element" law: how many rows an element contributes
/// and what a non-row element answer means.
#[derive(Clone, Copy)]
pub(crate) enum RowShape<'a> {
    /// Exactly one row per element; a non-object / absent-spine element (or any
    /// other terminal element answer) contributes an absent row.
    Projected(&'a [Name]),
    /// Zero or one row per element; only an element answer that produced cells
    /// contributes, so a non-matching element contributes nothing.
    Filtered(&'a [Name]),
    /// The element's own answer is the row (a `Whole`/`Path` element, a nested
    /// batch, or a windowed slice).
    Whole,
}

/// The row shape `view` accumulates. `None` for a view that is not a row
/// accumulator (a path-scoped `Project`/`Filter`, an oracle, a `Whole`, …).
pub(super) fn row_shape(view: View<'_>) -> Option<RowShape<'_>> {
    fn shape_of(nested: Option<&Demand>) -> RowShape<'_> {
        // Unwrap a spent `Path{[]}` to its nested demand, as
        // `enter_array`/`flatten_here` do.
        match flatten_view(View::from_nested(nested)) {
            View::Project { fields, .. } => RowShape::Projected(fields),
            View::Filter { project, .. } => RowShape::Filtered(project),
            _ => RowShape::Whole,
        }
    }
    match view {
        View::Project { path: [], fields } => Some(RowShape::Projected(fields)),
        View::Filter { path: [], project, .. } => Some(RowShape::Filtered(project)),
        View::Collection {
            keys: Some(keys),
            nested: None,
        } => Some(RowShape::Projected(keys)),
        View::Collection { nested, .. } | View::Slice { nested, .. } => Some(shape_of(nested)),
        _ => None,
    }
}

/// The field set of a row shape's batch: a filter's empty projection and every
/// whole-element row are the synthetic `$` column.
pub(crate) fn row_fields(shape: RowShape<'_>) -> Vec<String> {
    match shape {
        RowShape::Projected(fields) => fields.to_vec(),
        RowShape::Filtered(project) if !project.is_empty() => project.to_vec(),
        RowShape::Filtered(_) | RowShape::Whole => alloc::vec![alloc::string::String::from("$")],
    }
}

fn row_width(shape: RowShape<'_>) -> usize {
    match shape {
        RowShape::Projected(fields) => fields.len(),
        RowShape::Filtered(project) => project.len().max(1),
        RowShape::Whole => 1,
    }
}

/// The owned row law; the borrowed form is [`RowShape`]. The stream plan stores
/// this because it owns the cloned per-record demands.
#[derive(Clone)]
pub(crate) enum RowLaw {
    Projected(Vec<String>),
    Filtered(Vec<String>),
    Whole,
}

impl RowLaw {
    fn from_shape(shape: RowShape<'_>) -> Self {
        match shape {
            RowShape::Projected(fields) => Self::Projected(fields.to_vec()),
            RowShape::Filtered(project) => Self::Filtered(project.to_vec()),
            RowShape::Whole => Self::Whole,
        }
    }

    pub(crate) fn as_shape(&self) -> RowShape<'_> {
        match self {
            Self::Projected(fields) => RowShape::Projected(fields),
            Self::Filtered(project) => RowShape::Filtered(project),
            Self::Whole => RowShape::Whole,
        }
    }
}

/// The virtual-array row law of a stream plan's every-record demand: the
/// array-level view's law, or the element-position law for a bare spine.
pub(crate) fn stream_every_law(demand: &Demand, element: &Demand) -> Option<RowLaw> {
    match flatten_view(View::from_demand(demand)) {
        View::Whole => Some(RowLaw::Whole),
        other => row_shape(other)
            .map(RowLaw::from_shape)
            .or_else(|| element_row_law(element)),
    }
}

/// The law of a record in element position: a projection/filter contributes one
/// row (or none); a whole/path element is the record's own answer.
pub(crate) fn element_row_law(demand: &Demand) -> Option<RowLaw> {
    match flatten_view(View::from_demand(demand)) {
        View::Project { fields, .. } => Some(RowLaw::Projected(fields.to_vec())),
        View::Filter { project, .. } => Some(RowLaw::Filtered(project.to_vec())),
        _ => None,
    }
}

pub(super) fn flatten_view(view: View<'_>) -> View<'_> {
    flatten_here(Hit { idx: 0, view }).view
}

/// The one-row answer for a flat projection over a single selected element:
/// one absent row (`Project`), none (`Filter`).
pub(super) fn flat_index_row<'a>(bytes: &'a [u8], view: View<'_>) -> Answer<'a> {
    match row_shape(view) {
        Some(RowShape::Projected(fields)) => {
            let mut batch = Answer::Missing;
            push_absent_row(&mut batch, fields, bytes);
            batch
        }
        Some(shape @ RowShape::Filtered(_)) => Answer::Columns(Columns::new(bytes, row_fields(shape))),
        _ => Answer::Missing,
    }
}
