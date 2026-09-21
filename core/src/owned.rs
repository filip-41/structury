//! Owned arena DOM: a node arena, an edge arena, and concatenated payload bytes.
//!
//! Built in one pass with no per-value heap allocation.

use alloc::string::String;
use alloc::vec::Vec;

use crate::arena::{Arena, ArenaValue, Node};
use crate::value::Value;

/// Owned arena DOM over concatenated payloads. See the module docs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedDocument {
    nodes: Vec<Node>,
    edges: Vec<u32>,
    data: String,
    root: u32,
}

impl OwnedDocument {
    /// Assemble a document from its three arenas: the format codec's
    /// construction seam. `data` is UTF-8, so stored spans need no re-validation.
    ///
    /// `#[doc(hidden)]`: not an extension point, and it does not check its
    /// arenas. A malformed arena is read defensively: an out-of-range edge reads
    /// as `null`, a cycle reads as `null` (or is refused by `detach`), and a
    /// `Node::Number` with a bad spelling panics in `to_value`.
    #[doc(hidden)]
    #[must_use]
    pub fn from_parts(nodes: Vec<Node>, edges: Vec<u32>, data: String, root: u32) -> Self {
        Self {
            nodes,
            edges,
            data,
            root,
        }
    }

    /// View of the root value.
    #[must_use]
    pub fn root(&self) -> OwnedValue<'_> {
        ArenaValue::new(self, self.root)
    }

    /// Owned [`Value`] for the root. Allocates.
    ///
    /// # Panics
    ///
    /// Panics when a `Node::Number` payload is not a valid number spelling, as a
    /// document assembled through the public `from_parts` can hold. A malformed
    /// edge or cycle reads as `null` rather than recursing forever.
    #[must_use]
    pub fn to_value(&self) -> Value {
        self.root().to_value()
    }

    /// Owned arena of the root subtree: copies only the reachable payloads.
    #[must_use]
    pub fn detach(&self) -> Self {
        self.root().detach()
    }
}

impl Arena for OwnedDocument {
    #[inline]
    fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.get(id as usize)
    }

    #[inline]
    fn edges(&self) -> &[u32] {
        &self.edges
    }

    #[inline]
    fn payload(&self, off: u32, len: u32) -> &[u8] {
        let start = off as usize;
        let Some(end) = start.checked_add(len as usize) else {
            return &[];
        };
        self.data.as_bytes().get(start..end).unwrap_or(&[])
    }

    #[inline]
    fn payload_str(&self, off: u32, len: u32) -> &str {
        let start = off as usize;
        let Some(end) = start.checked_add(len as usize) else {
            return "";
        };
        self.data.get(start..end).unwrap_or("")
    }

    #[inline]
    fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

/// View of one value in an [`OwnedDocument`]; the API is the shared
/// [`ArenaValue`].
pub type OwnedValue<'a> = ArenaValue<'a, OwnedDocument>;
