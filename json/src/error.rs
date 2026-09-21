//! JSON `structury::Error` constructors. Codes match the scan walk.
//!
//! The hot path raises [`small::SmallErr`]. The public constructors delegate to
//! the same table.

use structury::{Error, ErrorClass};

macro_rules! codes {
    ($( $name:ident, $class:ident, $code:literal, $message:literal; )*) => {
        /// Compact hot-path refusal. [`From`] materializes the public error.
        pub(crate) mod small {
            use super::{Error, ErrorClass, sat_u32};

            #[derive(Clone, Copy, Debug, Eq, PartialEq)]
            pub(crate) struct SmallErr {
                code: u8,
                offset: u32,
            }

            #[derive(Clone, Copy)]
            #[allow(non_camel_case_types, reason = "variant names mirror the constructor names")]
            #[repr(u8)]
            enum Code {
                $( $name, )*
            }

            struct Info {
                class: ErrorClass,
                code: &'static str,
                message: &'static str,
            }

            const INFOS: &[Info] = &[
                $( Info { class: ErrorClass::$class, code: $code, message: $message }, )*
            ];

            impl SmallErr {
                const fn new(code: u8, offset: usize) -> Self {
                    Self {
                        code,
                        offset: sat_u32(offset),
                    }
                }
            }

            $(
                #[must_use]
                pub(crate) const fn $name(offset: usize) -> SmallErr {
                    SmallErr::new(Code::$name as u8, offset)
                }
            )*

            impl From<SmallErr> for Error {
                fn from(err: SmallErr) -> Self {
                    let info = &INFOS[usize::from(err.code)];
                    Error::new(info.class, info.code, info.message, err.offset as usize)
                }
            }
        }
    };
}

/// Materialize a hot-path refusal for the crate's non-hot modules.
macro_rules! exposed {
    ($( $name:ident ),* $(,)?) => {
        $(
            pub(crate) fn $name(offset: usize) -> Error {
                small::$name(offset).into()
            }
        )*
    };
}

codes! {
    expected_value, Syntax, "expected-value", "expected one complete JSON value";
    expected_key, Syntax, "expected-key", "expected an object string key";
    expected_colon, Syntax, "expected-colon", "expected `:`";
    expected_comma_object, Syntax, "expected-comma", "expected `,` or `}`";
    expected_comma_array, Syntax, "expected-comma", "expected `,` or `]`";
    trailing_comma, Syntax, "trailing-comma", "trailing comma is not permitted";
    trailing_content, Syntax, "trailing-content", "trailing content after JSON value";
    unterminated_string, Syntax, "unterminated-string", "unterminated JSON string";
    unterminated_comment, Syntax, "unterminated-comment", "unterminated block comment";
    invalid_comment, Syntax, "invalid-comment", "`/` does not start a comment";
    invalid_number, Number, "invalid-number", "invalid JSON number";
    leading_zeros, Number, "invalid-number", "a JSON number cannot have leading zeros";
    incomplete_number, Number, "invalid-number", "incomplete JSON number";
    invalid_literal, Syntax, "invalid-literal", "invalid JSON literal";
    invalid_escape, Escape, "invalid-escape", "invalid JSON escape";
    control_in_string, Syntax, "control-in-string", "unescaped control byte in string";
    invalid_unicode_escape, Escape, "invalid-unicode-escape", "invalid Unicode escape";
    missing_low_surrogate, Escape, "missing-low-surrogate", "high surrogate must be followed by a low surrogate";
    invalid_surrogate_pair, Escape, "invalid-surrogate-pair", "invalid Unicode surrogate pair";
    utf8, Utf8, "invalid-utf8", "input is not valid UTF-8";
    limit, Limit, "nesting", "nesting exceeds the codec bound";
    predicate_operand, Shape, "predicate-operand", "predicate equality operand must be a scalar";
    edit_unplaceable, Write, "edit-unplaceable", "edit cannot be spliced in place";
    cancelled, Control, "cancelled", "request cancelled by the host";
    deadline_exceeded, Control, "deadline-exceeded", "request deadline exceeded";
    memory_exceeded, Control, "memory-exceeded", "physical memory ceiling exceeded";
}

exposed! {
    expected_value,
    expected_key,
    expected_colon,
    expected_comma_object,
    expected_comma_array,
    trailing_comma,
    trailing_content,
    unterminated_string,
    unterminated_comment,
    invalid_comment,
    invalid_number,
    utf8,
    limit,
    predicate_operand,
    edit_unplaceable,
}

pub(crate) fn shape(message: &'static str, offset: usize) -> Error {
    Error::new(ErrorClass::Shape, "invalid-shape", message, offset)
}

pub(crate) fn write(message: &'static str, offset: usize) -> Error {
    Error::new(ErrorClass::Write, "write", message, offset)
}

/// A byte-preserving write whose document was validated under another grammar.
pub(crate) fn dialect_mismatch(offset: usize) -> Error {
    Error::new(
        ErrorClass::Write,
        "dialect-mismatch",
        "a verbatim write requires the document's validating dialect",
        offset,
    )
}

#[allow(clippy::cast_possible_truncation, reason = "saturating conversion to u32 offset")]
const fn sat_u32(n: usize) -> u32 {
    if n > u32::MAX as usize { u32::MAX } else { n as u32 }
}
