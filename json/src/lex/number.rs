//! RFC 8259 number grammar, plus the JSON5 number subset.
//!
//! Locate mode consumes a number-shaped span without value-checking it; JSON5 adds signs, hex, and `Infinity`/`NaN`.

#![expect(
    clippy::inline_always,
    reason = "the per-token scan must fold into the walk; a call per token is the residual this removes"
)]

use alloc::string::String;
use core::fmt::Write as _;

use structury::NonFinite;
use structury::byte_scan::lane8;

use super::hex_digit;
use super::ws::is_token_end;
use crate::dialect::Dialect;
use crate::error::small::{self as error, SmallErr};

/// Mask with `0x80` set in every byte of `word` that is **not** an ASCII digit.
///
/// `hasless`/`hasmore` over `0x30..=0x39`. A borrow out of a byte can only
/// originate from a byte outside the digit range, so the first set high bit is
/// the first non-digit even though the subtraction crosses byte boundaries.
#[must_use]
#[inline(always)]
const fn non_digit_mask(word: u64) -> u64 {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let below = word.wrapping_sub(ONES * 0x30) & !word & HIGH;
    let above = (word.wrapping_add(ONES * (0x7f - 0x39)) | word) & HIGH;
    below | above
}

/// Length of the ASCII-digit prefix. A short scalar head keeps the common
/// few-digit number off the lane path; a long run is not re-walked by a scalar tail.
#[must_use]
#[inline(always)]
pub(crate) fn digit_run_len(bytes: &[u8]) -> usize {
    let mut n = 0;
    while n < 4 && n < bytes.len() && bytes[n].is_ascii_digit() {
        n += 1;
    }
    if n < 4 {
        return n;
    }
    while let Some(word) = lane8(bytes, n) {
        let mask = non_digit_mask(word);
        if mask != 0 {
            return n + (mask.trailing_zeros() as usize) / 8;
        }
        n += 8;
    }
    while n < bytes.len() && bytes[n].is_ascii_digit() {
        n += 1;
    }
    n
}

/// Length of the ASCII hex-digit prefix.
#[must_use]
pub(crate) fn hex_run_len(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|b| b.is_ascii_hexdigit()).count()
}

/// RFC 8259 integer fast path: optional `-`, digits, no `.`/`e`/`E`, no
/// leading zeros. `None` means fall back to the full automaton.
pub(crate) fn skip_rfc_integer(bytes: &[u8], start: usize, dialect: Dialect) -> Option<usize> {
    let mut cursor = start;
    if bytes.get(cursor) == Some(&b'-') {
        cursor += 1;
    }
    let first = *bytes.get(cursor)?;
    if !first.is_ascii_digit() {
        return None;
    }
    cursor += 1;
    if first == b'0' {
        if bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            return None;
        }
    } else {
        cursor += digit_run_len(&bytes[cursor..]);
    }
    match bytes.get(cursor) {
        Some(b'.' | b'e' | b'E') => None,
        Some(_) if !is_token_end(bytes, cursor, dialect) => None,
        _ => Some(cursor),
    }
}

/// Number-shaped span for Structural/Lazy unread: digits, sign, point,
/// exponent marks, hex, or a non-finite word. Does not refuse `01`, `1.`,
/// `1e`, or `Infinity`.
#[inline(always)]
pub(crate) fn skip_number_locate(bytes: &[u8], start: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    let mut cursor = start;
    if matches!(bytes.get(cursor), Some(b'+' | b'-')) {
        cursor += 1;
    }
    let Some(&first) = bytes.get(cursor) else {
        return Err(error::incomplete_number(cursor));
    };
    if dialect.json5() {
        match first {
            b'I' | b'N' => {
                cursor += bytes[cursor..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
                return Ok(cursor);
            }
            b'0' if matches!(bytes.get(cursor + 1), Some(b'x' | b'X')) => {
                cursor += 2 + hex_run_len(&bytes[cursor + 2..]);
                return Ok(cursor);
            }
            _ => {}
        }
    }
    let digits = digit_run_len(&bytes[cursor..]);
    cursor += digits;
    let mut saw_digit = digits > 0;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'0'..=b'9' => {
                saw_digit = true;
                cursor += 1 + digit_run_len(&bytes[cursor + 1..]);
            }
            b'.' | b'e' | b'E' | b'+' | b'-' => cursor += 1,
            _ => break,
        }
    }
    if !saw_digit {
        return Err(error::expected_value(start));
    }
    Ok(cursor)
}

/// Full number grammar for `dialect`. Returns the cursor past the literal.
///
/// # Errors
///
/// [`SmallErrClass::Number`] on a malformed or non-finite number.
pub(crate) fn lex_number(bytes: &[u8], start: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    if let Some(end) = skip_rfc_integer(bytes, start, dialect) {
        return Ok(end);
    }
    if dialect.json5() {
        return lex_number_json5(bytes, start, dialect);
    }
    lex_number_rfc(bytes, start, dialect)
}

fn lex_number_rfc(bytes: &[u8], start: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    let mut cursor = start;
    if bytes.get(cursor) == Some(&b'-') {
        cursor += 1;
    }
    let Some(&first) = bytes.get(cursor) else {
        return Err(error::incomplete_number(cursor));
    };
    if !first.is_ascii_digit() {
        return Err(error::invalid_number(start));
    }
    if first == b'0' {
        cursor += 1;
        if bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            return Err(error::leading_zeros(cursor));
        }
    } else {
        cursor += 1 + digit_run_len(&bytes[cursor + 1..]);
    }
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let n = digit_run_len(&bytes[cursor..]);
        if n == 0 {
            return Err(error::incomplete_number(cursor));
        }
        cursor += n;
    }
    cursor = exponent(bytes, cursor)?;
    if !is_token_end(bytes, cursor, dialect) {
        return Err(error::invalid_number(cursor));
    }
    Ok(cursor)
}

fn lex_number_json5(bytes: &[u8], start: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    let mut cursor = start;
    if matches!(bytes.get(cursor), Some(b'+' | b'-')) {
        cursor += 1;
    }
    let Some(&first) = bytes.get(cursor) else {
        return Err(error::incomplete_number(cursor));
    };
    match first {
        b'I' => return non_finite(bytes, start, cursor, b"Infinity", dialect),
        b'N' => return non_finite(bytes, start, cursor, b"NaN", dialect),
        b'0' if matches!(bytes.get(cursor + 1), Some(b'x' | b'X')) => {
            cursor += 2;
            let n = hex_run_len(&bytes[cursor..]);
            if n == 0 {
                return Err(error::incomplete_number(cursor));
            }
            cursor += n;
            if !is_token_end(bytes, cursor, dialect) {
                return Err(error::invalid_number(cursor));
            }
            return Ok(cursor);
        }
        _ => {}
    }
    let int_digits = digit_run_len(&bytes[cursor..]);
    if int_digits > 0 {
        if first == b'0' && int_digits > 1 {
            return Err(error::leading_zeros(cursor + 1));
        }
        cursor += int_digits;
    }
    let mut saw_digit = int_digits > 0;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let frac = digit_run_len(&bytes[cursor..]);
        saw_digit |= frac > 0;
        cursor += frac;
    }
    if !saw_digit {
        return Err(error::invalid_number(start));
    }
    cursor = exponent(bytes, cursor)?;
    if !is_token_end(bytes, cursor, dialect) {
        return Err(error::invalid_number(cursor));
    }
    Ok(cursor)
}

fn exponent(bytes: &[u8], mut cursor: usize) -> Result<usize, SmallErr> {
    if matches!(bytes.get(cursor), Some(b'e' | b'E')) {
        cursor += 1;
        if matches!(bytes.get(cursor), Some(b'+' | b'-')) {
            cursor += 1;
        }
        let n = digit_run_len(&bytes[cursor..]);
        if n == 0 {
            return Err(error::incomplete_number(cursor));
        }
        cursor += n;
    }
    Ok(cursor)
}

fn non_finite(bytes: &[u8], start: usize, at: usize, spelling: &[u8], dialect: Dialect) -> Result<usize, SmallErr> {
    let end = at + spelling.len();
    if bytes.get(at..end) != Some(spelling) {
        return Err(error::invalid_number(start));
    }
    if !is_token_end(bytes, end, dialect) {
        return Err(error::invalid_number(end));
    }
    Ok(end)
}

/// Classify a JSON5 non-finite spelling (already `+`-stripped), signed or not.
#[must_use]
pub(crate) fn non_finite_value(spelling: &str) -> Option<NonFinite> {
    match spelling {
        "Infinity" => Some(NonFinite::Infinity),
        "-Infinity" => Some(NonFinite::NegativeInfinity),
        "NaN" | "-NaN" => Some(NonFinite::NaN),
        _ => None,
    }
}

/// Normalize a JSON5 number spelling into a reused buffer: `+` is dropped and hex
/// is widened to decimal digits. Borrows `buf` when widening runs, else the input.
pub(crate) fn normalize_json5_into<'a>(spelling: &'a str, buf: &'a mut String) -> &'a str {
    let Some(hex) = hex_body(spelling) else {
        return unsigned_prefix(spelling);
    };
    buf.clear();
    if spelling.starts_with('-') {
        buf.push('-');
    }
    push_hex_decimal(hex, buf);
    buf.as_str()
}

/// Hex digits after an optional sign and `0x`/`0X` prefix; `None` when the
/// spelling is not hexadecimal.
fn hex_body(spelling: &str) -> Option<&str> {
    let unsigned = spelling.strip_prefix('-').unwrap_or(spelling);
    let unsigned = unsigned_prefix(unsigned);
    let hex = unsigned.strip_prefix("0x").or_else(|| unsigned.strip_prefix("0X"))?;
    // Guard the widening boundary here rather than trusting callers: a body with a
    // non-hex byte falls through to the ordinary spelling. An empty body stays `Some`
    // so a locate-mode `0x` span remains representable.
    hex.bytes().all(|byte| byte.is_ascii_hexdigit()).then_some(hex)
}

/// Nibble of an ASCII hex byte. `hex_body` admits only ASCII hex, so this total form
/// keeps the widening path panic-free.
fn hex_nibble(byte: u8) -> u32 {
    u32::from(hex_digit(byte).unwrap_or(0))
}

/// Spelling after an optional leading `+`.
fn unsigned_prefix(spelling: &str) -> &str {
    spelling.strip_prefix('+').unwrap_or(spelling)
}

/// Append the decimal digits of a hex string, any width. Empty input is `"0"` so
/// a locate-mode `0x` span stays representable. Up to 128 bits converts through
/// `u128`; wider spellings fall back to a byte-at-a-time big decimal.
fn push_hex_decimal(hex: &str, out: &mut String) {
    if hex.is_empty() {
        out.push('0');
        return;
    }
    if hex.len() <= 32 {
        let mut value: u128 = 0;
        for ch in hex.bytes() {
            value = value * 16 + u128::from(hex_nibble(ch));
        }
        write!(out, "{value}").expect("write to a String cannot fail");
        return;
    }
    let mut digits: alloc::vec::Vec<u8> = alloc::vec![0];
    for ch in hex.bytes() {
        let mut carry = hex_nibble(ch);
        for byte in &mut digits {
            let value = u32::from(*byte) * 16 + carry;
            *byte = (value % 10) as u8;
            carry = value / 10;
        }
        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    for &digit in digits.iter().rev() {
        out.push(char::from(b'0' + digit));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_digit_mask_names_the_first_non_digit_at_every_position() {
        for value in 0u16..=0xff {
            let byte = u8::try_from(value).expect("byte range");
            for pos in 0..8 {
                let shift = 8 * u32::try_from(pos).expect("lane position");
                let word = (0x3131_3131_3131_3131 & !(0xffu64 << shift)) | (u64::from(byte) << shift);
                let mask = non_digit_mask(word);
                let got = if mask == 0 {
                    8
                } else {
                    (mask.trailing_zeros() / 8) as usize
                };
                let expected = if byte.is_ascii_digit() { 8 } else { pos };
                assert_eq!(expected, got, "byte {byte:#04x} at lane position {pos}");
            }
        }
    }

    #[test]
    fn digit_run_len_agrees_with_the_scalar_predicate() {
        let corpora: &[&[u8]] = &[
            b"",
            b"0",
            b"1234567",
            b"12345678",
            b"123456789",
            b"613616999999977",
            b"12345678901234567",
            b"1234567890123456789012345678901234567890",
            b"12.",
            b"1234567e",
            b"12345678e",
            b"999999999,43.4",
            b"00000000000000000001",
        ];
        for bytes in corpora {
            for start in 0..=bytes.len() {
                let slice = &bytes[start..];
                let want = slice.iter().take_while(|b| b.is_ascii_digit()).count();
                assert_eq!(digit_run_len(slice), want, "at {start} of {bytes:?}");
            }
        }
        // A long digit run at every offset in a fixed-size buffer.
        for offset in 0..16 {
            let mut bytes = [b'9'; 64];
            bytes[offset..offset + 40].fill(b'7');
            bytes[offset + 40] = b'x';
            assert_eq!(digit_run_len(&bytes[offset..]), 40, "offset {offset}");
        }
    }
}
