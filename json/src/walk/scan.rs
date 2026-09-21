//! The [`Scan`] strategy seam behind the demand walk.
//!
//! The walk ([`super`]) is shared; only byte-level scanning differs, monomorphized so each strategy carries no branch for the other.

#![expect(
    clippy::inline_always,
    reason = "a provided method is the per-token scan; forcing it inline folds it into the walk"
)]

use alloc::string::String;

use structury::ByteRange;

use crate::dialect::Dialect;
use crate::error::small::SmallErr;
use crate::lex::{self, Check};

/// Structural-scan strategy behind the demand walk: the byte-level scanning the
/// shared walk calls. Required items are the seams between strategies; provided
/// methods are the commonalities.
pub(crate) trait Scan: Copy {
    /// Longest trivia run from `pos`: whitespace, and comments for dialects.
    fn skip_trivia(bytes: &[u8], pos: usize, dialect: Dialect) -> Result<usize, SmallErr>;

    /// Advance past one string (or key string) at `pos` under `check`.
    fn skip_string(bytes: &[u8], pos: usize, check: Check, dialect: Dialect) -> Result<usize, SmallErr>;

    /// Whether `byte` can begin trivia. True for every byte
    /// [`Self::skip_trivia`] would consume. A false answer skips the scan.
    fn trivia_starts(_byte: u8) -> bool {
        true
    }

    /// Skip one value at `pos`, skipping leading trivia first: the one skip
    /// engine ([`lex::skip_value_at`]), shared by every strategy.
    #[inline(always)]
    fn skip_value_at(
        bytes: &[u8],
        pos: usize,
        check: Check,
        depth: u32,
        max: u32,
        dialect: Dialect,
    ) -> Result<usize, SmallErr> {
        lex::skip_value_at(bytes, pos, check, depth, max, dialect)
    }

    /// Skip one value that starts at `pos` (no leading trivia).
    #[inline(always)]
    fn skip_present_at(
        bytes: &[u8],
        pos: usize,
        check: Check,
        depth: u32,
        max: u32,
        dialect: Dialect,
    ) -> Result<usize, SmallErr> {
        lex::skip_present_at(bytes, pos, check, depth, max, dialect)
    }

    /// Skip one value at `pos` and count its preorder nodes.
    #[inline(always)]
    fn skip_value_counting(
        bytes: &[u8],
        pos: usize,
        check: Check,
        max: u32,
        dialect: Dialect,
    ) -> Result<(usize, u64), SmallErr> {
        lex::skip_value_counting(bytes, pos, check, max, dialect)
    }

    /// A plain member head at `start` — the quoted key and the `:`, with the
    /// value start already past any trivia after the colon — returning the key's
    /// inner range and the value start. The caller has already skipped trivia
    /// before the key, so only the after-colon trivia is owned here. `None` sends
    /// the member down the general path, which owns escaped keys and dialects
    /// with comments or bare identifiers.
    fn plain_member(_bytes: &[u8], _start: usize) -> Option<(ByteRange, usize)> {
        None
    }

    /// Skip one object key at `pos` (double-quoted, or a dialect's `'…'`/bare
    /// identifier) under `check`.
    #[inline(always)]
    fn skip_key(bytes: &[u8], pos: usize, check: Check, dialect: Dialect) -> Result<usize, SmallErr> {
        lex::skip_key(bytes, pos, check, dialect)
    }

    /// A plain double-quoted key's inner span at `start`, without decoding; the
    /// second value is the key end.
    #[inline(always)]
    fn plain_key(bytes: &[u8], start: usize) -> Option<(ByteRange, usize)> {
        lex::string::plain_key(bytes, start)
            .map(|(_, end)| (ByteRange::try_new(start + 1, end - 1).expect("ordered"), end))
    }

    /// Inner source span of a key at `start..end` when its bytes equal its
    /// decoded text; `None` when the key must be decoded to match a demand.
    #[inline(always)]
    fn key_plain_inner(bytes: &[u8], start: usize, end: usize, dialect: Dialect) -> Option<ByteRange> {
        lex::key_plain_inner(bytes, start, end, dialect)
    }

    /// Decode one string (or key string) at `start` into `text`.
    #[inline(always)]
    fn parse_string_into(bytes: &[u8], start: usize, text: &mut String, dialect: Dialect) -> Result<usize, SmallErr> {
        lex::parse_string_into(bytes, start, text, dialect)
    }
}
