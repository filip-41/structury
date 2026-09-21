//! Codec-owned grammar dialects. Core stays dialect-neutral.
//! [`Dialect`] selects the grammar the lexer and walk read.

use structury::{GrammarTag, NonFinite};

/// Grammar the codec reads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Dialect {
    /// RFC 8259, no comments, no trailing commas.
    #[default]
    Rfc8259,
    /// RFC 8259 plus `//` / `/* */` comments and trailing commas.
    Jsonc,
    /// JSONC plus the JSON5 subset: single-quoted strings, bare keys, extended numbers.
    Json5,
}

impl Dialect {
    /// Whether `//` and `/* */` comments are trivia.
    #[must_use]
    pub const fn has_comments(self) -> bool {
        matches!(self, Self::Jsonc | Self::Json5)
    }

    /// Whether a comma before a closer is allowed.
    #[must_use]
    pub(crate) const fn trailing_commas(self) -> bool {
        matches!(self, Self::Jsonc | Self::Json5)
    }

    /// Whether JSON5 scalars and bare keys are allowed.
    #[must_use]
    pub(crate) const fn json5(self) -> bool {
        matches!(self, Self::Json5)
    }

    /// The spelling this grammar writes for `value`.
    /// `None` when it admits no non-finite numbers.
    #[must_use]
    pub const fn non_finite_spelling(self, value: NonFinite) -> Option<&'static str> {
        if !self.json5() {
            return None;
        }
        Some(match value {
            NonFinite::Infinity => "Infinity",
            NonFinite::NegativeInfinity => "-Infinity",
            NonFinite::NaN => "NaN",
        })
    }

    /// Provenance tag a Strict pass records on a produced document.
    /// [`encode`](crate::encode) compares it against the write options before
    /// a byte-preserving write.
    #[must_use]
    pub const fn grammar_tag(self) -> GrammarTag {
        GrammarTag::codec(match self {
            Self::Rfc8259 => 1,
            Self::Jsonc => 2,
            Self::Json5 => 3,
        })
    }
}
