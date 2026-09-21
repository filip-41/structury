//! Source-backed, zero-copy whole-document view: node and edge arenas over
//! source bytes.
//!
//! A payload offset addresses the logical buffer `source ++ spill`, so a plain document copies no payload byte.

use alloc::string::String;
use alloc::vec::Vec;

use crate::arena::{Arena, ArenaValue, Node, append, empty_owned, malformed_error};
use crate::error::Error;
use crate::owned::OwnedDocument;
use crate::value::Value;

/// Borrowed arena view over `source` plus a payload spill. See module docs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BorrowedDocument<'src> {
    source: &'src [u8],
    nodes: Vec<Node>,
    edges: Vec<u32>,
    spill: String,
    root: u32,
}

impl<'src> BorrowedDocument<'src> {
    /// Assemble a document from its arenas: the format codec's construction
    /// seam. Payload offsets address `source ++ spill`.
    ///
    /// `#[doc(hidden)]`: not an extension point, and it does not check its
    /// arenas. A malformed arena is read defensively: an out-of-range edge reads
    /// as `null`, a cycle reads as `null` (or is refused by `detach`/`to_owned`),
    /// a straddling payload reads as empty (and is refused by `to_owned`), and a
    /// `Node::Number` with a bad spelling panics in `to_value`.
    #[doc(hidden)]
    #[must_use]
    pub fn from_parts(source: &'src [u8], nodes: Vec<Node>, edges: Vec<u32>, spill: String, root: u32) -> Self {
        Self {
            source,
            nodes,
            edges,
            spill,
            root,
        }
    }

    /// Source bytes the payloads are sliced from.
    #[must_use]
    pub const fn source(&self) -> &'src [u8] {
        self.source
    }

    /// Bytes in the document's own spill; zero when every payload is a source slice.
    #[must_use]
    pub fn spilled_bytes(&self) -> usize {
        self.spill.len()
    }

    /// View of the root value.
    #[must_use]
    pub fn root(&self) -> BorrowedValue<'_> {
        ArenaValue::new(self, self.root)
    }

    /// Owned [`Value`] for the root. Allocates.
    ///
    /// # Panics
    ///
    /// Panics when a node's payload is not a valid number spelling on a number
    /// node, as a document assembled through the public `from_parts` can hold. A
    /// malformed edge or cycle reads as `null` rather than recursing forever.
    #[must_use]
    pub fn to_value(&self) -> Value {
        self.root().to_value()
    }

    /// Owned arena twin of this view, copying every payload once.
    ///
    /// # Bounded refusal
    ///
    /// Arena payload offsets are 32-bit. If the copied payloads exceed that space
    /// (4 GiB cumulative) this returns an empty document (a null root) instead of
    /// panicking; an out-of-range, straddling, or non-UTF-8 payload is refused the
    /// same way. A codec-built document never hits either case.
    #[must_use]
    pub fn to_owned(&self) -> OwnedDocument {
        let mut data = String::new();
        let mut nodes = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let copied = match node {
                Node::Null => Node::Null,
                Node::Bool(value) => Node::Bool(*value),
                Node::Number { off, len } => match copy_payload(&mut data, self, *off, *len) {
                    Ok((off, len)) => Node::Number { off, len },
                    Err(_) => return empty_owned(),
                },
                Node::NonFinite { off, len, value } => match copy_payload(&mut data, self, *off, *len) {
                    Ok((off, len)) => Node::NonFinite {
                        off,
                        len,
                        value: *value,
                    },
                    Err(_) => return empty_owned(),
                },
                Node::Str { off, len } => match copy_payload(&mut data, self, *off, *len) {
                    Ok((off, len)) => Node::Str { off, len },
                    Err(_) => return empty_owned(),
                },
                Node::Array { edge, len } => Node::Array { edge: *edge, len: *len },
                Node::Object { edge, len } => Node::Object { edge: *edge, len: *len },
            };
            nodes.push(copied);
        }
        OwnedDocument::from_parts(nodes, self.edges.clone(), data, self.root)
    }

    /// Owned arena of the root subtree: copies only the reachable payloads, so the source is not retained.
    #[must_use]
    pub fn detach(&self) -> OwnedDocument {
        self.root().detach()
    }
}

/// Copy the payload at `off..off + len` into `data`, refusing an arena whose
/// payload is out of range, straddles the `source`/`spill` boundary, or is not
/// valid UTF-8. A codec-built document never hits this.
fn copy_payload(data: &mut String, document: &BorrowedDocument<'_>, off: u32, len: u32) -> Result<(u32, u32), Error> {
    let at = usize::try_from(off).unwrap_or(usize::MAX);
    let want = usize::try_from(len).map_err(|_| malformed_error(at))?;
    let bytes = document.payload(off, len);
    if bytes.len() != want {
        return Err(malformed_error(at));
    }
    let text = core::str::from_utf8(bytes).map_err(|_| malformed_error(at))?;
    append(data, text)
}

impl Arena for BorrowedDocument<'_> {
    #[inline]
    fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.get(id as usize)
    }

    #[inline]
    fn edges(&self) -> &[u32] {
        &self.edges
    }

    /// Payload bytes at `off..off + len` in `source ++ spill`. A payload that
    /// straddles the boundary (only a caller-built arena can) reads as empty;
    /// `copy_payload` refuses it rather than copying the wrong range.
    #[inline]
    fn payload(&self, off: u32, len: u32) -> &[u8] {
        let start = off as usize;
        let Some(end) = start.checked_add(len as usize) else {
            return &[];
        };
        if start < self.source.len() {
            self.source.get(start..end).unwrap_or(&[])
        } else {
            let spill_start = start - self.source.len();
            self.spill
                .as_bytes()
                .get(spill_start..spill_start + len as usize)
                .unwrap_or(&[])
        }
    }

    /// Payload text; codec-stored payloads are valid UTF-8 (source value-checked,
    /// spill decoded). A caller-built payload that is not valid UTF-8 reads empty.
    #[inline]
    fn payload_str(&self, off: u32, len: u32) -> &str {
        core::str::from_utf8(self.payload(off, len)).unwrap_or("")
    }

    #[inline]
    fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

/// Borrowed view of one value in a [`BorrowedDocument`]; the API is the shared
/// [`ArenaValue`].
pub type BorrowedValue<'doc> = ArenaValue<'doc, BorrowedDocument<'doc>>;
