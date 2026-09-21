//! Codec errors and recovering issues.
//!
//! [`Error`] is a neutral class plus the byte offset it concerns, [`Issue`] is the recovering channel.
//  The host owns I/O, so there are no I/O classes.

use core::fmt;

/// Neutral class of a fatal refusal. The vocabulary can grow, so an external
/// `match` must keep a wildcard arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorClass {
    /// Structure, literals, commas, colons, trailing content.
    Syntax,
    /// Number grammar or scale.
    Number,
    /// Invalid UTF-8 in a demanded string.
    Utf8,
    /// Escape / surrogate refusal.
    Escape,
    /// Demand the codec cannot serve faithfully.
    Shape,
    /// Nesting or other codec-side bound.
    Limit,
    /// Write/edit of a document that was not fully validated.
    Write,
    /// A host control stop: cancelled, past the deadline, or over the memory ceiling.
    Control,
}

impl ErrorClass {
    /// Neutral class name; the format crate prefixes its id for the wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "syntax",
            Self::Number => "number",
            Self::Utf8 => "utf8",
            Self::Escape => "escape",
            Self::Shape => "shape",
            Self::Limit => "limit",
            Self::Write => "write",
            Self::Control => "control",
        }
    }
}

/// Fatal codec refusal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    class: ErrorClass,
    offset: u32,
    code: &'static str,
    message: &'static str,
}

impl Error {
    /// Build an error at `offset`.
    #[must_use]
    pub const fn new(class: ErrorClass, code: &'static str, message: &'static str, offset: usize) -> Self {
        Self {
            class,
            offset: sat_u32(offset),
            code,
            message,
        }
    }

    /// Neutral class.
    #[must_use]
    pub const fn class(&self) -> ErrorClass {
        self.class
    }

    /// Byte offset of the first byte the error concerns.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Stable machine code (`expected-value`, `invalid-number`, …).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// Human message.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} at byte {} ({})",
            self.class.as_str(),
            self.message,
            self.offset,
            self.code
        )
    }
}

impl core::error::Error for Error {}

/// Recovering (non-fatal) diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Issue {
    /// Neutral class, same vocabulary as [`Error`].
    pub class: ErrorClass,
    /// Byte offset.
    pub offset: u32,
    /// Stable machine code.
    pub code: &'static str,
    /// Human message.
    pub message: &'static str,
}

impl From<Error> for Issue {
    fn from(error: Error) -> Self {
        Self {
            class: error.class,
            offset: error.offset,
            code: error.code,
            message: error.message,
        }
    }
}

#[allow(clippy::cast_possible_truncation, reason = "saturating conversion to u32 offset")]
const fn sat_u32(n: usize) -> u32 {
    if n > u32::MAX as usize { u32::MAX } else { n as u32 }
}
