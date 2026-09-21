//! The one skip engine: locate a whole value (or a run) without building it.
//!
//! Every atom routes to the scalar lexer. Nesting past [`MAX_NESTING`] is `nesting`.

use super::ws::{is_token_end, skip_trivia};
use super::{number, skip_key, string};
use crate::dialect::Dialect;
use crate::error::small::{self as error, SmallErr};

/// Container nesting bound (root is depth 0). Deeper input is refused as
/// `nesting`. A request overrides it via `ScanRequest::max_nesting`.
pub const MAX_NESTING: u32 = 256;

/// Value-check dial on the skip walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Check {
    /// Structure only.
    Locate,
    /// RFC value checks.
    Values,
}

/// Walk context shared by every monomorphization: the nesting bound and dialect.
#[derive(Clone, Copy)]
struct Ctx {
    max: u32,
    dialect: Dialect,
}

const fn check_of<const VALUES: bool>() -> Check {
    if VALUES { Check::Values } else { Check::Locate }
}

/// Byte classes the RFC structural walk dispatches on.
mod class {
    pub const WS: u8 = 0;
    pub const LBRACE: u8 = 1;
    pub const LBRACK: u8 = 2;
    pub const QUOTE: u8 = 3;
    /// A byte that can start a number: sign, or a digit.
    pub const NUM: u8 = 4;
    /// `t` / `f` / `n`.
    pub const LIT: u8 = 5;
    pub const OTHER: u8 = 6;
}

use class::{LBRACE, LBRACK, LIT, NUM, QUOTE, WS};

/// 256-entry RFC byte-class table, computed at compile time.
static CLASS: [u8; 256] = build_class();

const fn build_class() -> [u8; 256] {
    let mut table = [class::OTHER; 256];
    table[b' ' as usize] = WS;
    table[b'\t' as usize] = WS;
    table[b'\n' as usize] = WS;
    table[b'\r' as usize] = WS;
    table[b'{' as usize] = LBRACE;
    table[b'[' as usize] = LBRACK;
    table[b'"' as usize] = QUOTE;
    table[b'-' as usize] = NUM;
    let mut digit = b'0';
    while digit <= b'9' {
        table[digit as usize] = NUM;
        digit += 1;
    }
    table[b't' as usize] = LIT;
    table[b'f' as usize] = LIT;
    table[b'n' as usize] = LIT;
    table
}

/// Longest RFC whitespace run from `pos`. Every RFC whitespace byte is `<= 0x20`,
/// so one compare is the whole dense case and the class loop runs only where trivia begins.
#[allow(clippy::inline_always)] // hot tokenizer path: forced inline is intentional
#[inline(always)]
pub(crate) fn ws_rfc(bytes: &[u8], pos: usize) -> usize {
    match bytes.get(pos) {
        Some(&byte) if byte > b' ' => pos,
        Some(_) => {
            let mut cursor = pos;
            while cursor < bytes.len() && CLASS[bytes[cursor] as usize] == WS {
                cursor += 1;
            }
            cursor
        }
        None => pos,
    }
}

/// Skip one JSON value starting at `pos`.
pub(crate) fn skip_value(
    bytes: &[u8],
    pos: usize,
    check: Check,
    max: u32,
    dialect: Dialect,
) -> Result<usize, SmallErr> {
    skip_value_at(bytes, pos, check, 0, max, dialect)
}

/// [`skip_value`] continuing from an already-entered nesting depth.
pub(crate) fn skip_value_at(
    bytes: &[u8],
    pos: usize,
    check: Check,
    depth: u32,
    max: u32,
    dialect: Dialect,
) -> Result<usize, SmallErr> {
    let mut sink = 0u64;
    present_entry::<false>(bytes, pos, check, depth, max, dialect, true, &mut sink)
}

/// Skip one value and count preorder nodes (self + descendants).
pub(crate) fn skip_value_counting(
    bytes: &[u8],
    pos: usize,
    check: Check,
    max: u32,
    dialect: Dialect,
) -> Result<(usize, u64), SmallErr> {
    let mut count = 0u64;
    let end = present_entry::<true>(bytes, pos, check, 0, max, dialect, true, &mut count)?;
    Ok((end, count))
}

/// Skip a value whose first token byte is at `pos`.
pub(crate) fn skip_present(
    bytes: &[u8],
    pos: usize,
    check: Check,
    max: u32,
    dialect: Dialect,
) -> Result<usize, SmallErr> {
    skip_present_at(bytes, pos, check, 0, max, dialect)
}

pub(crate) fn skip_present_at(
    bytes: &[u8],
    pos: usize,
    check: Check,
    depth: u32,
    max: u32,
    dialect: Dialect,
) -> Result<usize, SmallErr> {
    let mut sink = 0u64;
    present_entry::<false>(bytes, pos, check, depth, max, dialect, false, &mut sink)
}

#[allow(clippy::too_many_arguments)] // the engine's inputs are all independent
fn present_entry<const COUNT: bool>(
    bytes: &[u8],
    pos: usize,
    check: Check,
    depth: u32,
    max: u32,
    dialect: Dialect,
    leading: bool,
    count: &mut u64,
) -> Result<usize, SmallErr> {
    let ctx = Ctx { max, dialect };
    let pos = if !leading {
        pos
    } else if dialect == Dialect::Rfc8259 {
        ws_rfc(bytes, pos)
    } else {
        skip_trivia(bytes, pos, dialect)?
    };
    if dialect == Dialect::Rfc8259 {
        match check {
            Check::Locate => present::<true, false, COUNT>(bytes, pos, ctx, count, depth),
            Check::Values => present::<true, true, COUNT>(bytes, pos, ctx, count, depth),
        }
    } else {
        match check {
            Check::Locate => present::<false, false, COUNT>(bytes, pos, ctx, count, depth),
            Check::Values => present::<false, true, COUNT>(bytes, pos, ctx, count, depth),
        }
    }
}

#[allow(clippy::inline_always)] // hot tokenizer path: forced inline is intentional
#[inline(always)]
fn present<const RFC: bool, const VALUES: bool, const COUNT: bool>(
    bytes: &[u8],
    pos: usize,
    ctx: Ctx,
    count: &mut u64,
    depth: u32,
) -> Result<usize, SmallErr> {
    if COUNT {
        *count = count.saturating_add(1);
    }
    let Some(&byte) = bytes.get(pos) else {
        return Err(error::expected_value(pos));
    };
    if RFC {
        match CLASS[byte as usize] {
            QUOTE => skip_string_checked::<RFC, VALUES>(bytes, pos, ctx.dialect),
            NUM => skip_number_checked::<RFC, VALUES>(bytes, pos, ctx.dialect),
            LIT => skip_literal_at(bytes, pos, ctx.dialect),
            LBRACE => skip_object::<RFC, VALUES, COUNT>(bytes, pos, ctx, count, depth, false),
            LBRACK => skip_array::<RFC, VALUES, COUNT>(bytes, pos, ctx, count, depth),
            _ => Err(error::expected_value(pos)),
        }
    } else {
        match byte {
            b'"' => skip_string_checked::<RFC, VALUES>(bytes, pos, ctx.dialect),
            b'\'' if ctx.dialect.json5() => skip_string_checked::<RFC, VALUES>(bytes, pos, ctx.dialect),
            b'n' | b't' | b'f' => skip_literal_at(bytes, pos, ctx.dialect),
            b'-' | b'0'..=b'9' => skip_number_checked::<RFC, VALUES>(bytes, pos, ctx.dialect),
            b'+' | b'.' | b'I' | b'N' if ctx.dialect.json5() => {
                skip_number_checked::<RFC, VALUES>(bytes, pos, ctx.dialect)
            }
            b'[' => skip_array::<RFC, VALUES, COUNT>(bytes, pos, ctx, count, depth),
            b'{' => skip_object::<RFC, VALUES, COUNT>(bytes, pos, ctx, count, depth, false),
            _ => Err(error::expected_value(pos)),
        }
    }
}

#[allow(clippy::inline_always)] // hot scanner path: forced inline is intentional
#[inline(always)]
fn trivia<const RFC: bool>(bytes: &[u8], pos: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    if RFC {
        Ok(ws_rfc(bytes, pos))
    } else {
        skip_trivia(bytes, pos, dialect)
    }
}

#[allow(clippy::inline_always)] // hot scanner path: forced inline is intentional
#[inline(always)]
fn skip_string_checked<const RFC: bool, const VALUES: bool>(
    bytes: &[u8],
    start: usize,
    dialect: Dialect,
) -> Result<usize, SmallErr> {
    if RFC {
        if VALUES {
            string::skip_string_rfc(bytes, start)
        } else {
            string::locate_double_quoted(bytes, start)
        }
    } else if VALUES {
        string::skip_string(bytes, start, dialect)
    } else {
        string::skip_string_locate(bytes, start, dialect)
    }
}

#[allow(clippy::inline_always)] // hot scanner path: forced inline is intentional
#[inline(always)]
fn skip_number_checked<const RFC: bool, const VALUES: bool>(
    bytes: &[u8],
    start: usize,
    dialect: Dialect,
) -> Result<usize, SmallErr> {
    let dialect = if RFC { Dialect::Rfc8259 } else { dialect };
    if VALUES {
        number::lex_number(bytes, start, dialect)
    } else {
        number::skip_number_locate(bytes, start, dialect)
    }
}

#[allow(clippy::inline_always)] // hot scanner path: forced inline is intentional
#[inline(always)]
fn skip_literal_at(bytes: &[u8], start: usize, dialect: Dialect) -> Result<usize, SmallErr> {
    let spelling: &[u8] = match bytes.get(start) {
        Some(b't') => b"true",
        Some(b'f') => b"false",
        Some(b'n') => b"null",
        _ => return Err(error::expected_value(start)),
    };
    skip_literal_spelling(bytes, start, spelling, dialect)
}

pub(crate) fn skip_literal_spelling(
    bytes: &[u8],
    start: usize,
    spelling: &[u8],
    dialect: Dialect,
) -> Result<usize, SmallErr> {
    if bytes.get(start..start + spelling.len()) != Some(spelling) {
        let at = if bytes.len() < start + spelling.len() && spelling.starts_with(&bytes[start..]) {
            bytes.len()
        } else {
            start
        };
        return Err(error::invalid_literal(at));
    }
    let end = start + spelling.len();
    if is_token_end(bytes, end, dialect) {
        Ok(end)
    } else {
        Err(error::invalid_literal(end))
    }
}

fn enter_depth(depth: u32, offset: usize, max: u32) -> Result<u32, SmallErr> {
    if depth >= max {
        return Err(error::limit(offset));
    }
    Ok(depth + 1)
}

/// Skip `{` … `}`. After `}`, `, {` continues as the next sibling object
/// (`fuse`), so an array of objects does not return per element.
fn skip_object<const RFC: bool, const VALUES: bool, const COUNT: bool>(
    bytes: &[u8],
    pos: usize,
    ctx: Ctx,
    count: &mut u64,
    depth: u32,
    fuse: bool,
) -> Result<usize, SmallErr> {
    let child_depth = enter_depth(depth, pos, ctx.max)?;
    let mut cursor = trivia::<RFC>(bytes, pos + 1, ctx.dialect)?;
    let mut first = true;
    loop {
        let Some(&byte) = bytes.get(cursor) else {
            return Err(error::expected_key(cursor));
        };
        if byte == b'}' {
            let close = cursor + 1;
            // Only a dialect object is ever fused (the array passes `!RFC`), so
            // the RFC path folds this away entirely.
            if !RFC && fuse {
                let after = skip_trivia(bytes, close, ctx.dialect)?;
                if bytes.get(after) == Some(&b',') {
                    let next = skip_trivia(bytes, after + 1, ctx.dialect)?;
                    if bytes.get(next) == Some(&b'{') {
                        if COUNT {
                            *count = count.saturating_add(1);
                        }
                        cursor = trivia::<RFC>(bytes, next + 1, ctx.dialect)?;
                        first = true;
                        continue;
                    }
                }
            }
            return Ok(close);
        }
        if !first {
            if byte != b',' {
                return Err(error::expected_comma_object(cursor));
            }
            cursor = trivia::<RFC>(bytes, cursor + 1, ctx.dialect)?;
            if bytes.get(cursor) == Some(&b'}') {
                if ctx.dialect.trailing_commas() {
                    return Ok(cursor + 1);
                }
                return Err(error::trailing_comma(cursor));
            }
        }
        cursor = skip_member::<RFC, VALUES>(bytes, cursor, ctx)?;
        cursor = present::<RFC, VALUES, COUNT>(bytes, cursor, ctx, count, child_depth)?;
        first = false;
        cursor = trivia::<RFC>(bytes, cursor, ctx.dialect)?;
    }
}

/// Member head: a plain double-quoted key with the `:` adjacent is one
/// `plain_key` run plus post-colon trivia; otherwise the general key path.
#[allow(clippy::inline_always)] // hot member loop: forced inline is intentional
#[inline(always)]
fn skip_member<const RFC: bool, const VALUES: bool>(bytes: &[u8], cursor: usize, ctx: Ctx) -> Result<usize, SmallErr> {
    if bytes.get(cursor) == Some(&b'"')
        && let Some((_, key_end)) = string::plain_key(bytes, cursor)
        && bytes.get(key_end) == Some(&b':')
    {
        return trivia::<RFC>(bytes, key_end + 1, ctx.dialect);
    }
    if RFC {
        if bytes.get(cursor) != Some(&b'"') {
            return Err(error::expected_key(cursor));
        }
        let mut cursor = skip_string_checked::<true, VALUES>(bytes, cursor, ctx.dialect)?;
        cursor = ws_rfc(bytes, cursor);
        if bytes.get(cursor) != Some(&b':') {
            return Err(error::expected_colon(cursor));
        }
        Ok(ws_rfc(bytes, cursor + 1))
    } else {
        let mut cursor = skip_key(bytes, cursor, check_of::<VALUES>(), ctx.dialect)?;
        cursor = skip_trivia(bytes, cursor, ctx.dialect)?;
        if bytes.get(cursor) != Some(&b':') {
            return Err(error::expected_colon(cursor));
        }
        skip_trivia(bytes, cursor + 1, ctx.dialect)
    }
}

/// Skip `[` … `]`, reusing the object walk for `, {` siblings.
fn skip_array<const RFC: bool, const VALUES: bool, const COUNT: bool>(
    bytes: &[u8],
    pos: usize,
    ctx: Ctx,
    count: &mut u64,
    depth: u32,
) -> Result<usize, SmallErr> {
    let child_depth = enter_depth(depth, pos, ctx.max)?;
    let mut cursor = pos + 1;
    let mut first = true;
    loop {
        cursor = trivia::<RFC>(bytes, cursor, ctx.dialect)?;
        let Some(&byte) = bytes.get(cursor) else {
            return Err(error::expected_comma_array(cursor));
        };
        if byte == b']' {
            return Ok(cursor + 1);
        }
        if !first {
            if byte != b',' {
                return Err(error::expected_comma_array(cursor));
            }
            cursor = trivia::<RFC>(bytes, cursor + 1, ctx.dialect)?;
            if bytes.get(cursor) == Some(&b']') {
                if ctx.dialect.trailing_commas() {
                    return Ok(cursor + 1);
                }
                return Err(error::trailing_comma(cursor));
            }
        }
        // Only a dialect object fuses siblings; RFC folds this test away and
        // rides the class dispatch in `present`.
        if !RFC && bytes.get(cursor) == Some(&b'{') {
            if COUNT {
                *count = count.saturating_add(1);
            }
            cursor = skip_object::<RFC, VALUES, COUNT>(bytes, cursor, ctx, count, child_depth, true)?;
        } else {
            cursor = present::<RFC, VALUES, COUNT>(bytes, cursor, ctx, count, child_depth)?;
        }
        first = false;
    }
}
