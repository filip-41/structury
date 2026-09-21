//! RFC 8259 strings, plus JSON5 single-quoted strings and JSON5 escapes.
//!
//! Locate resolves escaped quotes via the preceding backslash run; Strict validates UTF-8, escapes, surrogates, and controls.

#![expect(
    clippy::inline_always,
    reason = "the per-token scan must fold into the walk; a call per token is the residual this removes"
)]

use alloc::string::String;

use structury::byte_scan::{StopSet, prefix_len};

use super::hex_digit;
use super::stop_sets::{DoubleQuote, PlainString, SingleQuote, SingleStringEnd, StringContent, StringEnd};
use crate::dialect::Dialect;
use crate::error::small::{self as error, SmallErr};

/// Locate the closing quote without value-checking content.
#[inline]
pub(crate) fn skip_string_locate(bytes: &[u8], start: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    match bytes.get(start) {
        Some(&b'"') => locate_double_quoted(bytes, start),
        Some(&b'\'') if dialect.json5() => locate_single_quoted(bytes, start),
        _ => Err(error::expected_value(start)),
    }
}

/// Locate a closing `"`, resolving escapes by backslash parity.
#[inline]
pub(crate) fn locate_double_quoted(bytes: &[u8], start: usize) -> Result<usize, SmallErr> {
    locate_quoted::<StringEnd, DoubleQuote>(bytes, start, b'"')
}

/// Locate a closing `'` for a JSON5 single-quoted string.
#[inline]
pub(crate) fn locate_single_quoted(bytes: &[u8], start: usize) -> Result<usize, SmallErr> {
    locate_quoted::<SingleStringEnd, SingleQuote>(bytes, start, b'\'')
}

/// Whether the quote at `quote` is escaped: an odd run of `\` precedes it.
#[must_use]
pub(crate) fn quote_is_escaped(bytes: &[u8], quote: usize) -> bool {
    let mut run = 0usize;
    let mut i = quote;
    while i > 0 && bytes[i - 1] == b'\\' {
        run += 1;
        i -= 1;
    }
    run % 2 == 1
}

/// Locate the closing quote. A string with no escapes takes one lane run.
/// The first `\` switches to `Q`. Backslash parity resolves the rest.
fn locate_quoted<C: StopSet, Q: StopSet>(bytes: &[u8], start: usize, quote: u8) -> Result<usize, SmallErr> {
    let mut cursor = start + 1;
    cursor += prefix_len::<C>(&bytes[cursor..]);
    match bytes.get(cursor) {
        Some(&byte) if byte == quote => Ok(cursor + 1),
        Some(&b'\\') => locate_escaped::<Q>(bytes, cursor),
        _ => Err(error::unterminated_string(bytes.len())),
    }
}

/// Locate the closing quote from a `\` at `cursor`, skipping the escape and
/// then scanning with backslash parity.
fn locate_escaped<Q: StopSet>(bytes: &[u8], cursor: usize) -> Result<usize, SmallErr> {
    let mut cursor = cursor + 2;
    if cursor > bytes.len() {
        return Err(error::unterminated_string(bytes.len()));
    }
    loop {
        cursor += prefix_len::<Q>(&bytes[cursor..]);
        if bytes.get(cursor).is_none() {
            return Err(error::unterminated_string(bytes.len()));
        }
        if quote_is_escaped(bytes, cursor) {
            cursor += 1;
        } else {
            return Ok(cursor + 1);
        }
    }
}

/// Validate a string without allocating. Returns the cursor past the closer.
#[inline]
pub(crate) fn skip_string(bytes: &[u8], start: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    if dialect.json5() {
        skip_string_json5(bytes, start)
    } else {
        skip_string_rfc(bytes, start)
    }
}

/// Validate a double-quoted string under RFC 8259 / JSONC grammar.
#[inline]
pub(crate) fn skip_string_rfc(bytes: &[u8], start: usize) -> Result<usize, SmallErr> {
    walk_quoted::<false, b'"', false>(bytes, start, None)
}

/// Validate a JSON5 string: `"…"` or `'…'`, with JSON5 escapes.
#[inline]
pub(crate) fn skip_string_json5(bytes: &[u8], start: usize) -> Result<usize, SmallErr> {
    match bytes.get(start) {
        Some(&b'"') => walk_quoted::<false, b'"', true>(bytes, start, None),
        Some(&b'\'') => walk_quoted::<false, b'\'', true>(bytes, start, None),
        _ => Err(error::expected_value(start)),
    }
}

/// Decode a string into `text`. Clears `text` first.
pub(crate) fn parse_string_into(
    bytes: &[u8],
    start: usize,
    text: &mut String,
    dialect: Dialect,
) -> Result<usize, SmallErr> {
    if dialect.json5() {
        parse_string_into_json5(bytes, start, text)
    } else {
        parse_string_into_rfc(bytes, start, text)
    }
}

/// Decode a double-quoted string under RFC 8259 / JSONC grammar into `text`.
pub(crate) fn parse_string_into_rfc(bytes: &[u8], start: usize, text: &mut String) -> Result<usize, SmallErr> {
    text.clear();
    text.reserve(bytes.len().saturating_sub(start).saturating_sub(1));
    walk_quoted::<true, b'"', false>(bytes, start, Some(text))
}

/// Decode a JSON5 string (`"…"` or `'…'`) into `text`.
pub(crate) fn parse_string_into_json5(bytes: &[u8], start: usize, text: &mut String) -> Result<usize, SmallErr> {
    text.clear();
    text.reserve(bytes.len().saturating_sub(start).saturating_sub(1));
    match bytes.get(start) {
        Some(&b'"') => walk_quoted::<true, b'"', true>(bytes, start, Some(text)),
        Some(&b'\'') => walk_quoted::<true, b'\'', true>(bytes, start, Some(text)),
        _ => Err(error::expected_value(start)),
    }
}

fn emit<const DECODE: bool>(text: &mut Option<&mut String>, ch: char) {
    if DECODE && let Some(out) = text.as_mut() {
        out.push(ch);
    }
}

fn emit_str<const DECODE: bool>(text: &mut Option<&mut String>, s: &str) {
    if DECODE && let Some(out) = text.as_mut() {
        out.push_str(s);
    }
}

/// Consume a plain `"…"` run at `cursor`: emit it and return the cursor past the
/// closing quote (`closed` true) or just past the run (`closed` false). `None`
/// when no plain run starts at `cursor`.
fn take_plain_run<const DECODE: bool>(
    bytes: &[u8],
    cursor: usize,
    text: &mut Option<&mut String>,
) -> Result<Option<(usize, bool)>, SmallErr> {
    let rest = &bytes[cursor..];
    let run = prefix_len::<PlainString>(rest);
    if rest.get(run) == Some(&b'"') {
        if DECODE {
            emit_str::<DECODE>(text, utf8_at(rest, run, cursor)?);
        }
        return Ok(Some((cursor + run + 1, true)));
    }
    if run == 0 {
        return Ok(None);
    }
    if DECODE {
        emit_str::<DECODE>(text, utf8_at(rest, run, cursor)?);
    }
    Ok(Some((cursor + run, false)))
}

fn utf8_at(rest: &[u8], n: usize, cursor: usize) -> Result<&str, SmallErr> {
    core::str::from_utf8(&rest[..n]).map_err(|e| error::utf8(cursor + e.valid_up_to()))
}

#[allow(clippy::too_many_lines)] // single-pass decoder; splitting would obscure the byte cursor
fn walk_quoted<const DECODE: bool, const QUOTE: u8, const JSON5: bool>(
    bytes: &[u8],
    start: usize,
    mut text: Option<&mut String>,
) -> Result<usize, SmallErr> {
    let mut cursor = start + 1;
    if QUOTE == b'"'
        && let Some((next, closed)) = take_plain_run::<DECODE>(bytes, cursor, &mut text)?
    {
        if closed {
            return Ok(next);
        }
        cursor = next;
    }
    let mut high_surrogate: Option<u16> = None;
    while cursor < bytes.len() {
        if let Some(high) = high_surrogate.take() {
            let (ch, next) = take_low_surrogate(bytes, cursor, high)?;
            emit::<DECODE>(&mut text, ch);
            cursor = next;
            continue;
        }
        let byte = bytes[cursor];
        if byte == QUOTE {
            return Ok(cursor + 1);
        }
        match byte {
            b'\\' => {
                cursor += 1;
                let Some(escape) = bytes.get(cursor).copied() else {
                    return Err(error::unterminated_string(bytes.len()));
                };
                cursor += 1;
                match escape {
                    b'"' | b'\\' | b'/' => emit::<DECODE>(&mut text, char::from(escape)),
                    b'\'' if JSON5 => emit::<DECODE>(&mut text, '\''),
                    b'b' => emit::<DECODE>(&mut text, '\u{0008}'),
                    b'f' => emit::<DECODE>(&mut text, '\u{000c}'),
                    b'n' => emit::<DECODE>(&mut text, '\n'),
                    b'r' => emit::<DECODE>(&mut text, '\r'),
                    b't' => emit::<DECODE>(&mut text, '\t'),
                    b'u' => {
                        let (value, next) = take_four_hex(bytes, cursor)?;
                        cursor = next;
                        if (0xd800..=0xdbff).contains(&value) {
                            high_surrogate = Some(value);
                        } else if (0xdc00..=0xdfff).contains(&value) {
                            return Err(error::invalid_surrogate_pair(cursor - 4));
                        } else {
                            let Some(ch) = char::from_u32(u32::from(value)) else {
                                return Err(error::invalid_unicode_escape(cursor - 4));
                            };
                            emit::<DECODE>(&mut text, ch);
                        }
                    }
                    b'x' | b'v' | b'0' | b'\n' | b'\r' | 0xe2 if JSON5 => match json5_escape(bytes, cursor - 1) {
                        Some((end, ch)) => {
                            if let Some(ch) = ch {
                                emit::<DECODE>(&mut text, ch);
                            }
                            cursor = end;
                        }
                        None => return Err(error::invalid_escape(cursor - 1)),
                    },
                    _ => return Err(error::invalid_escape(cursor - 1)),
                }
            }
            0x00..=0x1f => return Err(error::control_in_string(cursor)),
            0x7f => {
                emit::<DECODE>(&mut text, '\u{7f}');
                cursor += 1;
            }
            _ => {
                if QUOTE == b'"'
                    && let Some((next, closed)) = take_plain_run::<DECODE>(bytes, cursor, &mut text)?
                {
                    if closed {
                        return Ok(next);
                    }
                    cursor = next;
                    continue;
                }
                let rest = &bytes[cursor..];
                // Earlier arms left only content bytes here. The run is never empty.
                let n = if QUOTE == b'"' {
                    prefix_len::<StringContent>(rest)
                } else {
                    rest.iter()
                        .take_while(|&&b| b >= 0x20 && b != 0x7f && b != b'\\' && b != b'\'')
                        .count()
                };
                emit_str::<DECODE>(&mut text, utf8_at(rest, n, cursor)?);
                cursor += n;
            }
        }
    }
    if high_surrogate.is_some() {
        return Err(error::missing_low_surrogate(bytes.len()));
    }
    Err(error::unterminated_string(bytes.len()))
}

fn take_low_surrogate(bytes: &[u8], mut cursor: usize, high: u16) -> Result<(char, usize), SmallErr> {
    if bytes.get(cursor) != Some(&b'\\') {
        return Err(error::missing_low_surrogate(cursor));
    }
    cursor += 1;
    if bytes.get(cursor) != Some(&b'u') {
        return Err(error::missing_low_surrogate(cursor.min(bytes.len())));
    }
    cursor += 1;
    let (low, next) = take_four_hex(bytes, cursor)?;
    if !(0xdc00..=0xdfff).contains(&low) {
        return Err(error::invalid_surrogate_pair(cursor));
    }
    let scalar = 0x1_0000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
    let Some(ch) = char::from_u32(scalar) else {
        return Err(error::invalid_surrogate_pair(cursor));
    };
    Ok((ch, next))
}

fn take_four_hex(bytes: &[u8], start: usize) -> Result<(u16, usize), SmallErr> {
    let mut value = 0u16;
    let mut cursor = start;
    for _ in 0..4 {
        let Some(byte) = bytes.get(cursor).copied() else {
            return Err(error::invalid_unicode_escape(bytes.len()));
        };
        let Some(digit) = hex_digit(byte) else {
            return Err(error::invalid_unicode_escape(cursor));
        };
        value = (value << 4) | u16::from(digit);
        cursor += 1;
    }
    Ok((value, cursor))
}

/// A JSON5-only escape. `at` indexes the escape byte after the backslash.
/// `Some((end, None))` is a line continuation; `None` means this is not a
/// well-formed JSON5-only escape.
fn json5_escape(bytes: &[u8], at: usize) -> Option<(usize, Option<char>)> {
    match bytes.get(at)? {
        b'x' => {
            let (value, end) = take_two_hex(bytes, at + 1)?;
            Some((end, Some(char::from(value))))
        }
        b'v' => Some((at + 1, Some('\u{000b}'))),
        b'0' if !bytes.get(at + 1).is_some_and(u8::is_ascii_digit) => Some((at + 1, Some('\0'))),
        b'\n' => Some((at + 1, None)),
        b'\r' => {
            let end = if bytes.get(at + 1) == Some(&b'\n') {
                at + 2
            } else {
                at + 1
            };
            Some((end, None))
        }
        0xe2 if bytes.get(at + 1) == Some(&0x80) && matches!(bytes.get(at + 2), Some(0xa8 | 0xa9)) => {
            Some((at + 3, None))
        }
        _ => None,
    }
}

fn take_two_hex(bytes: &[u8], start: usize) -> Option<(u8, usize)> {
    let high = hex_digit(*bytes.get(start)?)?;
    let low = hex_digit(*bytes.get(start + 1)?)?;
    Some(((high << 4) | low, start + 2))
}

#[must_use]
pub(crate) fn plain_double_quoted(bytes: &[u8], start: usize) -> Option<(&[u8], usize)> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let inner = start + 1;
    let run_end = super::stop_sets::plain_string_run_end(bytes, inner);
    if bytes.get(run_end) == Some(&b'"') {
        Some((&bytes[inner..run_end], run_end + 1))
    } else {
        None
    }
}

/// Locate a plain double-quoted span for a member key.
///
/// Keys are short, so the scalar run beats the wide kernel that
/// [`plain_double_quoted`] uses for string values.
#[must_use]
#[inline(always)]
pub(crate) fn plain_key(bytes: &[u8], start: usize) -> Option<(&[u8], usize)> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let inner = start + 1;
    let mut cursor = inner;
    while let Some(&byte) = bytes.get(cursor) {
        if byte == b'"' {
            return Some((&bytes[inner..cursor], cursor + 1));
        }
        if !(0x20..0x7f).contains(&byte) || byte == b'\\' {
            return None;
        }
        cursor += 1;
    }
    None
}
