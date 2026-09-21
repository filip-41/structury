//! One-pass arena build: decode a validated span into flat node and edge arenas.
//!
//! One builder and two payload strategies (owned copy vs source slice with an escape spill), selected by a type parameter.

use alloc::string::String;
use alloc::vec::Vec;

use structury::{ByteRange, Columns, CompactStr, ErrorClass, Node, OwnedDocument};

use crate::dialect::Dialect;
use crate::error;
use crate::lex::{self, MAX_NESTING};

/// Cheap 64-bit key hash for object dedup; a collision costs a `find_member` scan, never correctness.
#[inline]
fn key_hash(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mut word = len as u64;
    if len >= 8 {
        let head: [u8; 8] = bytes[..8].try_into().expect("8 bytes");
        let tail: [u8; 8] = bytes[len - 8..].try_into().expect("8 bytes");
        word ^= u64::from_le_bytes(head) ^ u64::from_le_bytes(tail).rotate_left(31);
    } else {
        let mut head = [0u8; 8];
        head[..len].copy_from_slice(bytes);
        word ^= u64::from_le_bytes(head);
    }
    word = word.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    word ^ (word >> 29)
}

/// The output strategy of a [`Builder`]: how a payload is stored and what document the arenas become.
pub(crate) trait Payload<'a> {
    /// Finished document type.
    type Doc;
    /// Strategy state sized from the input length.
    fn new(src: &'a [u8]) -> Self;
    /// Record the absolute source start of the span being lexed.
    fn start_span(&mut self, base: usize);
    /// Store a validated payload. `source` is the relative source span when the
    /// payload is a contiguous slice of it.
    fn store(&mut self, bytes: &[u8], source: Option<(usize, usize)>) -> Result<(u32, u32), structury::Error>;
    /// Payload bytes at `off..off + len` in the logical payload buffer.
    fn payload(&self, off: u32, len: u32) -> &[u8];
    /// Reserve payload capacity once the builder knows the span it will lex, so a
    /// projection over a large input does not reserve the whole input.
    fn reserve(&mut self, _additional: usize) {}
    /// 32-bit id overflow refusal for this strategy.
    fn overflow() -> structury::Error;
    /// Assemble the document from the arenas.
    fn into_doc(self, nodes: Vec<Node>, edges: Vec<u32>, root: u32) -> Self::Doc;
}

/// Owned payload strategy: every payload is copied into one `data` buffer.
pub(crate) struct OwnedPayload {
    data: Vec<u8>,
}

impl<'a> Payload<'a> for OwnedPayload {
    type Doc = OwnedDocument;

    fn new(_src: &'a [u8]) -> Self {
        // Payload size tracks the answered span, not the input buffer.
        Self { data: Vec::new() }
    }

    fn reserve(&mut self, additional: usize) {
        if additional > 0 {
            self.data.reserve(additional);
        }
    }

    fn start_span(&mut self, _base: usize) {}

    #[inline]
    fn store(&mut self, bytes: &[u8], _source: Option<(usize, usize)>) -> Result<(u32, u32), structury::Error> {
        let off = u32::try_from(self.data.len()).map_err(|_| Self::overflow())?;
        let len = u32::try_from(bytes.len()).map_err(|_| Self::overflow())?;
        if self.data.len() + bytes.len() > u32::MAX as usize {
            return Err(Self::overflow());
        }
        self.data.extend_from_slice(bytes);
        Ok((off, len))
    }

    #[inline]
    fn payload(&self, off: u32, len: u32) -> &[u8] {
        let start = off as usize;
        self.data.get(start..start + len as usize).unwrap_or(&[])
    }

    fn overflow() -> structury::Error {
        structury::Error::new(ErrorClass::Limit, "tape-capacity", "tape exceeds 32-bit arena ids", 0)
    }

    fn into_doc(self, nodes: Vec<Node>, edges: Vec<u32>, root: u32) -> OwnedDocument {
        OwnedDocument::from_parts(
            nodes,
            edges,
            String::from_utf8(self.data).expect("tape payload bytes are UTF-8"),
            root,
        )
    }
}

/// Nodes required before the one-time arena sizing trusts its density sample.
const SIZE_SAMPLE: usize = 256;

/// Cap on the input-length reserve, so the projection (not the initial guess)
/// sets the arena size and a resize copies at most this much.
const INITIAL_CAP: usize = 1 << 16;

/// Shared arena builder over one payload strategy. See the module docs.
pub(crate) struct Builder<'a, P: Payload<'a>> {
    src: &'a [u8],
    nodes: Vec<Node>,
    edges: Vec<u32>,
    pending: Vec<u32>,
    scratch: String,
    dialect: Dialect,
    payload: P,
    /// Bytes of the current span consumed at the last completed value.
    progress: usize,
    /// Length of the current span, the projection's denominator basis.
    span_len: usize,
    /// Whether the one-time arena sizing has run for this span.
    sized: bool,
}

impl<'a, P: Payload<'a>> Builder<'a, P> {
    /// Reserve from input length. Growth stays amortized.
    pub(crate) fn new(src: &'a [u8], dialect: Dialect) -> Self {
        Self {
            src,
            nodes: Vec::with_capacity((src.len() / 10).min(INITIAL_CAP)),
            edges: Vec::with_capacity((src.len() / 10).min(INITIAL_CAP)),
            pending: Vec::new(),
            scratch: String::new(),
            dialect,
            payload: P::new(src),
            progress: 0,
            span_len: 0,
            sized: false,
        }
    }

    pub(crate) fn finish(self, root: u32) -> P::Doc {
        self.payload.into_doc(self.nodes, self.edges, root)
    }

    fn push(&mut self, node: Node) -> Result<u32, structury::Error> {
        let id = u32::try_from(self.nodes.len()).map_err(|_| P::overflow())?;
        self.nodes.push(node);
        Ok(id)
    }

    /// Size both arenas once, after enough nodes project the span's density.
    #[inline]
    fn size_arenas(&mut self) {
        if self.sized || self.progress == 0 || self.nodes.len() < SIZE_SAMPLE {
            return;
        }
        self.sized = true;
        self.grow_projected();
    }

    #[cold]
    #[inline(never)]
    fn grow_projected(&mut self) {
        let nodes = self.projected_len(self.nodes.len());
        if nodes > self.nodes.capacity() {
            self.nodes.reserve_exact(nodes - self.nodes.len());
        }
        let edges = self.projected_len(self.edges.len());
        if edges > self.edges.capacity() {
            self.edges.reserve_exact(edges - self.edges.len());
        }
    }

    /// Project the final arena length from the sample, with an eighth of slack.
    fn projected_len(&self, produced: usize) -> usize {
        if self.progress == 0 || produced == 0 || self.span_len == 0 || produced > self.span_len {
            return produced.saturating_add(1);
        }
        let estimate = (produced as u128 * self.span_len as u128 / self.progress as u128) as usize;
        (estimate + estimate / 8).clamp(produced + 1, self.span_len.saturating_add(1))
    }

    fn node_bytes(&self, id: u32) -> &[u8] {
        match self.nodes.get(id as usize) {
            Some(Node::Str { off, len }) => self.payload.payload(*off, *len),
            _ => &[],
        }
    }

    /// Append `bytes` and push a string node. Keys and values share this.
    fn push_string(&mut self, bytes: &[u8], source: Option<(usize, usize)>) -> Result<(u32, u64), structury::Error> {
        let hash = key_hash(bytes);
        let (off, len) = self.payload.store(bytes, source)?;
        let id = self.push(Node::Str { off, len })?;
        Ok((id, hash))
    }

    pub(crate) fn span(&mut self, span: ByteRange) -> Result<u32, structury::Error> {
        self.payload.start_span(span.start());
        let bytes = self.src.get(span.start()..span.end()).unwrap_or(&[]);
        self.progress = 0;
        self.span_len = bytes.len();
        self.sized = false;
        // The payload buffers lexed tokens from this span. Reserve its length.
        self.payload.reserve(bytes.len());
        self.parse_span(bytes).map_err(|error| {
            structury::Error::new(
                error.class(),
                error.code(),
                error.message(),
                span.start().saturating_add(error.offset() as usize),
            )
        })
    }

    /// Parse in slice coordinates. `span` translates diagnostics at the source boundary.
    fn parse_span(&mut self, bytes: &[u8]) -> Result<u32, structury::Error> {
        let pos = self.trivia(bytes, 0)?;
        let (id, end) = self.present(bytes, pos, 0)?;
        let tail = self.trivia(bytes, end)?;
        if tail < bytes.len() {
            return Err(error::trailing_content(tail));
        }
        Ok(id)
    }

    /// Trivia. RFC 8259 takes the whitespace fast path.
    #[allow(clippy::inline_always)] // hot member loop: the peek must fold into it
    #[inline(always)]
    fn trivia(&self, bytes: &[u8], pos: usize) -> Result<usize, structury::Error> {
        if self.dialect == Dialect::Rfc8259 {
            Ok(lex::ws_rfc(bytes, pos))
        } else {
            Ok(lex::skip_trivia(bytes, pos, self.dialect)?)
        }
    }

    /// A value whose first token byte is `pos` (already past trivia).
    #[inline]
    fn present(&mut self, bytes: &[u8], pos: usize, depth: u32) -> Result<(u32, usize), structury::Error> {
        match bytes.get(pos) {
            Some(b'[') => self.array(bytes, pos, depth),
            Some(b'{') => self.object(bytes, pos, depth),
            Some(_) => self.scalar(bytes, pos),
            None => Err(error::expected_value(pos)),
        }
    }

    #[inline]
    fn scalar(&mut self, bytes: &[u8], pos: usize) -> Result<(u32, usize), structury::Error> {
        let Some(&byte) = bytes.get(pos) else {
            return Err(error::expected_value(pos));
        };
        match byte {
            b'n' => {
                let end = lex::skip_present(bytes, pos, lex::Check::Values, MAX_NESTING, self.dialect)?;
                Ok((self.push(Node::Null)?, end))
            }
            b't' => {
                let end = lex::skip_present(bytes, pos, lex::Check::Values, MAX_NESTING, self.dialect)?;
                Ok((self.push(Node::Bool(true))?, end))
            }
            b'f' => {
                let end = lex::skip_present(bytes, pos, lex::Check::Values, MAX_NESTING, self.dialect)?;
                Ok((self.push(Node::Bool(false))?, end))
            }
            b'-' | b'0'..=b'9' => self.atom(bytes, pos),
            b'+' | b'.' | b'I' | b'N' if self.dialect.json5() => self.number(bytes, pos),
            b'"' => self.string(bytes, pos),
            b'\'' if self.dialect.json5() => self.string(bytes, pos),
            _ => Err(error::expected_value(pos)),
        }
    }

    /// A digit or sign value. Integers store the spelling directly.
    fn atom(&mut self, bytes: &[u8], pos: usize) -> Result<(u32, usize), structury::Error> {
        let Some(end) = lex::number::skip_rfc_integer(bytes, pos, self.dialect) else {
            return self.number(bytes, pos);
        };
        let (off, len) = self.payload.store(&bytes[pos..end], Some((pos, end - pos)))?;
        Ok((self.push(Node::Number { off, len })?, end))
    }

    fn number(&mut self, bytes: &[u8], pos: usize) -> Result<(u32, usize), structury::Error> {
        let end = lex::number::lex_number(bytes, pos, self.dialect)?;
        let raw = &bytes[pos..end];
        let node = if self.dialect.json5() {
            let text = core::str::from_utf8(raw).map_err(|e| error::utf8(pos + e.valid_up_to()))?;
            let mut scratch = core::mem::take(&mut self.scratch);
            let normalized = lex::number::normalize_json5_into(text, &mut scratch);
            let non_finite = lex::number::non_finite_value(normalized);
            let validated = non_finite
                .is_none()
                .then(|| structury::Number::parse_owned(CompactStr::from(normalized)));
            let stored = self.payload.store(normalized.as_bytes(), None);
            self.scratch = scratch;
            if let Some(result) = validated {
                result.map_err(|_| error::invalid_number(pos))?;
            }
            let (off, len) = stored?;
            non_finite.map_or(Node::Number { off, len }, |value| Node::NonFinite { off, len, value })
        } else {
            // A spelling without `.`/`e`/`E` is an integer: `Number` never refuses it.
            if raw.iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
                let text = core::str::from_utf8(raw).map_err(|e| error::utf8(pos + e.valid_up_to()))?;
                structury::Number::parse(text).map_err(|_| error::invalid_number(pos))?;
            }
            let (off, len) = self.payload.store(raw, Some((pos, raw.len())))?;
            Node::Number { off, len }
        };
        Ok((self.push(node)?, end))
    }

    fn string(&mut self, bytes: &[u8], pos: usize) -> Result<(u32, usize), structury::Error> {
        if let Some((inner, end)) = lex::string::plain_double_quoted(bytes, pos) {
            // The plain run stops at non-ASCII, so `inner` is already valid UTF-8 and can be kept as a slice.
            let (off, len) = self.payload.store(inner, Some((pos + 1, inner.len())))?;
            return Ok((self.push(Node::Str { off, len })?, end));
        }
        let (off, len, end) = self.escaped(bytes, pos)?;
        Ok((self.push(Node::Str { off, len })?, end))
    }

    fn escaped(&mut self, bytes: &[u8], pos: usize) -> Result<(u32, u32, usize), structury::Error> {
        let end = lex::parse_string_into(bytes, pos, &mut self.scratch, self.dialect)?;
        let (off, len) = self.payload.store(self.scratch.as_bytes(), None)?;
        Ok((off, len, end))
    }

    fn key(&mut self, bytes: &[u8], pos: usize) -> Result<(u32, u64, usize), structury::Error> {
        match bytes.get(pos) {
            Some(b'"') => {
                if let Some((inner, end)) = lex::string::plain_key(bytes, pos) {
                    let (id, hash) = self.push_string(inner, Some((pos + 1, inner.len())))?;
                    return Ok((id, hash, end));
                }
                let end = lex::parse_string_into(bytes, pos, &mut self.scratch, self.dialect)?;
                let scratch = core::mem::take(&mut self.scratch);
                let pushed = self.push_string(scratch.as_bytes(), None);
                self.scratch = scratch;
                let (id, hash) = pushed?;
                Ok((id, hash, end))
            }
            Some(b'\'') if self.dialect.json5() => {
                let end = lex::parse_string_into(bytes, pos, &mut self.scratch, self.dialect)?;
                let scratch = core::mem::take(&mut self.scratch);
                let pushed = self.push_string(scratch.as_bytes(), None);
                self.scratch = scratch;
                let (id, hash) = pushed?;
                Ok((id, hash, end))
            }
            Some(&byte) if self.dialect.json5() && lex::is_ident_start(byte) => {
                let end = lex::ident_end(bytes, pos);
                let text = core::str::from_utf8(&bytes[pos..end]).map_err(|e| error::utf8(pos + e.valid_up_to()))?;
                let (id, hash) = self.push_string(text.as_bytes(), Some((pos, end - pos)))?;
                Ok((id, hash, end))
            }
            _ => Err(error::expected_key(pos)),
        }
    }

    fn array(&mut self, bytes: &[u8], start: usize, depth: u32) -> Result<(u32, usize), structury::Error> {
        if depth >= MAX_NESTING {
            return Err(error::limit(start));
        }
        let id = self.push(Node::Array { edge: 0, len: 0 })?;
        let base = self.pending.len();
        let mut count = 0u32;
        let mut pos = start + 1;
        loop {
            pos = self.trivia(bytes, pos)?;
            let Some(&byte) = bytes.get(pos) else {
                return Err(error::expected_comma_array(pos));
            };
            if byte == b']' {
                pos += 1;
                break;
            }
            if count > 0 {
                if byte != b',' {
                    return Err(error::expected_comma_array(pos));
                }
                pos += 1;
                pos = self.trivia(bytes, pos)?;
                if bytes.get(pos) == Some(&b']') {
                    if self.dialect.trailing_commas() {
                        pos += 1;
                        break;
                    }
                    return Err(error::trailing_comma(pos));
                }
            }
            let (child, end) = self.present(bytes, pos, depth + 1)?;
            self.pending.push(child);
            count += 1;
            pos = end;
            if !self.sized {
                self.progress = end;
                self.size_arenas();
            }
        }
        let edge = u32::try_from(self.edges.len()).map_err(|_| P::overflow())?;
        self.edges.extend_from_slice(&self.pending[base..]);
        self.pending.truncate(base);
        self.nodes[id as usize] = Node::Array { edge, len: count };
        self.progress = pos;
        self.size_arenas();
        Ok((id, pos))
    }

    fn object(&mut self, bytes: &[u8], start: usize, depth: u32) -> Result<(u32, usize), structury::Error> {
        if depth >= MAX_NESTING {
            return Err(error::limit(start));
        }
        let id = self.push(Node::Object { edge: 0, len: 0 })?;
        let base = self.pending.len();
        // 1024-bit Bloom gate: a clear bit means the key is new, so the member scan runs only on a set bit.
        let mut seen = [0u64; 16];
        let mut count = 0u32;
        let mut pos = start + 1;
        loop {
            pos = self.trivia(bytes, pos)?;
            let Some(&byte) = bytes.get(pos) else {
                return Err(error::expected_key(pos));
            };
            if byte == b'}' {
                pos += 1;
                break;
            }
            if count > 0 {
                if byte != b',' {
                    return Err(error::expected_comma_object(pos));
                }
                pos += 1;
                pos = self.trivia(bytes, pos)?;
                if bytes.get(pos) == Some(&b'}') {
                    if self.dialect.trailing_commas() {
                        pos += 1;
                        break;
                    }
                    return Err(error::trailing_comma(pos));
                }
            }
            let (key_id, key_hash, next) = self.key(bytes, pos)?;
            // A colon adjacent to the key is one byte test.
            pos = if bytes.get(next) == Some(&b':') {
                next + 1
            } else {
                let colon = self.trivia(bytes, next)?;
                if bytes.get(colon) != Some(&b':') {
                    return Err(error::expected_colon(colon));
                }
                colon + 1
            };
            pos = self.trivia(bytes, pos)?;
            let (value_id, end) = self.present(bytes, pos, depth + 1)?;
            pos = end;
            let bit = (key_hash.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 54) as usize;
            let mut existing = None;
            if seen[bit >> 6] & (1 << (bit & 63)) != 0 {
                existing = self.find_member(base, count, key_id);
            }
            seen[bit >> 6] |= 1 << (bit & 63);
            if let Some(i) = existing {
                self.pending[base + 2 * i as usize + 1] = value_id;
            } else {
                self.pending.push(key_id);
                self.pending.push(value_id);
                count += 1;
            }
        }
        let edge = u32::try_from(self.edges.len()).map_err(|_| P::overflow())?;
        self.edges.extend_from_slice(&self.pending[base..]);
        self.pending.truncate(base);
        self.nodes[id as usize] = Node::Object { edge, len: count };
        self.progress = pos;
        self.size_arenas();
        Ok((id, pos))
    }

    /// Member index whose stored key bytes equal `key_id`'s.
    fn find_member(&self, base: usize, count: u32, key_id: u32) -> Option<u32> {
        let key = self.node_bytes(key_id);
        for i in 0..count as usize {
            let stored = self.pending[base + 2 * i];
            if stored == key_id || self.node_bytes(stored) == key {
                return u32::try_from(i).ok();
            }
        }
        None
    }

    pub(crate) fn columns(&mut self, columns: &Columns<'_>) -> Result<u32, structury::Error> {
        let width = columns.width().max(1);
        let rows = columns.cells().len().checked_div(width).unwrap_or(0);
        // The payload holds each present cell's value plus one key per present
        // cell. Reserve the projection's own size.
        let key_bytes: usize = columns
            .fields()
            .iter()
            .map(String::len)
            .sum::<usize>()
            .saturating_mul(rows);
        let value_bytes: usize = columns
            .cells()
            .iter()
            .map(|cell| match cell {
                structury::ColumnCell::Span(range) => range.end().saturating_sub(range.start()),
                structury::ColumnCell::Absent => 0,
            })
            .sum();
        self.payload.reserve(key_bytes.saturating_add(value_bytes));
        let id = self.push(Node::Array { edge: 0, len: 0 })?;
        let base = self.pending.len();
        if width == 1 && columns.fields().first().map(String::as_str) == Some("$") {
            for cell in columns.cells().iter().take(rows) {
                let child = self.cell(cell)?;
                self.pending.push(child);
            }
        } else {
            for row in 0..rows {
                let object = self.push(Node::Object { edge: 0, len: 0 })?;
                let member_base = self.pending.len();
                let mut count = 0u32;
                for (column, name) in columns.fields().iter().enumerate() {
                    let cell = &columns.cells()[row * width + column];
                    if matches!(cell, structury::ColumnCell::Absent) {
                        continue;
                    }
                    let (key, _) = self.push_string(name.as_bytes(), None)?;
                    let value = self.cell(cell)?;
                    self.pending.push(key);
                    self.pending.push(value);
                    count += 1;
                }
                let edge = u32::try_from(self.edges.len()).map_err(|_| P::overflow())?;
                self.edges.extend_from_slice(&self.pending[member_base..]);
                self.pending.truncate(member_base);
                self.nodes[object as usize] = Node::Object { edge, len: count };
                self.pending.push(object);
            }
        }
        let edge = u32::try_from(self.edges.len()).map_err(|_| P::overflow())?;
        self.edges.extend_from_slice(&self.pending[base..]);
        self.pending.truncate(base);
        // Rows are bounded by the node arena's 32-bit ids; `push` already guards.
        #[allow(clippy::cast_possible_truncation)]
        let len = rows as u32;
        self.nodes[id as usize] = Node::Array { edge, len };
        Ok(id)
    }

    fn cell(&mut self, cell: &structury::ColumnCell) -> Result<u32, structury::Error> {
        match cell {
            structury::ColumnCell::Span(span) => self.span(*span),
            structury::ColumnCell::Absent => self.push(Node::Null),
        }
    }
}
