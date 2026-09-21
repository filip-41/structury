//! JSON stop sets for [`structury::byte_scan::prefix_len`].

use structury::byte_scan::{StopSet, prefix_len, sealed::Sealed};

/// Decode-side string CONTENT run: `"`, `\`, C0 control, or DEL.
#[derive(Clone, Copy)]
pub struct StringContent;
impl StopSet for StringContent {
    const EQ: [u8; 8] = [b'"', b'\\', 0x7f, 0, 0, 0, 0, 0];
    const EQ_LEN: u8 = 3;
    const LT: Option<u8> = Some(0x20);
    const GE: Option<u8> = None;
    const ALL: bool = false;
}

/// Bound-source string run: `"`, `\`, C0, DEL, or non-ASCII.
#[derive(Clone, Copy)]
pub struct PlainString;
impl StopSet for PlainString {
    const EQ: [u8; 8] = [b'"', b'\\', 0x7f, 0, 0, 0, 0, 0];
    const EQ_LEN: u8 = 3;
    const LT: Option<u8> = Some(0x20);
    const GE: Option<u8> = Some(0x80);
    const ALL: bool = false;
}

/// RFC 8259 whitespace.
#[derive(Clone, Copy)]
pub struct Ws;
impl StopSet for Ws {
    const EQ: [u8; 8] = [b' ', b'\t', b'\n', b'\r', 0, 0, 0, 0];
    const EQ_LEN: u8 = 4;
    const LT: Option<u8> = None;
    const GE: Option<u8> = None;
    const ALL: bool = true;
}

/// NDJSON record terminators.
#[derive(Clone, Copy)]
pub struct NdjsonFrame;
impl StopSet for NdjsonFrame {
    const EQ: [u8; 8] = [b'\n', b'\r', 0, 0, 0, 0, 0, 0];
    const EQ_LEN: u8 = 2;
    const LT: Option<u8> = None;
    const GE: Option<u8> = None;
    const ALL: bool = false;
}

/// A stop set that halts at one exact byte.
#[derive(Clone, Copy)]
pub struct ByteStop<const BYTE: u8>;
impl<const BYTE: u8> StopSet for ByteStop<BYTE> {
    const EQ: [u8; 8] = [BYTE, 0, 0, 0, 0, 0, 0, 0];
    const EQ_LEN: u8 = 1;
    const LT: Option<u8> = None;
    const GE: Option<u8> = None;
    const ALL: bool = false;
}

/// RFC 7464 record separator, so a JSON-seq record scan runs between `0x1E`.
pub type JsonSeqFrame = ByteStop<0x1E>;

/// Fact walk: every comment starts with `/`, so a range with no `/` has no facts.
pub type CommentStart = ByteStop<b'/'>;

/// Block-comment scan: the next `*`, so `*/` is a two-byte check after the run.
pub type Star = ByteStop<b'*'>;

/// Locate-mode string: stop only at `"` or `\`. Used by the shard cut scan.
#[derive(Clone, Copy)]
pub struct StringEnd;
impl StopSet for StringEnd {
    const EQ: [u8; 8] = [b'"', b'\\', 0, 0, 0, 0, 0, 0];
    const EQ_LEN: u8 = 2;
    const LT: Option<u8> = None;
    const GE: Option<u8> = None;
    const ALL: bool = false;
}

/// Locate-mode closing `"` only: an escaped quote is resolved by backslash
/// parity, so the run never stops at a content escape.
pub type DoubleQuote = ByteStop<b'"'>;

/// Locate-mode closing `'` for a JSON5 single-quoted string.
pub type SingleQuote = ByteStop<b'\''>;

/// JSON5 locate-mode string: stop at `'` or `\` for the no-escape run.
#[derive(Clone, Copy)]
pub struct SingleStringEnd;
impl StopSet for SingleStringEnd {
    const EQ: [u8; 8] = [b'\'', b'\\', 0, 0, 0, 0, 0, 0];
    const EQ_LEN: u8 = 2;
    const LT: Option<u8> = None;
    const GE: Option<u8> = None;
    const ALL: bool = false;
}

impl Sealed for StringContent {}
impl Sealed for PlainString {}
impl Sealed for Ws {}
impl Sealed for NdjsonFrame {}
impl<const BYTE: u8> Sealed for ByteStop<BYTE> {}
impl Sealed for StringEnd {}
impl Sealed for SingleStringEnd {}

/// End of a [`PlainString`] run starting at `start`.
#[must_use]
pub fn plain_string_run_end(bytes: &[u8], start: usize) -> usize {
    start + prefix_len::<PlainString>(&bytes[start..])
}
