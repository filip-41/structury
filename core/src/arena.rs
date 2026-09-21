//! One arena: the 12-byte node layout, payload resolution, navigation, and
//! subtree detach, shared by the owned and source-backed documents.

use alloc::string::String;
use alloc::vec::Vec;

use crate::compact::CompactStr;
use crate::error::{Error, ErrorClass};
use crate::number::{NonFinite, Number};
use crate::value::{Value, ValueKind};

/// Fallback id for a missing edge read: out of range, so it reads as `Null`
/// rather than aliasing node 0.
const NULL_ID: u32 = u32::MAX;

/// Supertrait that closes [`Arena`] to implementations outside this crate: the
/// node/edge/payload layout is the codec's own invariant, not an extension
/// point.
mod sealed {
    pub trait Sealed {}
}

/// One fixed-size arena node: the format-codec construction vocabulary, not a
/// navigation surface (read values through [`ArenaValue`]).
///
/// `#[doc(hidden)]`: a sibling codec assembles documents with it, but it is not
/// an extension point.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Node {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// Number spelling at `data[off..off + len]`.
    Number {
        /// Payload byte offset.
        off: u32,
        /// Payload byte length.
        len: u32,
    },
    /// Non-finite number: the codec classified `value` and stored its spelling at
    /// `data[off..off + len]`. The payload is codec-owned presentation; the
    /// classification is what the value model reads.
    NonFinite {
        /// Payload byte offset.
        off: u32,
        /// Payload byte length.
        len: u32,
        /// The classified non-finite value.
        value: NonFinite,
    },
    /// Decoded string bytes at `data[off..off + len]`.
    Str {
        /// Payload byte offset.
        off: u32,
        /// Payload byte length.
        len: u32,
    },
    /// Array of `len` child node ids at `edges[edge..edge + len]`.
    Array {
        /// First edge index.
        edge: u32,
        /// Child count.
        len: u32,
    },
    /// Object of `len` `(key, value)` node-id pairs at `edges[edge..edge + 2 * len]`, first-key last-wins.
    Object {
        /// First edge index.
        edge: u32,
        /// Member count.
        len: u32,
    },
}

/// The storage seam a document exposes to shared arena navigation. Payload
/// offsets address its logical payload buffer; not a user extension point:
/// **sealed**, so only [`OwnedDocument`](crate::OwnedDocument) and
/// [`BorrowedDocument`](crate::BorrowedDocument) implement it.
///
/// It stays public because [`ArenaValue`] is the navigation surface callers
/// read through; `Node` and the documents' `from_parts` are the hidden
/// construction seam.
pub trait Arena: sealed::Sealed {
    /// Node `id`, or `None` when out of range.
    fn node(&self, id: u32) -> Option<&Node>;
    /// Child and member id arena.
    fn edges(&self) -> &[u32];
    /// Payload bytes at `off..off + len`; empty when the range is out of bounds
    /// or, for a source-backed document, straddles the `source`/`spill` boundary.
    fn payload(&self, off: u32, len: u32) -> &[u8];
    /// Payload text at `off..off + len`; empty when out of bounds or not valid
    /// UTF-8. Every codec-stored payload is valid UTF-8, so the empty cases are
    /// only reachable through a caller-built arena.
    fn payload_str(&self, off: u32, len: u32) -> &str;
    /// Node count. A well-formed value tree is at most this deep, so it bounds
    /// the recursion over a caller-built cyclic arena.
    fn node_count(&self) -> usize;
}

impl sealed::Sealed for crate::owned::OwnedDocument {}
impl sealed::Sealed for crate::borrowed::BorrowedDocument<'_> {}

/// Read view of one value in an [`Arena`], exposed by the two documents as
/// `OwnedValue` / `BorrowedValue`.
pub struct ArenaValue<'a, A> {
    arena: &'a A,
    id: u32,
}

impl<A> Clone for ArenaValue<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A> Copy for ArenaValue<'_, A> {}

impl<A: core::fmt::Debug> core::fmt::Debug for ArenaValue<'_, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ArenaValue")
            .field("arena", &self.arena)
            .field("id", &self.id)
            .finish()
    }
}

impl<A: PartialEq> PartialEq for ArenaValue<'_, A> {
    fn eq(&self, other: &Self) -> bool {
        self.arena == other.arena && self.id == other.id
    }
}

impl<A: Eq> Eq for ArenaValue<'_, A> {}

impl<'a, A: Arena + 'a> ArenaValue<'a, A> {
    pub(crate) const fn new(arena: &'a A, id: u32) -> Self {
        Self { arena, id }
    }

    /// Kind of this value.
    #[must_use]
    #[inline]
    pub fn kind(self) -> ValueKind {
        kind(self.arena, self.id)
    }

    /// Raw payload bytes of a string or number; empty otherwise.
    #[must_use]
    #[inline]
    pub fn as_bytes(self) -> &'a [u8] {
        bytes(self.arena, self.id)
    }

    /// Decoded string text when this is a string.
    #[must_use]
    #[inline]
    pub fn as_str(self) -> Option<&'a str> {
        (self.kind() == ValueKind::String).then(|| str_of(self.arena, self.id))
    }

    /// Decoded string text; empty when this is not a string.
    #[must_use]
    #[inline]
    pub fn str(self) -> &'a str {
        self.as_str().unwrap_or("")
    }

    /// Exact `i64` when this is a number spelled as an integer (no fraction or
    /// exponent, and within `i64`).
    ///
    /// Parses the authored spelling, so the `to_` prefix marks the computation; it
    /// is not the free borrow a bare `as_` accessor would imply.
    #[must_use]
    #[inline]
    pub fn to_i64(self) -> Option<i64> {
        if self.kind() != ValueKind::Number {
            return None;
        }
        number(self.arena, self.id).parse().ok()
    }

    /// Authored (or codec-normalized) spelling of a number; empty otherwise.
    #[must_use]
    #[inline]
    pub fn number(self) -> &'a str {
        number(self.arena, self.id)
    }

    /// Child count for an array or object; `0` for a scalar.
    #[must_use]
    #[inline]
    pub fn len(self) -> usize {
        match self.kind() {
            ValueKind::Array => array_of(self.arena, self.id).map_or(0, |(_, len)| len),
            ValueKind::Object => object_of(self.arena, self.id).map_or(0, |(_, len)| len),
            _ => 0,
        }
    }

    /// Whether the container has no children (true for a scalar).
    #[must_use]
    #[inline]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Array elements in order; empty when this is not an array.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    #[inline]
    pub fn elements(self) -> impl Iterator<Item = ArenaValue<'a, A>> + 'a {
        let (edge, len) = array_of(self.arena, self.id).unwrap_or((0, 0));
        (0..len).map(move |i| ArenaValue::new(self.arena, self.arena.edges().get(edge + i).copied().unwrap_or(NULL_ID)))
    }

    /// Object members in first-key order as `(name, value)`; empty when not an object.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    #[inline]
    pub fn members(self) -> impl Iterator<Item = (&'a str, ArenaValue<'a, A>)> + 'a {
        let (edge, len) = object_of(self.arena, self.id).unwrap_or((0, 0));
        (0..len).map(move |i| {
            let (key, value) = member(self.arena, edge, i);
            (self.arena.payload_str(key.0, key.1), ArenaValue::new(self.arena, value))
        })
    }

    /// Object member values in first-key order, skipping the name decode; empty when not an object.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    #[inline]
    pub fn member_values(self) -> impl Iterator<Item = ArenaValue<'a, A>> + 'a {
        let (edge, len) = object_of(self.arena, self.id).unwrap_or((0, 0));
        (0..len).map(move |i| {
            ArenaValue::new(
                self.arena,
                self.arena.edges().get(edge + 2 * i + 1).copied().unwrap_or(NULL_ID),
            )
        })
    }

    /// Member `key`, last-wins when the arena holds duplicate keys; compares
    /// stored key bytes.
    #[must_use]
    #[inline]
    pub fn member(self, key: &str) -> Option<ArenaValue<'a, A>> {
        let query = key.as_bytes();
        let (edge, len) = object_of(self.arena, self.id)?;
        (0..len)
            .rev()
            .find_map(|i| {
                let (key, value) = member(self.arena, edge, i);
                (self.arena.payload(key.0, key.1) == query).then_some(value)
            })
            .map(|value| ArenaValue::new(self.arena, value))
    }

    /// Array element at `index`; `None` when not an array or out of range.
    #[must_use]
    #[inline]
    pub fn element(self, index: usize) -> Option<ArenaValue<'a, A>> {
        let (edge, len) = array_of(self.arena, self.id)?;
        (index < len).then(|| {
            ArenaValue::new(
                self.arena,
                self.arena.edges().get(edge + index).copied().unwrap_or(NULL_ID),
            )
        })
    }

    /// Owned [`Value`] for this value. Allocates.
    ///
    /// # Panics
    ///
    /// Panics when a `Node::Number` payload is not a valid number spelling, as
    /// a document assembled through the public `from_parts` can hold. A
    /// malformed arena — an out-of-range edge or a cycle — reads as
    /// [`Value::Null`] at the bad point instead of recursing forever.
    #[must_use]
    pub fn to_value(self) -> Value {
        to_value_at(self.arena, self.id, self.arena.node_count())
    }

    /// Owned arena of just this value's subtree: a selection from a large source
    /// never retains the source buffer.
    ///
    /// # Bounded refusal
    ///
    /// Arena payload offsets are 32-bit. A subtree whose copied payloads exceed
    /// that space (4 GiB cumulative) is refused, not panicked on: the result is
    /// an empty document (a null root). A malformed source arena — an
    /// out-of-range or straddling payload, invalid UTF-8, or a cycle — is
    /// refused the same way. The codec's scan paths already refuse the
    /// equivalent overflow at tape construction with a [`ErrorClass::Limit`]
    /// error.
    #[must_use]
    pub fn detach(self) -> crate::owned::OwnedDocument {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut pending = Vec::new();
        let mut data = String::new();
        match detach_at(
            self.arena,
            self.id,
            self.arena.node_count(),
            &mut nodes,
            &mut edges,
            &mut pending,
            &mut data,
        ) {
            Ok(root) => crate::owned::OwnedDocument::from_parts(nodes, edges, data, root),
            Err(_) => empty_owned(),
        }
    }
}

/// Kind of node `id`; `ValueKind::Null` when `id` is not a node.
fn kind<A: Arena + ?Sized>(arena: &A, id: u32) -> ValueKind {
    match arena.node(id) {
        Some(Node::Null) | None => ValueKind::Null,
        Some(Node::Bool(_)) => ValueKind::Bool,
        Some(Node::Number { .. } | Node::NonFinite { .. }) => ValueKind::Number,
        Some(Node::Str { .. }) => ValueKind::String,
        Some(Node::Array { .. }) => ValueKind::Array,
        Some(Node::Object { .. }) => ValueKind::Object,
    }
}

/// `(edge, len)` for an array node; `None` otherwise.
fn array_of<A: Arena + ?Sized>(arena: &A, id: u32) -> Option<(usize, usize)> {
    match arena.node(id)? {
        Node::Array { edge, len } => Some((*edge as usize, *len as usize)),
        Node::Null
        | Node::Bool(_)
        | Node::Number { .. }
        | Node::NonFinite { .. }
        | Node::Str { .. }
        | Node::Object { .. } => None,
    }
}

/// `(edge, len)` for an object node; `None` otherwise.
fn object_of<A: Arena + ?Sized>(arena: &A, id: u32) -> Option<(usize, usize)> {
    match arena.node(id)? {
        Node::Object { edge, len } => Some((*edge as usize, *len as usize)),
        Node::Null
        | Node::Bool(_)
        | Node::Number { .. }
        | Node::NonFinite { .. }
        | Node::Str { .. }
        | Node::Array { .. } => None,
    }
}

/// `(key node id, value node id)` of member `index` at `edge`.
fn member<A: Arena + ?Sized>(arena: &A, edge: usize, index: usize) -> ((u32, u32), u32) {
    let base = edge + 2 * index;
    let edges = arena.edges();
    let key = edges.get(base).copied().unwrap_or(NULL_ID);
    let value = edges.get(base + 1).copied().unwrap_or(NULL_ID);
    let (off, len) = match arena.node(key) {
        Some(Node::Str { off, len }) => (*off, *len),
        Some(
            Node::Null
            | Node::Bool(_)
            | Node::Number { .. }
            | Node::NonFinite { .. }
            | Node::Array { .. }
            | Node::Object { .. },
        )
        | None => (0, 0),
    };
    ((off, len), value)
}

/// Payload bytes of a string or number node; empty otherwise.
fn bytes<A: Arena + ?Sized>(arena: &A, id: u32) -> &[u8] {
    match arena.node(id) {
        Some(Node::Str { off, len } | Node::Number { off, len } | Node::NonFinite { off, len, .. }) => {
            arena.payload(*off, *len)
        }
        Some(Node::Null | Node::Bool(_) | Node::Array { .. } | Node::Object { .. }) | None => &[],
    }
}

/// Decoded string text of a string or number node; empty otherwise.
fn str_of<A: Arena + ?Sized>(arena: &A, id: u32) -> &str {
    match arena.node(id) {
        Some(Node::Str { off, len } | Node::Number { off, len } | Node::NonFinite { off, len, .. }) => {
            arena.payload_str(*off, *len)
        }
        Some(Node::Null | Node::Bool(_) | Node::Array { .. } | Node::Object { .. }) | None => "",
    }
}

/// Authored (or codec-normalized) spelling of a number node; empty otherwise.
fn number<A: Arena + ?Sized>(arena: &A, id: u32) -> &str {
    match arena.node(id) {
        Some(Node::Number { .. } | Node::NonFinite { .. }) => str_of(arena, id),
        Some(Node::Null | Node::Bool(_) | Node::Str { .. } | Node::Array { .. } | Node::Object { .. }) | None => "",
    }
}

/// Owned [`Value`] for node `id`. Allocates. `budget` bounds the recursion: a
/// well-formed tree is at most [`Arena::node_count`] deep, so an exhausted
/// budget means a malformed cycle, answered as [`Value::Null`].
fn to_value_at<A: Arena + ?Sized>(arena: &A, id: u32, budget: usize) -> Value {
    if budget == 0 {
        return Value::Null;
    }
    match arena.node(id) {
        Some(Node::Null) | None => Value::Null,
        Some(Node::Bool(value)) => Value::Bool(*value),
        Some(Node::Number { .. }) => Value::Number(Number::parse(number(arena, id)).expect("arena number spelling")),
        Some(Node::NonFinite { value, .. }) => Value::Number(Number::NonFinite(*value)),
        Some(Node::Str { .. }) => Value::Str(CompactStr::from(str_of(arena, id))),
        Some(Node::Array { .. }) => {
            let (edge, len) = array_of(arena, id).unwrap_or((0, 0));
            Value::Array(
                (0..len)
                    .map(|i| {
                        to_value_at(
                            arena,
                            arena.edges().get(edge + i).copied().unwrap_or(NULL_ID),
                            budget - 1,
                        )
                    })
                    .collect(),
            )
        }
        Some(Node::Object { .. }) => {
            let (edge, len) = object_of(arena, id).unwrap_or((0, 0));
            Value::Object(
                (0..len)
                    .map(|i| {
                        let ((off, key_len), value) = member(arena, edge, i);
                        (
                            CompactStr::from(arena.payload_str(off, key_len)),
                            to_value_at(arena, value, budget - 1),
                        )
                    })
                    .collect(),
            )
        }
    }
}

/// Copy `id`'s subtree into a fresh owned arena, remapping ids. Node ids come out
/// in pre-order and edge blocks in post-order, the layout the arena builders
/// produce. `budget` bounds the recursion like [`to_value_at`]; a malformed
/// payload, a 32-bit id overflow, or a cycle is a [`ErrorClass::Limit`] refusal.
#[allow(clippy::too_many_arguments)] // one recursion carrying the shared output arenas
fn detach_at<A: Arena + ?Sized>(
    arena: &A,
    id: u32,
    budget: usize,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<u32>,
    pending: &mut Vec<u32>,
    data: &mut String,
) -> Result<u32, Error> {
    if budget == 0 {
        return Err(malformed_error(id as usize));
    }
    Ok(match arena.node(id) {
        None | Some(Node::Null) => push(nodes, Node::Null)?,
        Some(Node::Bool(value)) => push(nodes, Node::Bool(*value))?,
        Some(Node::Number { off, len }) => {
            let (off, len) = append(data, checked_payload(arena, *off, *len)?)?;
            push(nodes, Node::Number { off, len })?
        }
        Some(Node::NonFinite { off, len, value }) => {
            let (off, len) = append(data, checked_payload(arena, *off, *len)?)?;
            push(
                nodes,
                Node::NonFinite {
                    off,
                    len,
                    value: *value,
                },
            )?
        }
        Some(Node::Str { off, len }) => {
            let (off, len) = append(data, checked_payload(arena, *off, *len)?)?;
            push(nodes, Node::Str { off, len })?
        }
        Some(Node::Array { edge, len }) => {
            let slot = push(nodes, Node::Null)?;
            let base = pending.len();
            for i in 0..*len as usize {
                let child = arena.edges().get(*edge as usize + i).copied().unwrap_or(NULL_ID);
                let child = detach_at(arena, child, budget - 1, nodes, edges, pending, data)?;
                pending.push(child);
            }
            let start = edges.len();
            edges.extend_from_slice(&pending[base..]);
            pending.truncate(base);
            nodes[slot as usize] = Node::Array {
                edge: edge_index(start)?,
                len: *len,
            };
            slot
        }
        Some(Node::Object { edge, len }) => {
            let slot = push(nodes, Node::Null)?;
            let base = pending.len();
            for i in 0..*len as usize {
                let child_edge = *edge as usize + 2 * i;
                let key = arena.edges().get(child_edge).copied().unwrap_or(NULL_ID);
                let value = arena.edges().get(child_edge + 1).copied().unwrap_or(NULL_ID);
                let key = detach_at(arena, key, budget - 1, nodes, edges, pending, data)?;
                let value = detach_at(arena, value, budget - 1, nodes, edges, pending, data)?;
                pending.push(key);
                pending.push(value);
            }
            let start = edges.len();
            edges.extend_from_slice(&pending[base..]);
            pending.truncate(base);
            nodes[slot as usize] = Node::Object {
                edge: edge_index(start)?,
                len: *len,
            };
            slot
        }
    })
}

/// The payload text at `off..off + len`, refusing an arena that cannot serve it:
/// out of range, a `source`/`spill` straddle, or invalid UTF-8. A codec-built
/// arena never hits this.
fn checked_payload<A: Arena + ?Sized>(arena: &A, off: u32, len: u32) -> Result<&str, Error> {
    let at = usize::try_from(off).unwrap_or(usize::MAX);
    let want = usize::try_from(len).map_err(|_| malformed_error(at))?;
    let bytes = arena.payload(off, len);
    if bytes.len() != want {
        return Err(malformed_error(at));
    }
    core::str::from_utf8(bytes).map_err(|_| malformed_error(at))
}

/// Append a node, returning its id. A node count past 32-bit ids is a refusal,
/// not a truncation: a caller-built arena can copy more nodes than its input.
fn push(nodes: &mut Vec<Node>, node: Node) -> Result<u32, Error> {
    let id = u32::try_from(nodes.len()).map_err(|_| count_error(nodes.len()))?;
    nodes.push(node);
    Ok(id)
}

/// Edge-arena start as a 32-bit index; a count past 32-bit ids is a refusal.
fn edge_index(len: usize) -> Result<u32, Error> {
    u32::try_from(len).map_err(|_| count_error(len))
}

/// Append `text` and report its `(off, len)` as 32-bit arena ids. Detach copies
/// whole payloads, so one length fits `u32`; the cumulative buffer need not, and
/// an offset past the 32-bit space (4 GiB) is a [`ErrorClass::Limit`] refusal.
pub(crate) fn append(data: &mut String, text: &str) -> Result<(u32, u32), Error> {
    let start = data.len();
    let end = start.checked_add(text.len()).ok_or_else(|| capacity_error(start))?;
    let Ok(end) = u64::try_from(end) else {
        return Err(capacity_error(start));
    };
    if end > u64::from(u32::MAX) + 1 {
        return Err(capacity_error(start));
    }
    let off = u32::try_from(start).map_err(|_| capacity_error(start))?;
    let len = u32::try_from(text.len()).map_err(|_| capacity_error(start))?;
    data.push_str(text);
    Ok((off, len))
}

/// The bounded-refusal result: one null node, no edges, no payload, a null root.
/// This is the same shape a `detach` of a null value produces.
pub(crate) fn empty_owned() -> crate::owned::OwnedDocument {
    crate::owned::OwnedDocument::from_parts(alloc::vec![Node::Null], Vec::new(), String::new(), 0)
}

/// The `Limit` refusal for a detached payload the 32-bit arena ids cannot address.
fn capacity_error(offset: usize) -> Error {
    Error::new(
        ErrorClass::Limit,
        "arena-capacity",
        "detached payload exceeds 32-bit arena offsets",
        offset,
    )
}

/// The `Limit` refusal for a detached arena past 32-bit node or edge ids.
fn count_error(offset: usize) -> Error {
    Error::new(
        ErrorClass::Limit,
        "arena-capacity",
        "detached arena exceeds 32-bit node or edge ids",
        offset,
    )
}

/// The `Limit` refusal for a malformed caller-built arena: an out-of-range or
/// straddling payload, invalid UTF-8, or a cycle.
pub(crate) fn malformed_error(offset: usize) -> Error {
    Error::new(
        ErrorClass::Limit,
        "arena-malformed",
        "arena cannot serve this subtree (out-of-range payload or a cycle)",
        offset,
    )
}
