//! RFC 8259 byte-level seams for the demand walk.
//!
//! Value skipping is the one engine in [`crate::lex`]; this module keeps only the RFC tokenizer's seams, so the walk pays no dialect branch on the RFC hot path.

#![expect(
    clippy::inline_always,
    reason = "the per-token scan must fold into the walk; a call per token is the residual this removes"
)]

use structury::ByteRange;

use crate::dialect::Dialect;
use crate::error::small::SmallErr;
use crate::lex::{self, Check};

use super::scan::Scan;

/// RFC scan strategy for the demand walk.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RfcScan;

impl Scan for RfcScan {
    #[inline(always)]
    fn skip_trivia(bytes: &[u8], pos: usize, _dialect: Dialect) -> Result<usize, SmallErr> {
        Ok(lex::ws_rfc(bytes, pos))
    }

    fn trivia_starts(byte: u8) -> bool {
        byte <= 0x20
    }

    #[inline(always)]
    fn skip_string(bytes: &[u8], pos: usize, check: Check, _dialect: Dialect) -> Result<usize, SmallErr> {
        match check {
            Check::Locate => lex::string::locate_double_quoted(bytes, pos),
            Check::Values => lex::string::skip_string_rfc(bytes, pos),
        }
    }

    #[inline(always)]
    fn plain_member(bytes: &[u8], start: usize) -> Option<(ByteRange, usize)> {
        let (_, key_end) = lex::string::plain_key(bytes, start)?;
        if bytes.get(key_end) != Some(&b':') {
            return None;
        }
        let value_at = lex::ws_rfc(bytes, key_end + 1);
        (value_at < bytes.len()).then(|| (ByteRange::try_new(start + 1, key_end - 1).expect("ordered"), value_at))
    }
}
