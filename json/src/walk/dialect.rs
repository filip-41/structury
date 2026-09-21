//! Dialect scan strategy: the `lex` walk (JSONC / JSON5).

use crate::dialect::Dialect;
use crate::error::small::SmallErr;
use crate::lex::{self, Check};

use super::scan::Scan;

/// Dialect scan strategy: the existing `lex` walk (JSONC / JSON5).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DialectScan;

impl Scan for DialectScan {
    fn trivia_starts(byte: u8) -> bool {
        byte <= 0x20 || byte == b'/'
    }

    fn skip_trivia(bytes: &[u8], pos: usize, dialect: Dialect) -> Result<usize, SmallErr> {
        lex::skip_trivia(bytes, pos, dialect)
    }

    fn skip_string(bytes: &[u8], pos: usize, check: Check, dialect: Dialect) -> Result<usize, SmallErr> {
        match check {
            Check::Locate => lex::skip_string_locate(bytes, pos, dialect),
            Check::Values => lex::skip_string(bytes, pos, dialect),
        }
    }
}
