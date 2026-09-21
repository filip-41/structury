//! Frame boundaries for adjacent values, NDJSON, and JSON-seq.

use alloc::vec::Vec;

use structury::ByteRange;
use structury::byte_scan::prefix_len;

use crate::dialect::Dialect;
use crate::lex;
use crate::lex::MAX_NESTING;
use crate::lex::stop_sets::{JsonSeqFrame, NdjsonFrame};

fn push_range(out: &mut Vec<ByteRange>, start: usize, end: usize) {
    if let Some(range) = ByteRange::try_new(start, end)
        && start < end
    {
        out.push(range);
    }
}

/// Record-aligned NDJSON morsels of about `target` bytes.
/// Frame ends are newlines, so any dialect reads the same cuts.
#[must_use]
pub fn partition_ndjson(bytes: &[u8], target: usize) -> Vec<ByteRange> {
    let target = target.max(1);
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut pos = 0usize;
    while pos < bytes.len() {
        pos = ndjson_frame_end(bytes, pos);
        if pos.saturating_sub(start) >= target && pos < bytes.len() {
            push_range(&mut out, start, pos);
            start = pos;
        }
    }
    if start < bytes.len() {
        push_range(&mut out, start, bytes.len());
    }
    out
}

/// Top-level adjacent-value morsels of about `target` bytes.
/// RFC 8259 only; callers gate commenting dialects out of this cut.
#[must_use]
pub fn partition_adjacent(bytes: &[u8], target: usize) -> Vec<ByteRange> {
    let Ok(values) = adjacent_values(bytes, MAX_NESTING, Dialect::Rfc8259) else {
        return Vec::new();
    };
    pack_ranges(&values, target)
}

/// JSON-seq morsels of about `target` bytes.
/// RFC 8259 only; callers gate commenting dialects out of this cut.
#[must_use]
pub fn partition_json_seq(bytes: &[u8], target: usize) -> Vec<ByteRange> {
    let Ok(values) = json_seq_ranges(bytes, MAX_NESTING, Dialect::Rfc8259, true) else {
        return Vec::new();
    };
    pack_ranges(&values, target)
}

fn pack_ranges(values: &[ByteRange], target: usize) -> Vec<ByteRange> {
    let last_end = values.last().map_or(0, |value| value.end());
    pack_runs(values, target, false, last_end)
}

/// Pack whole values into runs of at least `target` bytes.
/// When `contiguous`, each sealed run ends at the next value's start.
/// Otherwise it ends at the value itself. `final_end` closes the last run.
pub(crate) fn pack_runs(values: &[ByteRange], target: usize, contiguous: bool, final_end: usize) -> Vec<ByteRange> {
    let target = target.max(1);
    let Some(first) = values.first() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut start = first.start();
    for (i, value) in values.iter().enumerate() {
        if value.end().saturating_sub(start) >= target && i + 1 < values.len() {
            let end = if contiguous { values[i + 1].start() } else { value.end() };
            push_range(&mut out, start, end);
            start = values[i + 1].start();
        }
    }
    push_range(&mut out, start, final_end);
    out
}

pub(crate) fn adjacent_values(bytes: &[u8], max: u32, dialect: Dialect) -> Result<Vec<ByteRange>, structury::Error> {
    let mut out = Vec::new();
    let mut pos = crate::scan::strip_bom(bytes);
    pos = lex::skip_trivia(bytes, pos, dialect)?;
    while pos < bytes.len() {
        let start = pos;
        pos = lex::skip_value(bytes, pos, lex::Check::Locate, max, dialect)?;
        push_range(&mut out, start, pos);
        pos = lex::skip_trivia(bytes, pos, dialect)?;
    }
    Ok(out)
}

/// Complete value ranges plus the tail offset where location stopped.
/// The tail is a partial value, trailing garbage, or `bytes.len()`.
/// Unlike [`adjacent_values`], a bad tail does not fail the leading ranges.
pub(crate) fn adjacent_split(bytes: &[u8], max: u32, dialect: Dialect) -> (Vec<ByteRange>, usize) {
    let mut out = Vec::new();
    let mut pos = crate::scan::strip_bom(bytes);
    let Ok(next) = lex::skip_trivia(bytes, pos, dialect) else {
        return (out, 0);
    };
    pos = next;
    while pos < bytes.len() {
        let start = pos;
        let Ok(value_end) = lex::skip_value(bytes, pos, lex::Check::Locate, max, dialect) else {
            break;
        };
        push_range(&mut out, start, value_end);
        if let Ok(next) = lex::skip_trivia(bytes, value_end, dialect) {
            pos = next;
        } else {
            pos = value_end;
            break;
        }
    }
    (out, pos)
}

/// End of the longest prefix holding only complete values, trailing trivia included.
/// A value touching the end without trailing trivia stays out: it may extend.
/// Feeders drain `[..n]` and hold the rest; `finish` scans the remainder whole.
#[must_use]
pub fn adjacent_prefix_len(bytes: &[u8], max_nesting: u32, dialect: Dialect) -> usize {
    let mut pos = crate::scan::strip_bom(bytes);
    let Ok(next) = lex::skip_trivia(bytes, pos, dialect) else {
        return 0;
    };
    pos = next;
    let mut end = pos;
    while pos < bytes.len() {
        let Ok(value_end) = lex::skip_value(bytes, pos, lex::Check::Locate, max_nesting, dialect) else {
            break;
        };
        let Ok(trivia_end) = lex::skip_trivia(bytes, value_end, dialect) else {
            break;
        };
        if trivia_end == bytes.len() && trivia_end == value_end {
            break;
        }
        end = trivia_end;
        pos = trivia_end;
    }
    end
}

/// Payload ranges of every JSON-seq record: skip the record separator and
/// leading trivia, then close the range at the located value (`locate`).
/// Without `locate`, the range ends at the record end.
pub(crate) fn json_seq_ranges(
    bytes: &[u8],
    max: u32,
    dialect: Dialect,
    locate: bool,
) -> Result<Vec<ByteRange>, structury::Error> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        if bytes[pos] == 0x1E {
            pos += 1;
        }
        let rec_start = pos;
        pos += prefix_len::<JsonSeqFrame>(&bytes[rec_start..]);
        let rec_end = pos;
        let rec = &bytes[rec_start..rec_end];
        let inner = lex::skip_trivia(rec, 0, dialect)?;
        if inner < rec.len() {
            let start = rec_start + inner;
            let end = if locate {
                lex::skip_value(bytes, start, lex::Check::Locate, max, dialect)?
            } else {
                rec_end
            };
            push_range(&mut out, start, end);
        }
    }
    Ok(out)
}

pub(crate) fn ndjson_frame_end(bytes: &[u8], start: usize) -> usize {
    let rest = &bytes[start..];
    let end = start + prefix_len::<NdjsonFrame>(rest);
    match bytes.get(end) {
        Some(b'\r') if bytes.get(end + 1) == Some(&b'\n') => end + 2,
        Some(b'\r' | b'\n') => end + 1,
        _ => bytes.len(),
    }
}

/// Leading bytes holding complete frames. A growing tail feeds only this
/// prefix and holds the rest back until it completes.
/// NDJSON and JSON-seq end a frame at a newline. Text holds nothing back.
/// Adjacent holds nothing back here; its feeders use [`adjacent_prefix_len`].
#[must_use]
pub fn complete_prefix_len(bytes: &[u8], input: crate::scan::JsonInput) -> usize {
    match input {
        crate::scan::JsonInput::Ndjson | crate::scan::JsonInput::JsonSeq => {
            let mut end = 0usize;
            let mut pos = 0usize;
            while pos < bytes.len() {
                let next = ndjson_frame_end(bytes, pos);
                if next == pos {
                    break;
                }
                // A frame reaching EOF without a terminator may still grow.
                if next >= bytes.len() && !bytes.ends_with(b"\n") && !bytes.ends_with(b"\r") {
                    break;
                }
                end = next;
                pos = next;
            }
            end
        }
        crate::scan::JsonInput::Text | crate::scan::JsonInput::Adjacent => bytes.len(),
    }
}
/// Payload ranges of NDJSON records with blank lines skipped.
/// Each range covers one non-blank record body without its terminator.
pub(crate) fn ndjson_payloads(
    src: &[u8],
    dialect: Dialect,
) -> Result<(Vec<ByteRange>, Vec<structury::Issue>), structury::Error> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < src.len() {
        let end = ndjson_frame_end(src, pos);
        let rec = &src[pos..end];
        let payload_end = rec
            .iter()
            .rposition(|b| !matches!(b, b'\n' | b'\r'))
            .map_or(0, |i| i + 1);
        let body = &rec[..payload_end];
        let inner = lex::skip_trivia(body, 0, dialect)?;
        if inner < body.len() {
            let start = pos + inner;
            if let Some(span) = ByteRange::try_new(start, pos + payload_end) {
                out.push(span);
            }
        }
        pos = end;
    }
    Ok((out, Vec::new()))
}
