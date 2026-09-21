//! Parallel value encode: the codec cuts, the host threads encode.
//!
//! Mirrors the decode seam (`Plan::build` / `scan` / `stitch`): planning cuts
//! the top-level array into contiguous index ranges, per-part encode reuses
//! the serial writers at element depth 1, and the stitch re-adds separators
//! plus brackets. This module starts no thread and uses `alloc` only.
//!
//! Only top-level arrays split. Every other value plans as
//! [`ValuePlan::Serial`], as do small arrays whose parts would hold fewer
//! than [`MIN_ITEMS_PER_PART`] items, so small writes never pay fan-out.

use alloc::vec::Vec;

use structury::{Error, Value};

use crate::encode::{EncodeOptions, ItemFraming};
use crate::error;

/// Items per part below which planning falls back to [`ValuePlan::Serial`].
pub const MIN_ITEMS_PER_PART: usize = 512;

/// One chunk of top-level array items, half-open `[lo, hi)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemRange {
    /// First item index in the chunk.
    pub lo: usize,
    /// One past the last item index in the chunk.
    pub hi: usize,
}

/// Value-case plan: chunks of the top-level array, or one serial part.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValuePlan {
    /// Independent item chunks; each encodes without brackets.
    Chunks(Vec<ItemRange>),
    /// Serial fallback: not an array, fewer than two items, a framed write,
    /// or fewer than [`MIN_ITEMS_PER_PART`] items per part.
    Serial,
}

/// Plan a value encode: chunk the top-level array into at most `parts`
/// contiguous index ranges. `parts <= 1` means one chunk covering the array.
#[must_use]
pub fn plan_encode_value(value: &Value, opts: &EncodeOptions, parts: usize) -> ValuePlan {
    if opts.framing != ItemFraming::None {
        return ValuePlan::Serial;
    }
    let Value::Array(items) = value else {
        return ValuePlan::Serial;
    };
    if items.len() < 2 {
        return ValuePlan::Serial;
    }
    let mut want = parts.max(1).min(items.len());
    if want >= 2 {
        want = want.min((items.len() / MIN_ITEMS_PER_PART).max(1));
        if want < 2 {
            return ValuePlan::Serial;
        }
    }
    let width = items.len().div_ceil(want);
    let mut chunks = Vec::new();
    let mut lo = 0;
    while lo < items.len() {
        let hi = (lo + width).min(items.len());
        chunks.push(ItemRange { lo, hi });
        lo = hi;
    }
    ValuePlan::Chunks(chunks)
}

/// Encode `range` of the top-level array: the items joined with the array
/// separator at element depth 1, without brackets. Byte-identical to the
/// serial array body because every top-level element sits at depth 1.
///
/// # Errors
///
/// `value` is not an array, `range` is out of bounds, or a nested write fails.
pub fn encode_value_chunk(
    value: &Value,
    range: &ItemRange,
    opts: &EncodeOptions,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    let Value::Array(items) = value else {
        return Err(error::shape("parallel encode chunk needs an array value", 0));
    };
    if range.lo > range.hi || range.hi > items.len() {
        return Err(error::shape("parallel encode chunk is out of bounds", 0));
    }
    out.reserve(range.hi.saturating_sub(range.lo).saturating_mul(40));
    for (n, item) in items[range.lo..range.hi].iter().enumerate() {
        if n > 0 {
            out.push(b',');
            if opts.pretty {
                crate::encode::newline_indent(&mut *out, *opts, 1);
            }
        }
        crate::encode::write_value(item, *opts, out, 1)?;
    }
    Ok(())
}

/// Stitch value-chunk buffers: `[` plus separator-joined chunks plus the
/// pretty closer plus `]`. Empty buffers are skipped, so an empty chunk set
/// stitches to `[]` (pretty included, as the serial writer emits).
#[must_use]
pub fn stitch_value_chunks(chunks: &[Vec<u8>], opts: &EncodeOptions) -> Vec<u8> {
    let body: usize = chunks.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(body.saturating_add(chunks.len() + 2));
    out.push(b'[');
    let mut first = true;
    for chunk in chunks {
        if chunk.is_empty() {
            continue;
        }
        if first {
            first = false;
        } else {
            out.push(b',');
        }
        // Chunks hold bare elements: the array-level indent before each chunk
        // replays the serial array writer.
        if opts.pretty {
            crate::encode::newline_indent(&mut out, *opts, 1);
        }
        out.extend_from_slice(chunk);
    }
    if opts.pretty && !first {
        crate::encode::newline_indent(&mut out, *opts, 0);
    }
    out.push(b']');
    out
}
