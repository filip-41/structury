//! RFC 8259 / JSONC / JSON5 lexers: trivia, strings, numbers, unread skip.
//!
//! [`skip`] locates a whole value without building it; [`Dialect`] selects comments, trailing commas, and JSON5 scalars.

use structury::ByteRange;

use crate::dialect::Dialect;
use crate::error::small::{self as error, SmallErr};

pub(crate) mod number;
pub(crate) mod skip;
pub(crate) mod stop_sets;
pub(crate) mod string;
pub(crate) mod ws;

pub use skip::MAX_NESTING;
pub(crate) use skip::{Check, skip_present, skip_present_at, skip_value, skip_value_at, skip_value_counting, ws_rfc};
pub(crate) use string::{parse_string_into, skip_string, skip_string_locate};
pub(crate) use ws::{skip_comment, skip_trivia, skip_trivia_spans};

/// Whether `byte` can start a JSON5 identifier key.
#[must_use]
pub(crate) fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

/// Value of one ASCII hex digit; `None` for any other byte.
#[must_use]
pub(crate) fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// End of the JSON5 identifier starting at `start`.
#[must_use]
pub(crate) fn ident_end(bytes: &[u8], start: usize) -> usize {
    start
        + bytes[start..]
            .iter()
            .take_while(|&&b| is_ident_start(b) || b.is_ascii_digit())
            .count()
}

/// End of a quoted key token: locate it, or value-check it under [`Check::Values`].
#[inline]
fn skip_quoted_key(bytes: &[u8], pos: usize, check: Check, dialect: Dialect) -> Result<usize, SmallErr> {
    match check {
        Check::Locate => string::skip_string_locate(bytes, pos, dialect),
        Check::Values => string::skip_string(bytes, pos, dialect),
    }
}

/// End of an object key token. JSON5 allows `'…'` and bare identifiers.
#[inline]
pub(crate) fn skip_key(bytes: &[u8], pos: usize, check: Check, dialect: Dialect) -> Result<usize, SmallErr> {
    match bytes.get(pos) {
        Some(b'"') => skip_quoted_key(bytes, pos, check, dialect),
        Some(b'\'') if dialect.json5() => skip_quoted_key(bytes, pos, check, dialect),
        Some(&byte) if dialect.json5() && is_ident_start(byte) => Ok(ident_end(bytes, pos)),
        _ => Err(error::expected_key(pos)),
    }
}

/// Inner source span of a key when its bytes equal its decoded text.
///
/// `None` means the key must be decoded to match a demand.
#[must_use]
#[inline]
pub(crate) fn key_plain_inner(bytes: &[u8], start: usize, end: usize, dialect: Dialect) -> Option<ByteRange> {
    match bytes.get(start) {
        Some(b'"') => {
            let (_, quoted_end) = string::plain_double_quoted(bytes, start)?;
            (quoted_end == end).then(|| ByteRange::try_new(start + 1, end - 1).expect("ordered"))
        }
        Some(b'\'') if dialect.json5() && end >= start + 2 => {
            let inner = bytes.get(start + 1..end - 1)?;
            let plain = inner.iter().all(|&b| (0x20..0x7f).contains(&b) && b != b'\\');
            plain.then(|| ByteRange::try_new(start + 1, end - 1).expect("ordered"))
        }
        _ if dialect.json5() => ByteRange::try_new(start, end),
        _ => None,
    }
}
