//! Trivia: RFC 8259 whitespace plus optional JSONC / JSON5 comments.

use structury::byte_scan::prefix_len;

use super::stop_sets::{NdjsonFrame, Star, Ws};
use crate::dialect::Dialect;
use crate::error::small::{self as error, SmallErr};

#[allow(clippy::inline_always)] // hot scanner path: forced inline is intentional
#[inline(always)]
const fn is_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// Compact punctuation peeks and returns `pos` without a ws scan.
#[allow(clippy::inline_always)] // hot scanner path: forced inline is intentional
#[inline(always)]
#[must_use]
pub(crate) fn skip_ws(bytes: &[u8], pos: usize) -> usize {
    match bytes.get(pos) {
        Some(&byte) if !is_ws(byte) => pos,
        Some(_) => pos + prefix_len::<Ws>(&bytes[pos..]),
        None => pos,
    }
}

/// Skip whitespace and, when `dialect` has comments, comment trivia.
/// BOM is not trivia; callers strip an initial BOM.
#[allow(clippy::inline_always)] // hot scanner path: forced inline is intentional
#[inline(always)]
pub(crate) fn skip_trivia(bytes: &[u8], pos: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    skip_trivia_spans(bytes, pos, dialect, |_, _| {})
}

/// [`skip_trivia`] that also hands each comment span to `on_comment` as it is
/// classified, so a recording walk classifies trivia once. Skip order and error
/// cases match [`skip_trivia`].
#[allow(clippy::inline_always)] // hot scanner path: forced inline is intentional
#[inline(always)]
pub(crate) fn skip_trivia_spans<F: FnMut(usize, usize)>(
    bytes: &[u8],
    pos: usize,
    dialect: Dialect,
    mut on_comment: F,
) -> Result<usize, SmallErr> {
    let mut cursor = skip_ws(bytes, pos);
    if !dialect.has_comments() {
        return Ok(cursor);
    }
    loop {
        match bytes.get(cursor) {
            Some(b'/') => {
                let start = cursor;
                cursor = skip_comment(bytes, cursor)?;
                on_comment(start, cursor);
                cursor = skip_ws(bytes, cursor);
            }
            _ => return Ok(cursor),
        }
    }
}

/// End of the comment starting at `start` (a `/`).
pub(crate) fn skip_comment(bytes: &[u8], start: usize) -> Result<usize, SmallErr> {
    match bytes.get(start + 1) {
        Some(b'/') => {
            let cursor = start + 2;
            Ok(cursor + prefix_len::<NdjsonFrame>(&bytes[cursor..]))
        }
        Some(b'*') => {
            let mut cursor = start + 2;
            while cursor + 1 < bytes.len() {
                cursor += prefix_len::<Star>(&bytes[cursor..]);
                if cursor + 1 >= bytes.len() {
                    break;
                }
                if bytes[cursor + 1] == b'/' {
                    return Ok(cursor + 2);
                }
                cursor += 1;
            }
            Err(error::unterminated_comment(start))
        }
        _ => Err(error::invalid_comment(start)),
    }
}

/// Whether `pos` is a token boundary (EOF, whitespace, structural punct, or a
/// comment start when `dialect` has comments).
#[must_use]
pub(crate) fn is_token_end(bytes: &[u8], pos: usize, dialect: Dialect) -> bool {
    match bytes.get(pos) {
        None => true,
        Some(b) => {
            matches!(
                b,
                b' ' | b'\t' | b'\n' | b'\r' | b',' | b']' | b'}' | b':' | b'{' | b'['
            ) || (dialect.has_comments() && *b == b'/')
        }
    }
}
