//! Span edit: set, insert, delete, replace-member, and clear at a path, plus
//! comment fact writes. One Strict scan locates every edit. An edit the splice
//! cannot place falls back to a DOM re-encode.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use structury::{Answer, ByteRange, Demand, Document, Fact, FactOwner, FactRole, Step, Strictness, Value};

use crate::dialect::Dialect;
use crate::encode::EncodeOptions;
use crate::encode::Source;
use crate::error;
use crate::lex::MAX_NESTING;
use crate::materialize::{Form, MaterializeOptions, Materialized, parse};
use crate::scan::{JsonInput, ScanRequest, scan};

/// One splice against retained source.
#[derive(Clone, Debug)]
pub enum Edit {
    /// Replace the value at `path`.
    Set {
        /// Path to the existing value.
        path: Vec<Step>,
        /// Replacement.
        value: Value,
    },
    /// Insert `value` at `path` (array index or new object member). An object
    /// key that already exists replaces that member's value.
    Insert {
        /// Path whose last step is the insert slot.
        path: Vec<Step>,
        /// Inserted value.
        value: Value,
    },
    /// Delete the value at `path`.
    Delete {
        /// Path to remove.
        path: Vec<Step>,
    },
    /// Replace a member's key and value at `path`; it keeps its position and glue.
    ReplaceMember {
        /// Path whose last step names the member.
        path: Vec<Step>,
        /// Replacement key.
        key: String,
        /// Replacement value.
        value: Value,
    },
    /// Clear the container at `path`: remove every member or element, leaving
    /// an empty object or array in place. A non-container is refused.
    Clear {
        /// Path to the container.
        path: Vec<Step>,
    },
    /// Write a comment fact on the node at `path`. Only a commenting dialect
    /// ([`Dialect::Jsonc`], [`Dialect::Json5`]) has facts to write; under
    /// [`Dialect::Rfc8259`] every [`FactOp`] is a no-op.
    Fact {
        /// Path to the owner node.
        path: Vec<Step>,
        /// Which comment to write.
        op: FactOp,
    },
}

/// A comment write on one node. `Replace` and `Clear` affect every fact of the role.
#[derive(Clone, Debug)]
pub enum FactOp {
    /// Attach a new comment of `role`. Empty text is a no-op.
    Insert {
        /// Attachment role.
        role: FactRole,
        /// Comment body, without delimiters.
        text: String,
    },
    /// Rewrite the comment of `role` in place. Empty text clears it.
    Replace {
        /// Attachment role.
        role: FactRole,
        /// New comment body, without delimiters.
        text: String,
    },
    /// Remove the comment of `role`.
    Clear {
        /// Attachment role.
        role: FactRole,
    },
}

/// Request: options for [`edit`] and [`edit_document`] — the grammar the input
/// is read under and the output is validated under.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct EditOptions {
    /// Grammar the input is read under and the output is re-validated under.
    pub dialect: Dialect,
}

impl EditOptions {
    /// These options reading `dialect`.
    #[must_use]
    pub const fn new(dialect: Dialect) -> Self {
        Self { dialect }
    }

    /// These options reading `dialect`.
    #[must_use]
    pub const fn with_dialect(mut self, dialect: Dialect) -> Self {
        self.dialect = dialect;
        self
    }
}

/// Ops: apply `edits` to `src`, re-validating under `opts.dialect`. Under
/// [`Dialect::Rfc8259`], [`Edit::Delete`] of an absent path is a no-op and a
/// [`Edit::Fact`] write is inert; `Set`/`Insert` error when their path is absent.
///
/// # Errors
///
/// Invalid input, an absent `Set`/`Insert` path, or output that fails Strict.
pub fn edit(src: &[u8], edits: &[Edit], opts: EditOptions) -> Result<Vec<u8>, structury::Error> {
    let dialect = opts.dialect;
    let plan = match plan_edits(src, edits, dialect, false) {
        Ok(plan) => plan,
        Err(error) if fallback_able(&error) => return fallback_encode(src, edits, dialect, error),
        Err(error) => return Err(error),
    };
    if plan.splices.is_empty() {
        return Ok(src.to_vec());
    }
    let mut buf = Vec::new();
    plan.write_into(src, &mut buf);
    if plan.needs_validation
        && let Err(error) = validated_document(&buf, dialect, false)
    {
        return fallback_encode(src, edits, dialect, error);
    }
    Ok(buf)
}

/// Ops: apply `edits` and keep the spliced bytes as a [`Document`] over `out`
/// (cleared first); a fact glyph straddling a splice is dropped, never cut.
///
/// # Errors
///
/// Same as [`edit`].
pub fn edit_document<'out>(
    src: &[u8],
    edits: &[Edit],
    opts: EditOptions,
    out: &'out mut Vec<u8>,
) -> Result<Document<'out>, structury::Error> {
    let dialect = opts.dialect;
    let plan = match plan_edits(src, edits, dialect, true) {
        Ok(plan) => plan,
        Err(error) if fallback_able(&error) => return fallback_document(src, edits, dialect, out, error),
        Err(error) => return Err(error),
    };
    let facts = plan.remapped_facts();
    plan.write_into(src, out);
    let root = match validated_document(out, dialect, false) {
        Ok(doc) => doc.root(),
        Err(error) => return fallback_document(src, edits, dialect, out, error),
    };
    let bytes: &'out [u8] = out;
    let mut document = Document::from_span_validated(bytes, root, dialect.grammar_tag());
    document.set_facts(facts)?;
    Ok(document)
}

/// One splice against retained source.
struct Splice {
    /// Position in `edits`, the tie-break for an equal `from`.
    index: usize,
    from: usize,
    to: usize,
    bytes: Vec<u8>,
    /// A comment write this splice carries: the new fact and the glyph's byte
    /// range inside `bytes`.
    fact: Option<NewFact>,
}

impl Splice {
    /// Signed length change this splice applies to following offsets.
    fn shift(&self) -> isize {
        let inserted = isize::try_from(self.bytes.len()).unwrap_or(isize::MAX);
        let removed = isize::try_from(self.to - self.from).unwrap_or(isize::MAX);
        inserted.saturating_sub(removed)
    }
}

/// A fact a splice creates; its glyph is the splice's inserted bytes.
struct NewFact {
    role: FactRole,
    text: String,
    owner: FactOwner,
    /// Glyph byte range inside the splice's `bytes` (leading trivia and the
    /// terminating newline are not part of the comment token).
    glyph: (usize, usize),
}

/// The located splices, in descending `from` order, plus the input facts the plan rewrites.
struct Plan<'src> {
    facts: Vec<Fact<'src>>,
    splices: Vec<Splice>,
    /// Whether the written output still has to be Strict-validated. Each splice
    /// is proven well formed by construction under RFC 8259; a commenting
    /// dialect's trailing commas or an insert into a non-object parent can
    /// still place a comma against a closer, so those plans keep the check.
    needs_validation: bool,
}

impl Plan<'_> {
    fn write_into(&self, src: &[u8], out: &mut Vec<u8>) {
        out.clear();
        // Reserve the whole output up front so a splice never reallocates and copies the tail.
        let growth: usize = self
            .splices
            .iter()
            .map(|splice| splice.bytes.len().saturating_sub(splice.to - splice.from))
            .sum();
        out.reserve(src.len().saturating_add(growth));
        out.extend_from_slice(src);
        for splice in &self.splices {
            out.splice(splice.from..splice.to, splice.bytes.iter().copied());
        }
    }

    /// Facts adjusted for every splice, ascending; a write's record lands on its
    /// splice's bytes. Every fact's text is materialized owned, since the output
    /// document borrows `out` rather than the plan's input.
    fn remapped_facts(&self) -> Vec<Fact<'static>> {
        let mut facts: Vec<Fact<'static>> = self.facts.iter().map(owned_fact).collect();
        let mut delta: isize = 0;
        // Ascending: each splice's original bounds translate by the deltas
        // already applied before it.
        for splice in self.splices.iter().rev() {
            let from = splice.from.wrapping_add_signed(delta);
            let to = splice.to.wrapping_add_signed(delta);
            facts = remap_one(&facts, from, to, splice.shift());
            delta = delta.saturating_add(splice.shift());
        }
        // A write's glyph is its splice's inserted bytes, whose output start is
        // the splice `from` shifted by every earlier splice. Those spans are in
        // output space, so they are appended after every remap, never through it.
        let mut out_delta: isize = 0;
        for splice in self.splices.iter().rev() {
            let out_from = splice.from.wrapping_add_signed(out_delta);
            if let Some(fresh) = &splice.fact {
                let start = out_from.saturating_add(fresh.glyph.0);
                let end = out_from.saturating_add(fresh.glyph.1);
                if let Some(span) = ByteRange::try_new(start, end) {
                    facts.push(Fact::new(
                        fresh.role,
                        Cow::Owned(String::from(fresh.text.as_str())),
                        Some(span),
                        FactOwner::node(remap_span(fresh.owner.span(), &self.splices)),
                    ));
                }
            }
            out_delta = out_delta.saturating_add(splice.shift());
        }
        // A write appends its record; restore authored order for the glyph-span
        // invariant. The sort is stable, so a dual interstitial pair keeps its
        // foot-then-lead order.
        facts.sort_by_key(|fact| fact.source_span().map_or(usize::MAX, ByteRange::start));
        facts
    }
}

/// A fact over the same parts with owned text, so it can attach to a document
/// over a different buffer.
fn owned_fact(fact: &Fact<'_>) -> Fact<'static> {
    Fact::new(
        fact.role(),
        Cow::Owned(String::from(fact.text())),
        fact.source_span(),
        fact.owner(),
    )
}

/// One splice applied to `facts`. A glyph that straddles a boundary is dropped,
/// never cut in half; a surviving glyph and its owner follow the splice.
fn remap_one(facts: &[Fact<'static>], from: usize, to: usize, shift: isize) -> Vec<Fact<'static>> {
    let mut out = Vec::with_capacity(facts.len());
    for fact in facts {
        if survives(fact, from, to) {
            out.push(shifted(fact, from, to, shift));
        }
    }
    out
}

/// Whether a fact's glyph survives a splice replacing `[from, to)`: a glyph
/// fully before or fully after stays, one that straddles or sits inside is cut.
fn survives(fact: &Fact<'_>, from: usize, to: usize) -> bool {
    match fact.source_span() {
        Some(span) => span.end() <= from || span.start() >= to,
        None => true,
    }
}

fn shifted(fact: &Fact<'static>, from: usize, to: usize, shift: isize) -> Fact<'static> {
    let source_span = fact.source_span().map(|span| map_span(span, from, to, shift));
    Fact::new(
        fact.role(),
        Cow::Owned(String::from(fact.text())),
        source_span,
        FactOwner::node(map_span(fact.owner().span(), from, to, shift)),
    )
}

/// One span through every splice, ascending. Used for a comment write's owner,
/// which is never inside a removed region.
fn remap_span(span: ByteRange, splices: &[Splice]) -> ByteRange {
    let mut delta: isize = 0;
    let mut current = span;
    for splice in splices.iter().rev() {
        let from = splice.from.wrapping_add_signed(delta);
        let to = splice.to.wrapping_add_signed(delta);
        current = map_span(current, from, to, splice.shift());
        delta = delta.saturating_add(splice.shift());
    }
    current
}

/// A span after the splice shifts; one inside a replacement moves into the
/// inserted region so an owner never dangles into removed bytes.
fn map_span(span: ByteRange, from: usize, to: usize, shift: isize) -> ByteRange {
    if span.start() >= to {
        ByteRange::try_new(
            span.start().wrapping_add_signed(shift),
            span.end().wrapping_add_signed(shift),
        )
        .expect("a common shift preserves order")
    } else if from < to && span.start() >= from {
        let inserted = isize::try_from(to - from)
            .unwrap_or(isize::MAX)
            .saturating_add(shift)
            .max(0);
        ByteRange::try_new(from, from + usize::try_from(inserted).unwrap_or(usize::MAX)).expect("ordered")
    } else if span.end() >= to && span.end() > from {
        // Interior edits resize the owner; an insertion at its end is outside it.
        ByteRange::try_new(span.start(), span.end().wrapping_add_signed(shift)).expect("ordered")
    } else {
        span
    }
}

fn plan_edits<'src>(
    src: &'src [u8],
    edits: &[Edit],
    dialect: Dialect,
    want_facts: bool,
) -> Result<Plan<'src>, structury::Error> {
    // One Strict pass locates every edit's primary span and, walking the whole
    // document, value-checks it. A leading Whole demand rides the same pass to
    // hand back the input facts when the caller keeps them or a fact op needs
    // the comment records; without it the facts `edit` discards are never built.
    let requests = edit_requests(edits, dialect);
    let needs_facts =
        want_facts || (dialect.has_comments() && edits.iter().any(|edit| matches!(edit, Edit::Fact { .. })));
    let mut demands: Vec<Demand> = Vec::with_capacity(requests.len() + usize::from(needs_facts));
    let whole_index = if needs_facts {
        demands.push(Demand::Whole);
        Some(0)
    } else {
        None
    };
    let mut slots: Vec<Option<usize>> = Vec::with_capacity(requests.len());
    for request in requests {
        match request {
            Some(demand) => {
                slots.push(Some(demands.len()));
                demands.push(demand);
            }
            None => slots.push(None),
        }
    }
    let req = ScanRequest {
        input: JsonInput::Text,
        demands: &demands,
        strictness: Strictness::Strict,
        max_nesting: MAX_NESTING,
        dialect,
        facts: needs_facts,
    };
    let answers = scan(src, &req)?.answers;
    let facts = match whole_index {
        Some(index) => match &answers[index] {
            Answer::Document(doc) => doc.facts().to_vec(),
            _ => return Err(error::write("validated input did not yield a document", 0)),
        },
        None => Vec::new(),
    };
    let mut spans: Vec<Option<ByteRange>> = Vec::with_capacity(edits.len());
    for slot in &slots {
        match slot {
            Some(index) => spans.push(match &answers[*index] {
                Answer::Document(d) => Some(d.root()),
                Answer::Missing => None,
                _ => return Err(error::write("edit path is not a value span", 0)),
            }),
            None => spans.push(None),
        }
    }
    let mut splices = Vec::with_capacity(edits.len());
    let mut facts = facts;
    // A trailing comma before a closer, only reachable in a commenting dialect,
    // can make a member/element splice malformed; that dialect keeps the check.
    let needs_validation = dialect.trailing_commas();
    for (index, edit) in edits.iter().enumerate() {
        plan_edit(src, edit, spans[index], dialect, index, &mut splices, &mut facts)?;
    }
    splices.sort_by(|a, b| b.from.cmp(&a.from).then(b.index.cmp(&a.index)));
    for (i, a) in splices.iter().enumerate() {
        if splices[i + 1..]
            .iter()
            .any(|b| a.from == b.from || (a.from < b.to && b.from < a.to))
        {
            return Err(error::write("overlapping edits", a.from));
        }
    }
    Ok(Plan {
        facts,
        splices,
        needs_validation,
    })
}

/// Plan one edit into `splices`, rewriting `facts` for a fact write.
#[allow(clippy::too_many_arguments)] // one call site; each table is a named splice layer
fn plan_edit(
    src: &[u8],
    edit: &Edit,
    span: Option<ByteRange>,
    dialect: Dialect,
    index: usize,
    splices: &mut Vec<Splice>,
    facts: &mut Vec<Fact<'_>>,
) -> Result<(), structury::Error> {
    if let Edit::Fact { op, .. } = edit {
        return plan_fact(src, op, span, dialect, index, splices, facts);
    }
    for (from, to, bytes) in plan_splice(src, edit, span, dialect)? {
        splices.push(Splice {
            index,
            from,
            to,
            bytes,
            fact: None,
        });
    }
    Ok(())
}

/// Plan one fact write. A commenting dialect's grammar gives the placement;
/// under [`Dialect::Rfc8259`] there are no facts, so every write is a no-op.
fn plan_fact(
    src: &[u8],
    op: &FactOp,
    span: Option<ByteRange>,
    dialect: Dialect,
    index: usize,
    splices: &mut Vec<Splice>,
    facts: &mut Vec<Fact<'_>>,
) -> Result<(), structury::Error> {
    if !dialect.has_comments() {
        return Ok(());
    }
    match op {
        FactOp::Insert { role, text } => {
            let owner = span.ok_or_else(|| error::edit_unplaceable(0))?;
            if text.is_empty() {
                return Ok(());
            }
            let (at, bytes) = match role {
                FactRole::CommentLead => (member_insert_at(src, owner.start(), dialect), canonical_comment(text)),
                FactRole::CommentInline => (owner.end(), inline_comment(text)),
                FactRole::CommentFoot => (owner.end(), foot_comment(text)),
            };
            let glyph = comment_glyph(&bytes);
            splices.push(Splice {
                index,
                from: at,
                to: at,
                bytes,
                fact: Some(NewFact {
                    role: *role,
                    text: String::from(text.as_str()),
                    owner: FactOwner::node(owner),
                    glyph,
                }),
            });
        }
        FactOp::Replace { role, text } => {
            plan_fact_rewrite(src, *role, span, Some(text.as_str()), index, splices, facts)?;
        }
        FactOp::Clear { role } => {
            plan_fact_rewrite(src, *role, span, None, index, splices, facts)?;
        }
    }
    Ok(())
}

/// Rewrite or clear the comment(s) of `role` on the node at `span`.
///
/// A foot fact shares the following node's lead glyph, so it is a no-op.
fn plan_fact_rewrite<'src>(
    src: &[u8],
    role: FactRole,
    span: Option<ByteRange>,
    replacement: Option<&str>,
    index: usize,
    splices: &mut Vec<Splice>,
    facts: &mut Vec<Fact<'src>>,
) -> Result<(), structury::Error> {
    if role == FactRole::CommentFoot {
        return Ok(());
    }
    let owner = span.ok_or_else(|| error::edit_unplaceable(0))?;
    let text = replacement.unwrap_or("");
    let mut found = false;
    let mut kept: Vec<Fact<'src>> = Vec::with_capacity(facts.len());
    for fact in facts.drain(..) {
        if fact.role() != role || fact.owner().span() != owner {
            kept.push(fact);
            continue;
        }
        let Some(glyph) = fact.source_span() else {
            kept.push(fact);
            continue;
        };
        found = true;
        let bytes = if text.is_empty() {
            Vec::new()
        } else {
            same_place_comment(src.get(glyph.start()..glyph.end()).unwrap_or(&[]), text)
                .unwrap_or_else(|| canonical_comment(text))
        };
        let fresh = (!text.is_empty()).then(|| NewFact {
            role,
            text: String::from(text),
            owner: fact.owner(),
            glyph: comment_glyph(&bytes),
        });
        splices.push(Splice {
            index,
            from: glyph.start(),
            to: glyph.end(),
            bytes,
            fact: fresh,
        });
    }
    *facts = kept;
    if !found && replacement.is_some() {
        return Err(error::write("edit fact is missing", owner.start()));
    }
    Ok(())
}

/// The comment token inside rendered comment bytes: leading trivia and the
/// terminating newline are not part of the glyph a scan records.
fn comment_glyph(bytes: &[u8]) -> (usize, usize) {
    let start = bytes
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        .map_or(start, |last| last + 1);
    (start, end)
}

/// The one path each edit locates; `None` skips an edit with nothing to locate
/// (a fact write under a non-commenting dialect).
fn edit_requests(edits: &[Edit], dialect: Dialect) -> Vec<Option<Demand>> {
    edits
        .iter()
        .map(|edit| match edit {
            Edit::Fact { .. } if !dialect.has_comments() => None,
            Edit::Insert { path, .. } | Edit::Fact { path, .. } if path.is_empty() => Some(Demand::Whole),
            Edit::Delete { path } | Edit::ReplaceMember { path, .. } if path.is_empty() => Some(Demand::Whole),
            Edit::Delete { path } | Edit::ReplaceMember { path, .. } => {
                Some(Demand::path(path[..path.len() - 1].to_vec()))
            }
            Edit::Insert { path, .. } => Some(Demand::path(path[..path.len() - 1].to_vec())),
            Edit::Set { path, .. } | Edit::Clear { path } | Edit::Fact { path, .. } => Some(Demand::path(path.clone())),
        })
        .collect()
}

/// Strict-validate one text value and return its document.
fn validated_document(src: &[u8], dialect: Dialect, facts: bool) -> Result<Document<'_>, structury::Error> {
    let req = ScanRequest {
        input: JsonInput::Text,
        demands: &[Demand::Whole],
        strictness: Strictness::Strict,
        max_nesting: MAX_NESTING,
        dialect,
        facts,
    };
    let mut answers = scan(src, &req)?.answers;
    match answers.pop() {
        Some(Answer::Document(doc)) => Ok(doc),
        _ => Err(error::write("validated input did not yield a document", 0)),
    }
}

/// Whether a failed plan may be re-expressed by re-encoding a DOM. A request
/// error (overlap, root delete/insert) or a host stop stays fatal.
fn fallback_able(error: &structury::Error) -> bool {
    error.code() == "edit-unplaceable"
}

/// Re-encode the whole document from a DOM application of `edits`, when the
/// splice path could not place them; returns `original` when the DOM cannot
/// represent the request or the result is invalid. The fallback drops formatting
/// and comments, where the splice keeps them.
fn fallback_encode(
    src: &[u8],
    edits: &[Edit],
    dialect: Dialect,
    original: structury::Error,
) -> Result<Vec<u8>, structury::Error> {
    if edits.iter().any(|edit| matches!(edit, Edit::Fact { .. })) {
        return Err(original);
    }
    let Ok(mut value) = parse(
        src,
        MaterializeOptions {
            dialect,
            form: Form::Value,
        },
    )
    .map(Materialized::into_value) else {
        return Err(original);
    };
    for edit in edits {
        if apply_dom(&mut value, edit).is_err() {
            return Err(original);
        }
    }
    let mut out = Vec::new();
    if crate::encode::encode(
        Source::Value(&value),
        &EncodeOptions::compact().with_dialect(dialect),
        &mut out,
    )
    .is_err()
    {
        return Err(original);
    }
    if validated_document(&out, dialect, false).is_err() {
        return Err(original);
    }
    Ok(out)
}

/// [`fallback_encode`] into `out`, returning the output's [`Document`].
fn fallback_document<'out>(
    src: &[u8],
    edits: &[Edit],
    dialect: Dialect,
    out: &'out mut Vec<u8>,
    original: structury::Error,
) -> Result<Document<'out>, structury::Error> {
    let bytes = fallback_encode(src, edits, dialect, original)?;
    out.clear();
    out.extend_from_slice(&bytes);
    let bytes: &'out [u8] = out;
    validated_document(bytes, dialect, true)
}

/// Apply one edit to a materialized value, last-wins like the splice path.
fn apply_dom(value: &mut Value, edit: &Edit) -> Result<(), ()> {
    match edit {
        Edit::Set { path, value: new } => {
            let target = dom_at_mut(value, path).ok_or(())?;
            *target = new.clone();
        }
        Edit::Insert { path, value: new } => {
            let Some(last) = path.last() else {
                return Err(());
            };
            let parent = dom_at_mut(value, &path[..path.len() - 1]).ok_or(())?;
            match (parent, last) {
                (Value::Object(members), Step::Key(name)) => {
                    if let Some((_, slot)) = members.iter_mut().rev().find(|(key, _)| key.as_str() == name.as_str()) {
                        *slot = new.clone();
                    } else {
                        members.push((structury::CompactStr::from(name.as_str()), new.clone()));
                    }
                }
                (Value::Array(items), Step::Index(index)) => {
                    let at = structury::resolve_index(items.len(), *index).ok_or(())?;
                    items.insert(at, new.clone());
                }
                _ => return Err(()),
            }
        }
        Edit::Delete { path } => {
            let Some(last) = path.last() else {
                return Err(());
            };
            let parent = dom_at_mut(value, &path[..path.len() - 1]).ok_or(())?;
            match (parent, last) {
                (Value::Object(members), Step::Key(name)) => {
                    if let Some(at) = members.iter().rposition(|(key, _)| key.as_str() == name.as_str()) {
                        members.remove(at);
                    }
                }
                (Value::Array(items), Step::Index(index)) => {
                    if let Some(at) = structury::resolve_index(items.len(), *index) {
                        items.remove(at);
                    }
                }
                _ => return Err(()),
            }
        }
        Edit::ReplaceMember { path, key, value: new } => {
            let Some(Step::Key(name)) = path.last() else {
                return Err(());
            };
            let parent = dom_at_mut(value, &path[..path.len() - 1]).ok_or(())?;
            let Value::Object(members) = parent else {
                return Err(());
            };
            let at = members
                .iter()
                .rposition(|(member, _)| member.as_str() == name.as_str())
                .ok_or(())?;
            members.remove(at);
            if let Some((_, slot)) = members.iter_mut().find(|(member, _)| member.as_str() == key.as_str()) {
                *slot = new.clone();
            } else {
                members.push((structury::CompactStr::from(key.as_str()), new.clone()));
            }
        }
        Edit::Clear { path } => {
            let target = dom_at_mut(value, path).ok_or(())?;
            match target {
                Value::Object(members) => members.clear(),
                Value::Array(items) => items.clear(),
                _ => return Err(()),
            }
        }
        Edit::Fact { .. } => return Err(()),
    }
    Ok(())
}

/// Mutable navigation to the value at `path`; absent key, index, or wrong kind is `None`.
fn dom_at_mut<'a>(value: &'a mut Value, path: &[Step]) -> Option<&'a mut Value> {
    match path.split_first() {
        None => Some(value),
        Some((Step::Key(name), rest)) => match value {
            Value::Object(members) => {
                let at = members.iter().rposition(|(key, _)| key.as_str() == name.as_str())?;
                dom_at_mut(&mut members[at].1, rest)
            }
            _ => None,
        },
        Some((Step::Index(index), rest)) => match value {
            Value::Array(items) => {
                let at = structury::resolve_index(items.len(), *index)?;
                dom_at_mut(items.get_mut(at)?, rest)
            }
            _ => None,
        },
    }
}

fn plan_splice(
    src: &[u8],
    edit: &Edit,
    span: Option<structury::ByteRange>,
    dialect: Dialect,
) -> Result<Vec<(usize, usize, Vec<u8>)>, structury::Error> {
    match edit {
        Edit::Set { value, .. } => {
            let span = span.ok_or_else(|| error::edit_unplaceable(0))?;
            Ok(vec![(span.start(), span.end(), encoded(value, dialect)?)])
        }
        Edit::Delete { path } => plan_delete(src, path, span, dialect),
        Edit::ReplaceMember { path, key, value } => plan_replace_member(src, path, key, value, span, dialect),
        Edit::Clear { .. } => plan_clear(src, span, dialect).map(|splice| vec![splice]),
        Edit::Insert { path, value } => {
            let span = span.ok_or_else(|| error::edit_unplaceable(0))?;
            plan_insert(src, path, span, value, dialect)
        }
        // Routed to `plan_fact` by `plan_edit`.
        Edit::Fact { .. } => Ok(Vec::new()),
    }
}

fn encoded(value: &Value, dialect: Dialect) -> Result<Vec<u8>, structury::Error> {
    let mut out = Vec::new();
    crate::encode::encode(
        Source::Value(value),
        &EncodeOptions::compact().with_dialect(dialect),
        &mut out,
    )?;
    Ok(out)
}

/// Remove the member or element `path` names. An absent target is a no-op; a
/// step that names the wrong container kind for its parent is one too.
fn plan_delete(
    src: &[u8],
    path: &[Step],
    span: Option<structury::ByteRange>,
    dialect: Dialect,
) -> Result<Vec<(usize, usize, Vec<u8>)>, structury::Error> {
    let Some(last) = path.last() else {
        return Err(error::write("cannot delete the document root", 0));
    };
    let Some(parent) = span else {
        return Ok(Vec::new());
    };
    if !matches!(
        (container_kind(src, parent, dialect)?, last),
        (Some(ContainerKind::Object), Step::Key(_)) | (Some(ContainerKind::Array), Step::Index(_))
    ) {
        return Ok(Vec::new());
    }
    let members = collect_members(src, parent, dialect)?;
    let target = match last {
        Step::Key(name) => members
            .iter()
            .rev()
            .find(|member| member.key.as_deref() == Some(name.as_str())),
        Step::Index(index) => structury::resolve_index(members.len(), *index).and_then(|at| members.get(at)),
    };
    let Some(member) = target else {
        return Ok(Vec::new());
    };
    let range = structury::ByteRange::try_new(member.start, member.end).expect("member span is ordered");
    let (from, to) = comma_span(src, range, dialect);
    Ok(vec![(from, to, Vec::new())])
}

/// Rewrite the member `path` names as `key` + `value`, keeping the colon and whitespace.
fn plan_replace_member(
    src: &[u8],
    path: &[Step],
    key: &str,
    value: &Value,
    span: Option<structury::ByteRange>,
    dialect: Dialect,
) -> Result<Vec<(usize, usize, Vec<u8>)>, structury::Error> {
    let Some(Step::Key(name)) = path.last() else {
        return Err(error::write("replace-member needs an object member path", 0));
    };
    let parent = span.ok_or_else(|| error::edit_unplaceable(0))?;
    let members = collect_members(src, parent, dialect)?;
    let member = members
        .iter()
        .rev()
        .find(|member| member.key.as_deref() == Some(name.as_str()))
        .ok_or_else(|| error::edit_unplaceable(parent.start()))?;
    // Renaming onto another member's key cannot be a splice: it would leave two
    // members with the same key. The DOM fallback collapses them instead.
    if member.key.as_deref() != Some(key) && members.iter().any(|member| member.key.as_deref() == Some(key)) {
        return Err(error::edit_unplaceable(parent.start()));
    }
    let mut new_key = Vec::new();
    crate::encode::write_string(key, false, &mut new_key);
    Ok(vec![
        (member.key_start, member.key_end, new_key),
        (member.value_start, member.value_end, encoded(value, dialect)?),
    ])
}

/// Replace the container at `span` with an empty one of the same kind.
fn plan_clear(
    src: &[u8],
    span: Option<structury::ByteRange>,
    dialect: Dialect,
) -> Result<(usize, usize, Vec<u8>), structury::Error> {
    let span = span.ok_or_else(|| error::edit_unplaceable(0))?;
    let open = crate::lex::skip_trivia(src, span.start(), dialect)?;
    let empty: &[u8] = match src.get(open) {
        Some(b'{') => b"{}",
        Some(b'[') => b"[]",
        _ => return Err(error::edit_unplaceable(open)),
    };
    Ok((span.start(), span.end(), empty.to_vec()))
}

/// Which container a parent span opens.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ContainerKind {
    Object,
    Array,
}

fn container_kind(
    src: &[u8],
    parent: structury::ByteRange,
    dialect: Dialect,
) -> Result<Option<ContainerKind>, structury::Error> {
    let open = crate::lex::skip_trivia(src, parent.start(), dialect)?;
    Ok(match src.get(open) {
        Some(b'{') => Some(ContainerKind::Object),
        Some(b'[') => Some(ContainerKind::Array),
        _ => None,
    })
}

/// Insert `value` at `path`. A key that already exists replaces that member's
/// value; a new key or index appends or lands in place with its separating
/// comma, never against a source trailing comma.
fn plan_insert(
    src: &[u8],
    path: &[Step],
    parent: structury::ByteRange,
    value: &Value,
    dialect: Dialect,
) -> Result<Vec<(usize, usize, Vec<u8>)>, structury::Error> {
    if path.is_empty() {
        return Err(error::write("cannot insert at the document root", 0));
    }
    let Some(last) = path.last() else {
        return Ok(Vec::new());
    };
    let kind = container_kind(src, parent, dialect)?.ok_or_else(|| error::edit_unplaceable(parent.start()))?;
    let repl = encoded(value, dialect)?;
    let members = collect_members(src, parent, dialect)?;
    match last {
        Step::Key(name) => {
            if kind != ContainerKind::Object {
                return Err(error::edit_unplaceable(parent.start()));
            }
            if let Some(member) = members
                .iter()
                .rev()
                .find(|member| member.key.as_deref() == Some(name.as_str()))
            {
                return Ok(vec![(member.value_start, member.value_end, repl)]);
            }
            let mut piece = Vec::new();
            crate::encode::write_string(name, false, &mut piece);
            piece.push(b':');
            piece.extend_from_slice(&repl);
            Ok(vec![append_member(parent, &members, &piece)])
        }
        Step::Index(index) => {
            if kind != ContainerKind::Array {
                return Err(error::edit_unplaceable(parent.start()));
            }
            let at = insert_index(members.len(), *index)?;
            if members.is_empty() {
                let open = parent.start() + 1;
                return Ok(vec![(open, open, repl)]);
            }
            if at == 0 {
                let mut piece = repl;
                piece.push(b',');
                return Ok(vec![(members[0].start, members[0].start, piece)]);
            }
            if at >= members.len() {
                return Ok(vec![append_member(parent, &members, &repl)]);
            }
            let mut piece = repl;
            piece.push(b',');
            Ok(vec![(members[at].start, members[at].start, piece)])
        }
    }
}

/// Splice a fresh member into a container; a non-empty one drops its trailing glue
/// (and any trailing comma) before its closer.
fn append_member(parent: structury::ByteRange, members: &[Member], piece: &[u8]) -> (usize, usize, Vec<u8>) {
    let from = members.last().map_or(parent.start() + 1, |member| member.end);
    let to = parent.end().saturating_sub(1);
    let mut bytes = Vec::with_capacity(piece.len().saturating_add(1));
    if !members.is_empty() {
        bytes.push(b',');
    }
    bytes.extend_from_slice(piece);
    (from, to, bytes)
}

/// One member/element of a parent span with the glue an edit splices around.
struct Member {
    /// Decoded object key; `None` for an array element.
    key: Option<String>,
    /// Leading trivia start: just after the opener or the previous comma.
    start: usize,
    /// Object key token.
    key_start: usize,
    key_end: usize,
    value_start: usize,
    value_end: usize,
    /// Past a same-line comment after the value, when one follows.
    end: usize,
}

/// Collect one parent's members in a single pass; the cursor is left Strict-valid input.
fn collect_members(
    src: &[u8],
    parent: structury::ByteRange,
    dialect: Dialect,
) -> Result<Vec<Member>, structury::Error> {
    let kind = container_kind(src, parent, dialect)?.ok_or_else(|| error::edit_unplaceable(parent.start()))?;
    let object = kind == ContainerKind::Object;
    let closer = if object { b'}' } else { b']' };
    let mut pos = parent.start() + 1;
    let mut members = Vec::new();
    loop {
        let start = pos;
        pos = crate::lex::skip_trivia(src, pos, dialect)?;
        if pos >= parent.end() {
            break;
        }
        let Some(&byte) = src.get(pos) else {
            break;
        };
        if byte == closer {
            break;
        }
        if byte == b',' {
            pos += 1;
            continue;
        }
        let (key, key_start, key_end) = if object {
            let key_start = pos;
            let key_end = crate::lex::skip_key(src, pos, crate::lex::Check::Values, dialect)?;
            let key = decode_key(src, key_start, key_end, dialect)?;
            pos = crate::lex::skip_trivia(src, key_end, dialect)?;
            if src.get(pos) != Some(&b':') {
                return Err(error::expected_colon(pos));
            }
            pos = crate::lex::skip_trivia(src, pos + 1, dialect)?;
            (Some(key), key_start, key_end)
        } else {
            (None, pos, pos)
        };
        let value_start = pos;
        let value_end = crate::lex::skip_value(src, value_start, crate::lex::Check::Values, MAX_NESTING, dialect)?;
        pos = value_end;
        let mut end = value_end;
        if let Some(inline) = take_inline_comment(src, pos, dialect)? {
            end = inline.end();
            pos = end;
        }
        members.push(Member {
            key,
            start,
            key_start,
            key_end,
            value_start,
            value_end,
            end,
        });
        let after = crate::lex::skip_trivia(src, pos, dialect)?;
        pos = if src.get(after) == Some(&b',') {
            after + 1
        } else {
            after
        };
    }
    Ok(members)
}

/// The decoded object key at `key_start..key_end`.
fn decode_key(src: &[u8], key_start: usize, key_end: usize, dialect: Dialect) -> Result<String, structury::Error> {
    if let Some(span) = crate::lex::key_plain_inner(src, key_start, key_end, dialect) {
        return Ok(String::from_utf8_lossy(src.get(span.start()..span.end()).unwrap_or(&[])).into_owned());
    }
    let mut key = String::new();
    crate::lex::parse_string_into(src, key_start, &mut key, dialect)?;
    Ok(key)
}

/// A comment on the same line as the value ending at `pos`, if any.
fn take_inline_comment(
    src: &[u8],
    pos: usize,
    dialect: Dialect,
) -> Result<Option<structury::ByteRange>, structury::Error> {
    if !dialect.has_comments() {
        return Ok(None);
    }
    let mut cursor = pos;
    while let Some(&byte) = src.get(cursor) {
        match byte {
            b' ' | b'\t' | b'\r' => cursor += 1,
            _ => break,
        }
    }
    if src.get(cursor) != Some(&b'/') {
        return Ok(None);
    }
    let end = crate::lex::skip_comment(src, cursor)?;
    Ok(structury::ByteRange::try_new(cursor, end))
}

fn insert_index(len: usize, index: i64) -> Result<usize, structury::Error> {
    if index >= 0 {
        let i = usize::try_from(index).map_err(|_| error::edit_unplaceable(0))?;
        if i > len {
            return Err(error::edit_unplaceable(0));
        }
        Ok(i)
    } else {
        let mag = usize::try_from(index.checked_neg().ok_or_else(|| error::edit_unplaceable(0))?)
            .map_err(|_| error::edit_unplaceable(0))?;
        len.checked_sub(mag).ok_or_else(|| error::edit_unplaceable(0))
    }
}

/// Canonical comment bytes: one `//` line per payload line, newline-terminated.
fn canonical_comment(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len().saturating_add(text.len() / 8).saturating_add(3));
    for line in text.split('\n') {
        out.extend_from_slice(b"//");
        if !line.is_empty() {
            out.push(b' ');
            out.extend_from_slice(line.as_bytes());
        }
        out.push(b'\n');
    }
    out
}

/// Reuse the authored glyph's shape when the new body fits it: a `//` span stays
/// a line comment, a `/* */` span a block comment. `None` needs the canonical
/// `//` (a multi-line body, or one that would close the block comment early).
fn same_place_comment(span: &[u8], text: &str) -> Option<Vec<u8>> {
    if text.contains('\n') {
        return None;
    }
    if span.starts_with(b"//") {
        let mut out = Vec::with_capacity(text.len().saturating_add(3));
        out.extend_from_slice(b"//");
        if !text.is_empty() {
            out.push(b' ');
            out.extend_from_slice(text.as_bytes());
        }
        return Some(out);
    }
    if span.starts_with(b"/*") && span.ends_with(b"*/") && !text.contains("*/") {
        let mut out = Vec::with_capacity(text.len().saturating_add(6));
        out.extend_from_slice(b"/* ");
        out.extend_from_slice(text.as_bytes());
        if !text.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(b"*/");
        return Some(out);
    }
    None
}

/// A same-line comment after a value, newline-terminated so a following `,` is not swallowed.
fn inline_comment(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len().saturating_add(5));
    out.extend_from_slice(b" // ");
    for line in text.split('\n') {
        if !out.ends_with(b" // ") {
            out.push(b' ');
        }
        out.extend_from_slice(line.as_bytes());
    }
    out.push(b'\n');
    out
}

/// A comment on its own line after a value: each payload line is its own `//`.
fn foot_comment(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len().saturating_add(6));
    for line in text.split('\n') {
        out.push(b'\n');
        out.extend_from_slice(b"//");
        if !line.is_empty() {
            out.push(b' ');
            out.extend_from_slice(line.as_bytes());
        }
    }
    out.push(b'\n');
    out
}

/// Where a fresh leading comment on the value at `value_start` begins: before
/// the member's key when it has one, else at the value itself. Walks back over
/// trivia, the `:` and its key, so a member lead lands before the key and an
/// array element lead lands after the `[` or `,`.
fn member_insert_at(bytes: &[u8], value_start: usize, dialect: Dialect) -> usize {
    let mut i = value_start;
    while i > 0 {
        let prev = i - 1;
        if matches!(bytes[prev], b' ' | b'\t' | b'\n' | b'\r') {
            i = prev;
            continue;
        }
        if bytes[prev] == b':' {
            i = prev;
            while i > 0 && matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'\r') {
                i -= 1;
            }
            if i > 0 && (bytes[i - 1] == b'"' || (dialect.json5() && bytes[i - 1] == b'\'')) {
                let quote = bytes[i - 1];
                i -= 1;
                while i > 0 {
                    i -= 1;
                    if bytes[i] == quote && (i == 0 || bytes[i - 1] != b'\\') {
                        break;
                    }
                }
            } else if dialect.json5() {
                while i > 0 && (crate::lex::is_ident_start(bytes[i - 1]) || bytes[i - 1].is_ascii_digit()) {
                    i -= 1;
                }
            }
            continue;
        }
        break;
    }
    i
}

fn comma_span(src: &[u8], span: structury::ByteRange, dialect: Dialect) -> (usize, usize) {
    let from = span.start();
    let mut to = span.end();
    let mut i = from;
    while i > 0 {
        i -= 1;
        match src[i] {
            b' ' | b'\t' | b'\n' | b'\r' => {}
            b',' => return (i, to),
            _ => break,
        }
    }
    if let Ok(after) = crate::lex::skip_trivia(src, span.end(), dialect)
        && src.get(after) == Some(&b',')
    {
        to = after + 1;
    }
    (from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_comment_writes_one_line_per_payload_line() {
        assert_eq!(canonical_comment("hi"), b"// hi\n");
        assert_eq!(canonical_comment("a\nb"), b"// a\n// b\n");
        assert_eq!(canonical_comment(""), b"//\n");
    }

    #[test]
    fn same_place_comment_reuses_the_glyph_shape() {
        assert_eq!(same_place_comment(b"/* hi */", "yo"), Some(b"/* yo */".to_vec()));
        assert_eq!(same_place_comment(b"// hi", "yo"), Some(b"// yo".to_vec()));
        assert_eq!(
            same_place_comment(b"/* hi */", "a\nb"),
            None,
            "multi-line needs canonical"
        );
        assert_eq!(same_place_comment(b"/* hi */", "x */"), None, "must not close early");
    }

    #[test]
    fn inline_and_foot_comments_terminate_the_line() {
        assert_eq!(inline_comment("x"), b" // x\n");
        assert_eq!(inline_comment("a\nb"), b" // a b\n");
        assert_eq!(foot_comment("x"), b"\n// x\n");
        assert_eq!(foot_comment("a\nb"), b"\n// a\n// b\n");
    }

    #[test]
    fn member_insert_at_walks_back_over_a_key() {
        let src = br#"{"a" : 1}"#;
        assert_eq!(member_insert_at(src, 7, Dialect::Rfc8259), 1, "before the key");
        let array = b"[ 1, 2 ]";
        assert_eq!(member_insert_at(array, 5, Dialect::Rfc8259), 4, "after the comma");
        assert_eq!(member_insert_at(b"[1]", 1, Dialect::Rfc8259), 1, "at the first element");
    }

    #[test]
    fn json5_member_insert_at_walks_back_over_a_bare_key() {
        let src = b"{a:1}";
        assert_eq!(member_insert_at(src, 3, Dialect::Json5), 1);
    }

    #[test]
    fn comma_span_drops_the_leading_or_trailing_separator() {
        let src = br#"{"a":1,"b":2}"#;
        let member = ByteRange::try_new(7, 12).expect("ordered");
        assert_eq!(comma_span(src, member, Dialect::Rfc8259), (6, 12), "leading comma");
        let last = ByteRange::try_new(1, 6).expect("ordered");
        assert_eq!(comma_span(src, last, Dialect::Rfc8259), (1, 7), "trailing comma");
    }

    fn number(text: &str) -> Value {
        Value::Number(structury::Number::parse(text).expect("number"))
    }

    #[test]
    fn fallback_encodes_the_dom_applied_value() {
        let src = br#"{ "a" : 1, "b" : [2, 3] }"#;
        let edits = [
            Edit::Set {
                path: vec![Step::Key("a".into())],
                value: number("9"),
            },
            Edit::Delete {
                path: vec![Step::Key("b".into()), Step::Index(0)],
            },
        ];
        let out = fallback_encode(src, &edits, Dialect::Rfc8259, error::write("forced", 0)).expect("fallback");
        assert_eq!(out, br#"{"a":9,"b":[3]}"#);
    }

    #[test]
    fn fallback_declines_a_fact_write() {
        let src = br#"{"a":1}"#;
        let edits = [Edit::Fact {
            path: Vec::new(),
            op: FactOp::Insert {
                role: FactRole::CommentLead,
                text: "x".into(),
            },
        }];
        let original = error::edit_unplaceable(0);
        assert_eq!(
            fallback_encode(src, &edits, Dialect::Jsonc, original.clone()),
            Err(original)
        );
    }

    #[test]
    fn fallback_able_marks_only_unplaceable_edits() {
        assert!(fallback_able(&error::edit_unplaceable(0)));
        assert!(!fallback_able(&error::write("overlapping edits", 0)));
    }
}
