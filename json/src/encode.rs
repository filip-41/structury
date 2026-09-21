//! Compact, pretty, and verbatim-when-attested encode.
//! Authored number spellings (`1.50`, `-0`) survive.
//! A JSON5 `Infinity` / `NaN` encodes only under [`Dialect::Json5`].

use alloc::vec::Vec;

use structury::byte_scan::prefix_len;
use structury::{ByteRange, Document, Fact, FactRole, Number, Value};

use crate::dialect::Dialect;
use crate::error;
use crate::lex::MAX_NESTING;
use crate::lex::stop_sets::PlainString;

/// Pretty indent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Indent {
    /// N spaces.
    Spaces(u8),
    /// One tab.
    Tab,
}

/// How to write one encoded item into a stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ItemFraming {
    /// No terminator.
    None,
    /// NDJSON `\n`.
    NdjsonLf,
    /// RFC 7464 record: `0x1E` prefix + `\n`.
    JsonSeq,
}

/// Encode knobs, owned by the encode step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
#[allow(clippy::struct_excessive_bools, reason = "independent output flags set via builders")]
pub struct EncodeOptions {
    /// Pretty-print when true.
    pub pretty: bool,
    /// Indent unit when pretty.
    pub indent: Indent,
    /// Item terminator.
    pub framing: ItemFraming,
    /// Memcpy attested canonical spans when the demand proved them.
    pub verbatim: bool,
    /// Source dialect token-copy encoding reads.
    pub dialect: Dialect,
    /// Sort object members by key on a [`Source::Value`] write.
    pub sort_keys: bool,
    /// Escape non-ASCII as `\uXXXX` on a [`Source::Value`] write.
    pub ascii: bool,
}

impl EncodeOptions {
    /// Compact, no framing, verbatim when attested.
    #[must_use]
    pub const fn compact() -> Self {
        Self {
            pretty: false,
            indent: Indent::Spaces(2),
            framing: ItemFraming::None,
            verbatim: true,
            dialect: Dialect::Rfc8259,
            sort_keys: false,
            ascii: false,
        }
    }

    /// Pretty two-space indent.
    #[must_use]
    pub const fn pretty() -> Self {
        Self {
            pretty: true,
            indent: Indent::Spaces(2),
            framing: ItemFraming::None,
            verbatim: false,
            dialect: Dialect::Rfc8259,
            sort_keys: false,
            ascii: false,
        }
    }

    /// These options reading `dialect`.
    #[must_use]
    pub const fn with_dialect(mut self, dialect: Dialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// These options pretty-printing (or not).
    #[must_use]
    pub const fn with_pretty(mut self, pretty: bool) -> Self {
        self.pretty = pretty;
        self
    }

    /// These options indenting with `indent`.
    #[must_use]
    pub const fn with_indent(mut self, indent: Indent) -> Self {
        self.indent = indent;
        self
    }

    /// These options terminating each written item with `framing`.
    #[must_use]
    pub const fn with_framing(mut self, framing: ItemFraming) -> Self {
        self.framing = framing;
        self
    }

    /// These options memcpying attested canonical spans (or not).
    #[must_use]
    pub const fn with_verbatim(mut self, verbatim: bool) -> Self {
        self.verbatim = verbatim;
        self
    }

    /// These options sorting object members by key on a value write (or not).
    #[must_use]
    pub const fn with_sort_keys(mut self, sort_keys: bool) -> Self {
        self.sort_keys = sort_keys;
        self
    }

    /// These options escaping non-ASCII as `\uXXXX` on a value write (or not).
    #[must_use]
    pub const fn with_ascii(mut self, ascii: bool) -> Self {
        self.ascii = ascii;
        self
    }
}

impl Default for EncodeOptions {
    /// [`Self::compact`]: verbatim, no framing, RFC 8259.
    fn default() -> Self {
        Self::compact()
    }
}

/// Request: the input a write consumes, selected by the caller.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Source<'a, 'src> {
    /// Owned mutable tree: a canonical walk.
    Value(&'a Value),
    /// A [`Document`]. Refused unless fully validated (the write gate).
    /// A byte-preserving write requires the document grammar to match `opts.dialect`.
    Document(&'a Document<'src>),
}

/// Encode one value or document under `opts` into `out`.
/// A [`Source::Document`] memcpys when `opts.verbatim`.
///
/// # Errors
///
/// `Infinity` / `-Infinity` / `NaN` outside [`Dialect::Json5`], a
/// [`Source::Document`] that was not fully validated
/// ([`structury::ErrorClass::Write`]), or a byte-preserving write whose document
/// was validated under another dialect (`write` / `dialect-mismatch`).
pub fn encode(source: Source<'_, '_>, opts: &EncodeOptions, out: &mut Vec<u8>) -> Result<(), structury::Error> {
    if opts.framing == ItemFraming::JsonSeq {
        out.push(0x1E);
    }
    match source {
        Source::Value(value) => write_value(value, *opts, out, 0)?,
        Source::Document(doc) => write_document(doc, *opts, out)?,
    }
    match opts.framing {
        ItemFraming::None => {}
        ItemFraming::NdjsonLf | ItemFraming::JsonSeq => out.push(b'\n'),
    }
    Ok(())
}

/// Encode one value under `opts` into fresh bytes. Allocates.
///
/// # Errors
///
/// `Infinity` / `-Infinity` / `NaN` outside [`Dialect::Json5`], or nesting past
/// [`MAX_NESTING`](crate::lex::MAX_NESTING).
pub fn encode_value(value: &Value, opts: &EncodeOptions) -> Result<Vec<u8>, structury::Error> {
    let mut out = Vec::new();
    encode(Source::Value(value), opts, &mut out)?;
    Ok(out)
}

/// Encode one document under `opts` into fresh bytes. Allocates.
///
/// # Errors
///
/// Same class as [`encode`].
pub fn encode_document(doc: &Document<'_>, opts: &EncodeOptions) -> Result<Vec<u8>, structury::Error> {
    let mut out = Vec::new();
    encode(Source::Document(doc), opts, &mut out)?;
    Ok(out)
}

fn write_document(doc: &Document<'_>, opts: EncodeOptions, out: &mut Vec<u8>) -> Result<(), structury::Error> {
    if !doc.is_fully_validated() {
        return Err(error::write(
            "encode refuses a document that was not fully validated",
            0,
        ));
    }
    if opts.verbatim && !opts.pretty {
        // Copying under another grammar could emit spellings the requested
        // grammar does not admit. A caller that wants the rewrite sets
        // `verbatim(false)`.
        if doc.grammar() != opts.dialect.grammar_tag() {
            return Err(error::dialect_mismatch(0));
        }
        out.extend_from_slice(doc.root_bytes());
    } else {
        let pretty = opts.pretty.then_some(&opts);
        emit_span(
            doc.source(),
            doc.root().start(),
            doc.root().end(),
            pretty,
            opts.dialect,
            doc.facts(),
            out,
        )?;
    }
    Ok(())
}

pub(crate) fn write_value(
    value: &Value,
    opts: EncodeOptions,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), structury::Error> {
    if depth >= MAX_NESTING as usize {
        return Err(error::limit(0));
    }
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n) => {
            if let Number::NonFinite(value) = n {
                let Some(spelling) = opts.dialect.non_finite_spelling(*value) else {
                    return Err(error::write("non-finite number requires a dialect that admits it", 0));
                };
                out.extend_from_slice(spelling.as_bytes());
            } else {
                out.extend_from_slice(n.spelling().as_bytes());
            }
        }
        Value::Str(s) => write_string(s, opts.ascii, out),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                if opts.pretty {
                    newline_indent(out, opts, depth + 1);
                }
                write_value(item, opts, out, depth + 1)?;
            }
            if opts.pretty && !items.is_empty() {
                newline_indent(out, opts, depth);
            }
            out.push(b']');
        }
        Value::Object(members) => {
            out.push(b'{');
            if opts.sort_keys {
                let mut order: Vec<usize> = (0..members.len()).collect();
                order.sort_by(|&a, &b| members[a].0.cmp(&members[b].0));
                for (n, i) in order.iter().enumerate() {
                    let (k, v) = &members[*i];
                    write_member(k, v, opts, out, depth, n == 0)?;
                }
            } else {
                for (i, (k, v)) in members.iter().enumerate() {
                    write_member(k, v, opts, out, depth, i == 0)?;
                }
            }
            if opts.pretty && !members.is_empty() {
                newline_indent(out, opts, depth);
            }
            out.push(b'}');
        }
    }
    Ok(())
}

/// Write one object member. `first` skips the leading comma.
fn write_member(
    key: &str,
    value: &Value,
    opts: EncodeOptions,
    out: &mut Vec<u8>,
    depth: usize,
    first: bool,
) -> Result<(), structury::Error> {
    if !first {
        out.push(b',');
    }
    if opts.pretty {
        newline_indent(out, opts, depth + 1);
    }
    write_string(key, opts.ascii, out);
    out.push(b':');
    if opts.pretty {
        out.push(b' ');
    }
    write_value(value, opts, out, depth + 1)
}

pub(crate) fn newline_indent(out: &mut Vec<u8>, opts: EncodeOptions, depth: usize) {
    out.push(b'\n');
    match opts.indent {
        Indent::Spaces(n) => out.resize(out.len() + depth * usize::from(n), b' '),
        Indent::Tab => out.resize(out.len() + depth, b'\t'),
    }
}

pub(crate) fn write_string(s: &str, ascii: bool, out: &mut Vec<u8>) {
    let bytes = s.as_bytes();
    if bytes.len() < 24
        && bytes
            .iter()
            .all(|&b| (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\')
    {
        out.push(b'"');
        out.extend_from_slice(bytes);
        out.push(b'"');
        return;
    }
    out.push(b'"');
    let mut i = 0;
    while i < bytes.len() {
        let run = prefix_len::<PlainString>(&bytes[i..]);
        if run > 0 {
            out.extend_from_slice(&bytes[i..i + run]);
            i += run;
            continue;
        }
        let Some(ch) = s.get(i..).and_then(|rest| rest.chars().next()) else {
            break;
        };
        i += ch.len_utf8();
        match ch {
            '"' => out.extend_from_slice(br#"\""#),
            '\\' => out.extend_from_slice(br"\\"),
            '\u{0008}' => out.extend_from_slice(br"\b"),
            '\u{000c}' => out.extend_from_slice(br"\f"),
            '\n' => out.extend_from_slice(br"\n"),
            '\r' => out.extend_from_slice(br"\r"),
            '\t' => out.extend_from_slice(br"\t"),
            c if (c as u32) < 0x20 => {
                let n = c as u32;
                let hex = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    hex_nibble((n >> 4 & 0xf) as u8),
                    hex_nibble((n & 0xf) as u8),
                ];
                out.extend_from_slice(&hex);
            }
            c => {
                if ascii && (c as u32) > 0x7f {
                    push_u16_escaped(out, c);
                } else {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
    }
    out.push(b'"');
}

const fn hex_nibble(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'a' + (n - 10) }
}

/// Write `c` as `\uXXXX`, with a surrogate pair past the BMP.
fn push_u16_escaped(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u16; 2];
    for unit in c.encode_utf16(&mut buf) {
        push_u16(out, *unit);
    }
}

fn push_u16(out: &mut Vec<u8>, n: u16) {
    out.extend_from_slice(&[
        b'\\',
        b'u',
        hex_nibble((n >> 12 & 0xf) as u8),
        hex_nibble((n >> 8 & 0xf) as u8),
        hex_nibble((n >> 4 & 0xf) as u8),
        hex_nibble((n & 0xf) as u8),
    ]);
}

/// Emit the value in `[from, to)` and the facts attached inside it.
/// Compact output never exceeds its source. Pretty reserves a half allowance
/// for the markup it adds.
fn emit_span<'a>(
    bytes: &'a [u8],
    from: usize,
    to: usize,
    opts: Option<&EncodeOptions>,
    dialect: Dialect,
    facts: &'a [Fact<'_>],
    out: &mut Vec<u8>,
) -> Result<(), structury::Error> {
    let len = to.saturating_sub(from);
    let reserve = if opts.is_some() {
        len.saturating_add(len / 2)
    } else {
        len
    };
    out.reserve(reserve.max(16));
    let mut cursor = FactCursor::new(bytes, facts, from, to);
    let pos = crate::lex::skip_trivia(bytes, from, dialect)?;
    cursor.flush(out, pos);
    let end = emit(bytes, pos, opts, dialect, &mut cursor, out, 0)?;
    let tail = crate::lex::skip_trivia(bytes, end, dialect)?;
    cursor.flush(out, tail.min(to));
    if tail < to {
        return Err(error::trailing_content(tail));
    }
    Ok(())
}

struct FactCursor<'a, 'src> {
    bytes: &'a [u8],
    facts: &'a [Fact<'src>],
    order: Vec<usize>,
    next: usize,
    last_glyph: Option<ByteRange>,
}

impl<'a, 'src> FactCursor<'a, 'src> {
    fn new(bytes: &'a [u8], facts: &'a [Fact<'src>], from: usize, to: usize) -> Self {
        let mut order: Vec<usize> = (0..facts.len())
            .filter(|&index| within(&facts[index], from, to))
            .collect();
        order.sort_by_key(|&index| position(&facts[index]));
        Self {
            bytes,
            facts,
            order,
            next: 0,
            last_glyph: None,
        }
    }

    /// Write every pending fact at or before `upto`.
    fn flush(&mut self, out: &mut Vec<u8>, upto: usize) {
        while let Some(&index) = self.order.get(self.next) {
            let fact = &self.facts[index];
            if position(fact) > upto {
                break;
            }
            self.next += 1;
            if fact.source_span().is_some() && fact.source_span() == self.last_glyph {
                continue;
            }
            self.last_glyph = fact.source_span();
            self.write(out, fact);
        }
    }

    fn write(&self, out: &mut Vec<u8>, fact: &Fact<'_>) {
        if let Some(span) = fact.source_span() {
            out.extend_from_slice(self.bytes.get(span.start()..span.end()).unwrap_or_default());
            // A line comment needs a terminator before the next token.
            if self.bytes.get(span.start()..span.start() + 2) == Some(b"//") {
                out.push(b'\n');
            }
        } else {
            out.extend_from_slice(b"//");
            out.extend_from_slice(fact.text().as_bytes());
            out.push(b'\n');
        }
    }
}

/// Whether a fact's glyph (its owner when it has none) lies inside `[from, to)`.
pub(crate) fn within(fact: &Fact<'_>, from: usize, to: usize) -> bool {
    if let Some(span) = fact.source_span() {
        span.start() >= from && span.end() <= to
    } else {
        let owner = fact.owner().span();
        owner.start() >= from && owner.end() <= to
    }
}

/// Where a canonical walk emits a fact.
fn position(fact: &Fact<'_>) -> usize {
    match fact.source_span() {
        Some(span) => span.end(),
        None => match fact.role() {
            FactRole::CommentFoot => fact.owner().span().end(),
            FactRole::CommentLead | FactRole::CommentInline => fact.owner().span().start(),
        },
    }
}

fn emit(
    bytes: &[u8],
    pos: usize,
    opts: Option<&EncodeOptions>,
    dialect: Dialect,
    facts: &mut FactCursor<'_, '_>,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<usize, structury::Error> {
    let pos = crate::lex::skip_trivia(bytes, pos, dialect)?;
    facts.flush(out, pos);
    let Some(&byte) = bytes.get(pos) else {
        return Err(error::expected_value(pos));
    };
    match byte {
        b'n' | b't' | b'f' | b'-' | b'0'..=b'9' => {
            let end = crate::lex::skip_present(bytes, pos, crate::lex::Check::Locate, MAX_NESTING, dialect)?;
            out.extend_from_slice(&bytes[pos..end]);
            Ok(end)
        }
        b'+' | b'.' | b'I' | b'N' if dialect.json5() => {
            let end = crate::lex::skip_present(bytes, pos, crate::lex::Check::Locate, MAX_NESTING, dialect)?;
            out.extend_from_slice(&bytes[pos..end]);
            Ok(end)
        }
        b'"' => {
            let end = crate::lex::skip_string_locate(bytes, pos, dialect)?;
            out.extend_from_slice(&bytes[pos..end]);
            Ok(end)
        }
        b'\'' if dialect.json5() => {
            let end = crate::lex::skip_string_locate(bytes, pos, dialect)?;
            out.extend_from_slice(&bytes[pos..end]);
            Ok(end)
        }
        b'[' => emit_array(bytes, pos, opts, dialect, facts, out, depth),
        b'{' => emit_object(bytes, pos, opts, dialect, facts, out, depth),
        _ => Err(error::expected_value(pos)),
    }
}

fn indent(out: &mut Vec<u8>, opts: Option<&EncodeOptions>, depth: usize) {
    if let Some(opts) = opts {
        newline_indent(out, *opts, depth);
    }
}

fn emit_array(
    bytes: &[u8],
    pos: usize,
    opts: Option<&EncodeOptions>,
    dialect: Dialect,
    facts: &mut FactCursor<'_, '_>,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<usize, structury::Error> {
    if depth >= MAX_NESTING as usize {
        return Err(error::limit(pos));
    }
    out.push(b'[');
    let mut cursor = crate::lex::skip_trivia(bytes, pos + 1, dialect)?;
    facts.flush(out, cursor);
    if bytes.get(cursor) == Some(&b']') {
        out.push(b']');
        return Ok(cursor + 1);
    }
    let mut first = true;
    loop {
        cursor = crate::lex::skip_trivia(bytes, cursor, dialect)?;
        facts.flush(out, cursor);
        let Some(&byte) = bytes.get(cursor) else {
            return Err(error::expected_comma_array(cursor));
        };
        if byte == b']' {
            indent(out, opts, depth);
            out.push(b']');
            return Ok(cursor + 1);
        }
        if !first {
            if byte != b',' {
                return Err(error::expected_comma_array(cursor));
            }
            out.push(b',');
            cursor = crate::lex::skip_trivia(bytes, cursor + 1, dialect)?;
            facts.flush(out, cursor);
            if bytes.get(cursor) == Some(&b']') {
                if dialect.trailing_commas() {
                    indent(out, opts, depth);
                    out.push(b']');
                    return Ok(cursor + 1);
                }
                return Err(error::trailing_comma(cursor));
            }
        }
        indent(out, opts, depth + 1);
        cursor = emit(bytes, cursor, opts, dialect, facts, out, depth + 1)?;
        first = false;
    }
}

fn emit_object(
    bytes: &[u8],
    pos: usize,
    opts: Option<&EncodeOptions>,
    dialect: Dialect,
    facts: &mut FactCursor<'_, '_>,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<usize, structury::Error> {
    if depth >= MAX_NESTING as usize {
        return Err(error::limit(pos));
    }
    out.push(b'{');
    let mut cursor = crate::lex::skip_trivia(bytes, pos + 1, dialect)?;
    facts.flush(out, cursor);
    if bytes.get(cursor) == Some(&b'}') {
        out.push(b'}');
        return Ok(cursor + 1);
    }
    let mut first = true;
    loop {
        cursor = crate::lex::skip_trivia(bytes, cursor, dialect)?;
        facts.flush(out, cursor);
        let Some(&byte) = bytes.get(cursor) else {
            return Err(error::expected_key(cursor));
        };
        if byte == b'}' {
            indent(out, opts, depth);
            out.push(b'}');
            return Ok(cursor + 1);
        }
        if !first {
            if byte != b',' {
                return Err(error::expected_comma_object(cursor));
            }
            out.push(b',');
            cursor = crate::lex::skip_trivia(bytes, cursor + 1, dialect)?;
            facts.flush(out, cursor);
            if bytes.get(cursor) == Some(&b'}') {
                if dialect.trailing_commas() {
                    indent(out, opts, depth);
                    out.push(b'}');
                    return Ok(cursor + 1);
                }
                return Err(error::trailing_comma(cursor));
            }
        }
        indent(out, opts, depth + 1);
        let key_end = crate::lex::skip_key(bytes, cursor, crate::lex::Check::Locate, dialect)?;
        out.extend_from_slice(&bytes[cursor..key_end]);
        if opts.is_some() {
            out.extend_from_slice(b": ");
        } else {
            out.push(b':');
        }
        cursor = crate::lex::skip_trivia(bytes, key_end, dialect)?;
        facts.flush(out, cursor);
        if bytes.get(cursor) != Some(&b':') {
            return Err(error::expected_colon(cursor));
        }
        cursor = crate::lex::skip_trivia(bytes, cursor + 1, dialect)?;
        facts.flush(out, cursor);
        cursor = emit(bytes, cursor, opts, dialect, facts, out, depth + 1)?;
        first = false;
    }
}
