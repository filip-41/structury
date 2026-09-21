//! Comment facts: role, owner, and optional glyph span for each comment.
//!
//! One linear walk assigns roles and owners. A malformed region stops
//! collection. The validating walk owns error reporting.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use structury::byte_scan::prefix_len;
use structury::{ByteRange, Fact, FactOwner, FactRole};

use crate::dialect::Dialect;
use crate::lex::stop_sets::{CommentStart, Ws};
use crate::lex::{self, Check, MAX_NESTING};

/// Collect comment facts over `[from, to)`, in ascending glyph order.
/// `retain_glyphs` records the authored comment span as `source_span`.
pub(crate) fn collect<'src>(
    bytes: &'src [u8],
    from: usize,
    to: usize,
    dialect: Dialect,
    retain_glyphs: bool,
    out: &mut Vec<Fact<'src>>,
) {
    if !dialect.has_comments() {
        return;
    }
    let end = to.min(bytes.len());
    if from >= end || prefix_len::<CommentStart>(&bytes[from..end]) == end - from {
        return;
    }
    Collector {
        bytes,
        dialect,
        retain_glyphs,
        comments: Vec::new(),
        facts: Vec::new(),
    }
    .run(from, end, out);
}

struct Collector<'src> {
    bytes: &'src [u8],
    dialect: Dialect,
    retain_glyphs: bool,
    comments: Vec<Vec<(usize, usize)>>,
    facts: Vec<Vec<Fact<'src>>>,
}

impl<'src> Collector<'src> {
    fn run(&mut self, from: usize, to: usize, out: &mut Vec<Fact<'src>>) {
        let mut pos = from;
        let mut previous: Option<ByteRange> = None;
        let mut root: Option<ByteRange> = None;
        loop {
            let mut comments = self.comments.pop().unwrap_or_default();
            let Some(next) = read_trivia(self.bytes, pos, to, self.dialect, &mut comments) else {
                self.keep_comments(comments);
                return;
            };
            let starts_value = self.bytes.get(next).is_some_and(|&b| {
                matches!(b, b'n' | b't' | b'f' | b'-' | b'0'..=b'9' | b'"' | b'[' | b'{')
                    || (self.dialect.json5() && matches!(b, b'\'' | b'+' | b'.' | b'I' | b'N'))
            });
            if next >= to || !starts_value {
                let container = root.unwrap_or_else(|| ByteRange::try_new(from, to).expect("ordered"));
                self.emit_trailer(out, &comments, previous, container);
                self.keep_comments(comments);
                return;
            }
            let Some(span) = self.child(next, &mut comments, previous, out) else {
                self.keep_comments(comments);
                return;
            };
            self.keep_comments(comments);
            root.get_or_insert(span);
            previous = Some(span);
            pos = span.end();
        }
    }

    fn container(&mut self, start: usize, closer: u8, out: &mut Vec<Fact<'src>>) -> usize {
        let mut pos = start + 1;
        let mut previous: Option<ByteRange> = None;
        loop {
            let mut comments = self.comments.pop().unwrap_or_default();
            let Some(mut next) = read_trivia(self.bytes, pos, self.bytes.len(), self.dialect, &mut comments) else {
                self.keep_comments(comments);
                return self.bytes.len();
            };
            if self.bytes.get(next) == Some(&b',') {
                if let Some(after) = read_trivia(self.bytes, next + 1, self.bytes.len(), self.dialect, &mut comments) {
                    next = after;
                } else {
                    self.keep_comments(comments);
                    return self.bytes.len();
                }
            }
            if self.bytes.get(next) == Some(&closer) {
                let end = next + 1;
                let container = ByteRange::try_new(start, end).expect("ordered");
                self.emit_trailer(out, &comments, previous, container);
                self.keep_comments(comments);
                return end;
            }
            if closer == b'}' {
                if let Some(after) = self.object_value(next, &mut comments) {
                    next = after;
                } else {
                    self.keep_comments(comments);
                    return self.bytes.len();
                }
            }
            let Some(span) = self.child(next, &mut comments, previous, out) else {
                self.keep_comments(comments);
                return self.bytes.len();
            };
            self.keep_comments(comments);
            previous = Some(span);
            pos = span.end();
        }
    }

    fn child(
        &mut self,
        next: usize,
        comments: &mut Vec<(usize, usize)>,
        previous: Option<ByteRange>,
        out: &mut Vec<Fact<'src>>,
    ) -> Option<ByteRange> {
        if !matches!(self.bytes.get(next), Some(b'[' | b'{')) {
            let span = ByteRange::try_new(next, self.locate(next))?;
            self.emit_gap(out, comments.as_slice(), previous, Some(span));
            return Some(span);
        }
        let mut inner = self.facts.pop().unwrap_or_default();
        let Some(span) = self.span(next, &mut inner) else {
            self.keep_facts(inner);
            return None;
        };
        self.emit_gap(out, comments.as_slice(), previous, Some(span));
        out.append(&mut inner);
        self.keep_facts(inner);
        Some(span)
    }

    /// The span of a container at `start`, with its inner facts appended to `out`.
    /// A container whose interior holds no comment is skipped, not descended.
    fn span(&mut self, start: usize, out: &mut Vec<Fact<'src>>) -> Option<ByteRange> {
        let byte = *self.bytes.get(start)?;
        let end = match byte {
            b'[' | b'{' => {
                let end = self.locate(start);
                if prefix_len::<CommentStart>(&self.bytes[start..end]) == end - start {
                    end
                } else if byte == b'[' {
                    self.container(start, b']', out)
                } else {
                    self.container(start, b'}', out)
                }
            }
            _ => self.locate(start),
        };
        ByteRange::try_new(start, end)
    }

    fn locate(&self, start: usize) -> usize {
        lex::skip_present(self.bytes, start, Check::Locate, MAX_NESTING, self.dialect).unwrap_or(start + 1)
    }

    fn object_value(&self, key: usize, comments: &mut Vec<(usize, usize)>) -> Option<usize> {
        let key_end = lex::skip_key(self.bytes, key, Check::Locate, self.dialect).ok()?;
        let colon = read_trivia(self.bytes, key_end, self.bytes.len(), self.dialect, comments)?;
        if self.bytes.get(colon) != Some(&b':') {
            return None;
        }
        read_trivia(self.bytes, colon + 1, self.bytes.len(), self.dialect, comments)
    }

    /// Emit the comments between `previous` and `next`.
    /// A same-line comment is a foot of `previous`. A comment before `next` is
    /// a lead of `next`.
    fn emit_gap(
        &self,
        out: &mut Vec<Fact<'src>>,
        comments: &[(usize, usize)],
        previous: Option<ByteRange>,
        next: Option<ByteRange>,
    ) {
        for &(start, end) in comments {
            if let Some(previous) = previous
                && same_line_after(self.bytes, previous.end(), start)
            {
                self.push(out, FactRole::CommentFoot, start, end, previous);
            }
            if let Some(next) = next {
                self.push(out, FactRole::CommentLead, start, end, next);
            }
        }
    }

    /// Emit a trailer owned by `container`.
    fn emit_trailer(
        &self,
        out: &mut Vec<Fact<'src>>,
        comments: &[(usize, usize)],
        previous: Option<ByteRange>,
        container: ByteRange,
    ) {
        let role = if previous.is_none() {
            FactRole::CommentInline
        } else {
            FactRole::CommentLead
        };
        for &(start, end) in comments {
            self.push(out, role, start, end, container);
        }
    }

    fn push(&self, out: &mut Vec<Fact<'src>>, role: FactRole, start: usize, end: usize, owner: ByteRange) {
        emit_fact(out, self.bytes, role, start, end, owner, self.retain_glyphs);
    }

    fn keep_comments(&mut self, mut buffer: Vec<(usize, usize)>) {
        buffer.clear();
        self.comments.push(buffer);
    }

    fn keep_facts(&mut self, mut buffer: Vec<Fact<'src>>) {
        buffer.clear();
        self.facts.push(buffer);
    }
}

/// Read whitespace and comments from `pos`, collecting comment spans in `out`.
/// `None` on a malformed comment or one past `to`.
fn read_trivia(bytes: &[u8], pos: usize, to: usize, dialect: Dialect, out: &mut Vec<(usize, usize)>) -> Option<usize> {
    let end = to.min(bytes.len());
    let mut cursor = pos;
    loop {
        cursor += prefix_len::<Ws>(&bytes[cursor..end]);
        if cursor >= end || !(dialect.has_comments() && bytes[cursor] == b'/') {
            return Some(cursor);
        }
        let close = lex::skip_comment(bytes, cursor).ok()?;
        if close > end {
            return None;
        }
        out.push((cursor, close));
        cursor = close;
    }
}

/// Comment body without its `//` or `/* */` delimiters.
fn comment_body(bytes: &[u8], start: usize, end: usize) -> Cow<'_, str> {
    let inner = if bytes.get(start + 1) == Some(&b'/') {
        bytes.get(start + 2..end).unwrap_or(&[])
    } else {
        bytes.get(start + 2..end.saturating_sub(2)).unwrap_or(&[])
    };
    // A comment body is normally valid UTF-8, so the borrow covers the hot path.
    match core::str::from_utf8(inner) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(String::from_utf8_lossy(inner).into_owned()),
    }
}

/// Whether only whitespace and a separator sit between `value_end` and
/// `comment_start` on the same line.
fn same_line_after(bytes: &[u8], value_end: usize, comment_start: usize) -> bool {
    bytes
        .get(value_end..comment_start)
        .is_some_and(|gap| !gap.contains(&b'\n') && gap.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\r' | b',')))
}

/// Where the validating walk feeds the comment trivia it skips.
pub(crate) trait RecordSink: Sized {
    /// Whether this instantiation records. Constant per monomorphization.
    const RECORDING: bool;

    /// Build the sink. `retain_glyphs` keeps authored spans for Strict.
    fn new(retain_glyphs: bool) -> Self;
    /// The sibling value the trivia about to be skipped follows, if any.
    fn set_previous(&mut self, previous: Option<ByteRange>);
    /// One comment span inside the trivia the caller skipped, in authored order.
    fn note_comment(&mut self, start: usize, end: usize);
    /// Close the open gap against the value at `span`, before the value's own
    /// facts are recorded.
    fn begin_gap(&mut self) -> Option<usize>;
    /// Fill the following-value span of the event `begin_gap` opened.
    fn fill_next(&mut self, event: Option<usize>, span: ByteRange);
    /// Close the open gap as a container trailer owned by `container`.
    fn trailer(&mut self, container: ByteRange);
    /// Materialize the recorded facts in authored order.
    fn take_facts<'src>(&mut self, bytes: &'src [u8]) -> Vec<Fact<'src>>;
}

/// Facts-off sink: every operation is a no-op.
#[derive(Clone, Copy, Default)]
pub(crate) struct NoRec;

impl RecordSink for NoRec {
    const RECORDING: bool = false;

    #[inline]
    fn new(_retain_glyphs: bool) -> Self {
        Self
    }

    #[inline]
    fn set_previous(&mut self, _previous: Option<ByteRange>) {}

    #[inline]
    fn note_comment(&mut self, _start: usize, _end: usize) {}

    #[inline]
    fn begin_gap(&mut self) -> Option<usize> {
        None
    }

    #[inline]
    fn fill_next(&mut self, _event: Option<usize>, _span: ByteRange) {}

    #[inline]
    fn trailer(&mut self, _container: ByteRange) {}

    #[inline]
    fn take_facts<'src>(&mut self, _bytes: &'src [u8]) -> Vec<Fact<'src>> {
        Vec::new()
    }
}

/// One run of trivia and the node its facts attach to.
struct Commentary {
    /// Half-open slice of [`Recorder::comments`].
    comments: core::ops::Range<usize>,
    previous: Option<ByteRange>,
    /// Following value; `None` for a trailer.
    next: Option<ByteRange>,
    /// Enclosing container; `None` for a gap before a value.
    container: Option<ByteRange>,
}

/// Fused recorder: the validating walk reports each comment as it is skipped.
/// This sink materializes the facts the standalone [`collect`] oracle would.
pub(crate) struct Recorder {
    comments: Vec<(usize, usize)>,
    /// Start of the currently open gap in [`Self::comments`].
    open_start: usize,
    previous: Option<ByteRange>,
    events: Vec<Commentary>,
    retain_glyphs: bool,
}

impl Recorder {
    /// Record the comment spans in `[start, end)` as trivia.
    fn note_range(&mut self, bytes: &[u8], start: usize, end: usize) {
        // Every comment starts with `/`; the SIMD prefilter skips the
        // whitespace between comments in one run.
        let end = end.min(bytes.len());
        let mut pos = start;
        while pos < end {
            pos += prefix_len::<CommentStart>(&bytes[pos..end]);
            if pos >= end {
                break;
            }
            match lex::skip_comment(bytes, pos) {
                Ok(close) if close <= end => {
                    self.note_comment(pos, close);
                    pos = close;
                }
                _ => break,
            }
        }
    }
}

impl RecordSink for Recorder {
    const RECORDING: bool = true;

    fn new(retain_glyphs: bool) -> Self {
        Self {
            comments: Vec::new(),
            open_start: 0,
            previous: None,
            events: Vec::new(),
            retain_glyphs,
        }
    }

    fn set_previous(&mut self, previous: Option<ByteRange>) {
        self.previous = previous;
    }

    fn note_comment(&mut self, start: usize, end: usize) {
        self.comments.push((start, end));
    }

    fn begin_gap(&mut self) -> Option<usize> {
        if self.comments.len() == self.open_start {
            return None;
        }
        let index = self.events.len();
        self.events.push(Commentary {
            comments: self.open_start..self.comments.len(),
            previous: self.previous,
            next: None,
            container: None,
        });
        self.open_start = self.comments.len();
        Some(index)
    }

    fn fill_next(&mut self, event: Option<usize>, span: ByteRange) {
        if let Some(index) = event {
            self.events[index].next = Some(span);
        }
    }

    fn trailer(&mut self, container: ByteRange) {
        if self.comments.len() == self.open_start {
            return;
        }
        self.events.push(Commentary {
            comments: self.open_start..self.comments.len(),
            previous: self.previous,
            next: None,
            container: Some(container),
        });
        self.open_start = self.comments.len();
    }

    fn take_facts<'src>(&mut self, bytes: &'src [u8]) -> Vec<Fact<'src>> {
        // A gap comment can emit both a foot and a lead. Reserve that upper bound.
        let mut out = Vec::with_capacity(self.comments.len() * 2);
        for event in core::mem::take(&mut self.events) {
            for &(start, end) in &self.comments[event.comments] {
                // Only a gap before a value has a foot. A trailer leads its container.
                if event.container.is_none()
                    && let Some(previous) = event.previous
                    && same_line_after(bytes, previous.end(), start)
                {
                    emit_fact(
                        &mut out,
                        bytes,
                        FactRole::CommentFoot,
                        start,
                        end,
                        previous,
                        self.retain_glyphs,
                    );
                }
                let (role, owner) = match (event.next, event.container) {
                    (Some(next), _) => (FactRole::CommentLead, next),
                    (None, Some(container)) if event.previous.is_none() => (FactRole::CommentInline, container),
                    (None, Some(container)) => (FactRole::CommentLead, container),
                    (None, None) => continue,
                };
                emit_fact(&mut out, bytes, role, start, end, owner, self.retain_glyphs);
            }
        }
        self.comments.clear();
        self.open_start = 0;
        out
    }
}

/// Facts for the pre-root trivia `[from, to)`: every comment leads `root`.
pub(crate) fn leading_facts(
    bytes: &[u8],
    from: usize,
    to: usize,
    root: ByteRange,
    retain_glyphs: bool,
) -> Vec<Fact<'_>> {
    let mut rec = Recorder::new(retain_glyphs);
    rec.note_range(bytes, from, to);
    let gap = rec.begin_gap();
    rec.fill_next(gap, root);
    rec.take_facts(bytes)
}

/// Facts for the trailing top-level trivia `[from, to)`.
pub(crate) fn trailing_facts(
    bytes: &[u8],
    from: usize,
    to: usize,
    root: ByteRange,
    retain_glyphs: bool,
) -> Vec<Fact<'_>> {
    let mut rec = Recorder::new(retain_glyphs);
    rec.set_previous(Some(root));
    rec.note_range(bytes, from, to);
    rec.trailer(root);
    rec.take_facts(bytes)
}

/// Push one comment fact. Drops an unordered glyph span.
fn emit_fact<'src>(
    out: &mut Vec<Fact<'src>>,
    bytes: &'src [u8],
    role: FactRole,
    start: usize,
    end: usize,
    owner: ByteRange,
    retain_glyphs: bool,
) {
    let Some(glyph) = ByteRange::try_new(start, end) else {
        return;
    };
    out.push(Fact::new(
        role,
        comment_body(bytes, start, end),
        retain_glyphs.then_some(glyph),
        FactOwner::node(owner),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;
    use crate::lex::MAX_NESTING;
    use crate::scan::{JsonInput, ScanRequest, scan};
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::fmt::Write as _;
    use structury::{Answer, Demand, Fact, Strictness};

    fn fused(src: &[u8], strictness: Strictness, dialect: Dialect) -> Vec<Fact<'_>> {
        let req = ScanRequest {
            input: JsonInput::Text,
            demands: &[Demand::Whole],
            strictness,
            max_nesting: MAX_NESTING,
            dialect,
            facts: true,
        };
        let result = scan(src, &req).expect("fused scan");
        let Answer::Document(doc) = &result.answers[0] else {
            panic!("fused scan did not yield a document");
        };
        doc.facts().to_vec()
    }

    fn assert_fused_matches_oracle(name: &str, src: &[u8], dialect: Dialect) {
        for strictness in [Strictness::Lazy, Strictness::Structural, Strictness::Strict] {
            let mut oracle = Vec::new();
            collect(
                src,
                0,
                src.len(),
                dialect,
                matches!(strictness, Strictness::Strict),
                &mut oracle,
            );
            let got = fused(src, strictness, dialect);
            assert_eq!(got, oracle, "{name} {dialect:?} {strictness:?}");
        }
    }

    /// Cases valid under both JSONC and JSON5, so the dialect axis reads the same trivia.
    fn common_cases() -> Vec<(String, Vec<u8>)> {
        vec![
            (
                "lead/interstitial/trail".into(),
                b"// lead\n[ 1/* inner */, 2 ] // trail\n".to_vec(),
            ),
            (
                "leading comment lines".into(),
                b"// a\n// b\n[1,2]// c\n// d\n".to_vec(),
            ),
            ("empty array inline".into(), b"[/* only */]".to_vec()),
            ("empty object inline".into(), b"{ /* only */ }".to_vec()),
            ("key-colon gap".into(), b"{\"a\"/*k*/: /*v*/1}".to_vec()),
            ("after colon".into(), b"{\"a\":/*v*/1}".to_vec()),
            ("before closer".into(), b"{\"a\":1 /* t */}".to_vec()),
            ("after trailing comma".into(), b"{\"a\":1, /* tc */}".to_vec()),
            ("array trailing comma".into(), b"[1, /* c */ ]".to_vec()),
            ("two comments one gap".into(), b"[1/*a*//*b*/,2]".to_vec()),
            ("escaped string".into(), b"[/*a*/\"x\\\"y\"/*b*/,/*c*/2]".to_vec()),
            ("foot then newline".into(), b"{\"a\":1 // foot\n,\"b\":2}".to_vec()),
            (
                "between values object".into(),
                b"{\"a\":1, /* between */ \"b\":2}".to_vec(),
            ),
            ("nested".into(), b"{\"a\":[1,/*b*/2,{\"c\"/*d*/:3}]//e\n}".to_vec()),
            ("users".into(), shape_users()),
            ("wide".into(), shape_wide()),
            ("deep".into(), shape_deep()),
            ("blob".into(), shape_blob()),
        ]
    }

    /// Sources that only JSON5 accepts, exercising trivia beside JSON5 tokens.
    fn json5_cases() -> Vec<(String, Vec<u8>)> {
        vec![
            ("unquoted key".into(), b"{a:1,/*c*/b:'x'}".to_vec()),
            ("hex values".into(), b"[0x1,/*h*/0x2,/*t*/]".to_vec()),
            ("key colon json5".into(), b"{unquoted:/*k*/'v'}".to_vec()),
        ]
    }

    fn shape_users() -> Vec<u8> {
        let mut out = String::from("// users\n[");
        for i in 0..8 {
            if i > 0 {
                out.push(',');
            }
            if i % 3 == 0 {
                out.push_str("\n// next row\n");
            }
            write!(out, "/*row*/{{\"id\":{i},\"name\":\"u{i}\"}}").expect("write to String");
        }
        out.push_str("] // end\n");
        out.into_bytes()
    }

    fn shape_wide() -> Vec<u8> {
        let mut out = String::from("{\n");
        for i in 0..24 {
            if i > 0 {
                out.push_str(",\n");
            }
            if i % 4 == 0 {
                out.push_str("  // field\n");
            }
            write!(out, "  \"k{i}\": {i} /* v{i} */").expect("write to String");
        }
        out.push_str("\n} // done\n");
        out.into_bytes()
    }

    fn shape_deep() -> Vec<u8> {
        let mut out = String::new();
        for d in 0..12 {
            out.push('[');
            if d % 2 == 0 {
                out.push_str("/*d*/");
            }
        }
        out.push('1');
        for _ in 0..12 {
            out.push_str("/*u*/]");
        }
        out.into_bytes()
    }

    fn shape_blob() -> Vec<u8> {
        let blob = "x".repeat(200);
        format!("{{/*pre*/\"data\":\"{blob}\"/*mid*/,/*post*/\"n\":1}} // tail\n").into_bytes()
    }

    #[test]
    fn fused_facts_match_collector() {
        for (name, src) in common_cases() {
            for dialect in [Dialect::Jsonc, Dialect::Json5] {
                assert_fused_matches_oracle(&name, &src, dialect);
            }
        }
        for (name, src) in json5_cases() {
            assert_fused_matches_oracle(&name, &src, Dialect::Json5);
        }
    }

    /// A keyed `Path` co-demanded with a root `Whole` still matches on the key under
    /// the fused facts walk: the Record-only key-skip must not swallow it.
    #[test]
    fn fused_facts_keep_a_co_demanded_key_path() {
        let src = br#"{"a":1,"b":2}"#;
        let demands = [
            Demand::Whole,
            Demand::path(vec![structury::Step::Key("a".into())]),
            Demand::path(vec![structury::Step::Key("b".into())]),
        ];
        let req = ScanRequest {
            input: JsonInput::Text,
            demands: &demands,
            strictness: Strictness::Strict,
            max_nesting: MAX_NESTING,
            dialect: Dialect::Jsonc,
            facts: true,
        };
        let result = scan(src, &req).expect("fused scan");
        let spans: Vec<Option<(usize, usize)>> = result
            .answers
            .iter()
            .map(|answer| match answer {
                Answer::Document(doc) => Some((doc.root().start(), doc.root().end())),
                Answer::Missing => None,
                _ => panic!("unexpected answer"),
            })
            .collect();
        assert_eq!(spans, vec![Some((0, 13)), Some((5, 6)), Some((11, 12))]);
    }
}
