//! Owned semantic values.
//!
//! Absence is still [`crate::Answer`]. Numbers are exact
//! ([`crate::Number`]) with no f64 arm. [`Value::as_f64`] is the one explicit,
//! lossy projection.

use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt;

use crate::compact::CompactStr;
use crate::number::Number;

/// Kind of a value. Used by oracles and type mismatch marks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueKind {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool,
    /// Number (int or decimal).
    Number,
    /// String.
    String,
    /// Array.
    Array,
    /// Object.
    Object,
}

/// Owned semantic value.
#[derive(Clone, Debug)]
pub enum Value {
    /// Null.
    Null,
    /// Boolean.
    Bool(bool),
    /// Exact number with authored spelling.
    Number(Number),
    /// UTF-8 string.
    Str(CompactStr),
    /// Array.
    Array(Vec<Value>),
    /// Object, first-key last-wins order.
    Object(Vec<(CompactStr, Value)>),
}

impl Value {
    /// Object member by decoded key, last-wins.
    #[must_use]
    pub fn member(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Object(members) => members.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Array element at `index`. Negatives count from the end.
    #[must_use]
    pub fn element(&self, index: i64) -> Option<&Value> {
        match self {
            Self::Array(items) => resolve_index(items.len(), index).and_then(|i| items.get(i)),
            _ => None,
        }
    }

    /// Decoded string text when this is a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(text) => Some(text.as_str()),
            _ => None,
        }
    }

    /// Boolean when this is a boolean.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// `i128` when this is an integer that fits; `None` for fractions, wide
    /// integers, and non-finite numbers.
    #[must_use]
    pub fn as_i128(&self) -> Option<i128> {
        match self {
            Self::Number(number) => number.to_i128(),
            _ => None,
        }
    }

    /// `i64` when this is an integer that fits.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(number) => number.to_i64(),
            _ => None,
        }
    }

    /// `u64` when this is a non-negative integer that fits.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(number) => number.to_u64(),
            _ => None,
        }
    }

    /// Nearest `f64` for a number; **lossy** (see [`Number::to_f64`]).
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(number) => number.to_f64(),
            _ => None,
        }
    }

    /// Array elements; `None` when this is not an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Object members in first-key last-wins order; `None` when not an object.
    #[must_use]
    pub fn as_object(&self) -> Option<&[(CompactStr, Value)]> {
        match self {
            Self::Object(members) => Some(members),
            _ => None,
        }
    }

    /// Array elements in order; empty when this is not an array.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    pub fn elements(&self) -> impl Iterator<Item = &Value> {
        self.as_array().unwrap_or(&[]).iter()
    }

    /// Semantic equality: finite numbers compare by value (`1.50` equals `1.5`),
    /// a non-finite value equals only the same non-finite value, arrays compare
    /// element-wise, and objects compare by member name **ignoring order**, since
    /// member order is not significant. This is what `==` uses and what the
    /// codec's `Eq`/`Ne` filter arms mean.
    ///
    /// The codec's objects hold unique names, which this rule assumes (the tape
    /// collapses duplicates last-wins); a hand-built object holding duplicate
    /// names compares its pairs, not its last-wins view.
    ///
    /// Contrast [`Value::strict_equal`], which preserves spelling, member order,
    /// and structure.
    #[must_use]
    #[inline]
    pub fn equal(&self, other: &Value) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => {
                a == b || a.numeric_cmp_different_spelling(b) == Some(Ordering::Equal)
            }
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.equal(y)),
            (Self::Object(a), Self::Object(b)) => object_equal(a, b),
            _ => false,
        }
    }

    /// Structural, spelling-preserving equality: `1.50` and `1.5` differ, arrays
    /// and objects compare member-wise, and member order and count are part of
    /// the structure. The method form of the strict notion; `==` is
    /// [`Value::equal`] instead.
    #[must_use]
    #[inline]
    pub fn strict_equal(&self, other: &Value) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.strict_equal(y)),
            (Self::Object(a), Self::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ka, va), (kb, vb))| ka == kb && va.strict_equal(vb))
            }
            _ => false,
        }
    }

    /// Mathematical ordering: numbers compare by value and a boolean counts as
    /// `1`/`0`. `None` when either side is not a number, which the codec's
    /// ordering arms treat as "no match".
    #[must_use]
    pub fn compare(&self, other: &Value) -> Option<Ordering> {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.numeric_cmp(b),
            (Self::Bool(a), Self::Bool(b)) => Some(a.cmp(b)),
            (Self::Bool(flag), Self::Number(number)) => bool_number(*flag).numeric_cmp(number),
            (Self::Number(number), Self::Bool(flag)) => number.numeric_cmp(&bool_number(*flag)),
            _ => None,
        }
    }

    /// Object members in first-key order as `(name, value)`; empty when not an object.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    pub fn members(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.as_object()
            .unwrap_or(&[])
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }
}

/// `==` is [`Value::equal`]: numeric-aware, and reflexive because a non-finite
/// value compares by variant. Use [`Value::strict_equal`] for the
/// spelling-preserving notion.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl Eq for Value {}

/// Compact diagnostic rendering: authored number spellings, strings quoted with
/// backslash escapes.
///
/// This is not a codec. It does not know which grammar the value was read under:
/// a non-finite number prints its canonical name (`Infinity`, `NaN`), and source
/// comments are not represented. Use the codec's encoder for a faithful,
/// grammar-aware write.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(true) => f.write_str("true"),
            Self::Bool(false) => f.write_str("false"),
            Self::Number(number) => f.write_str(number.spelling()),
            Self::Str(text) => write_quoted(f, text),
            Self::Array(items) => {
                f.write_str("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_str(",")?;
                    }
                    item.fmt(f)?;
                }
                f.write_str("]")
            }
            Self::Object(members) => {
                f.write_str("{")?;
                for (index, (key, value)) in members.iter().enumerate() {
                    if index > 0 {
                        f.write_str(",")?;
                    }
                    write_quoted(f, key)?;
                    f.write_str(":")?;
                    value.fmt(f)?;
                }
                f.write_str("}")
            }
        }
    }
}

/// Write `text` as a quoted string literal with backslash escapes.
fn write_quoted(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    f.write_str("\"")?;
    for ch in text.chars() {
        match ch {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\u{0008}' => f.write_str("\\b")?,
            '\u{000c}' => f.write_str("\\f")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            ch if (ch as u32) < 0x20 => write!(f, "\\u{:04x}", ch as u32)?,
            ch => f.write_fmt(format_args!("{ch}"))?,
        }
    }
    f.write_str("\"")
}

/// Semantic object equality: members compare by name, ignoring order.
///
/// The fast path is the common one — the codec emits the same member order for
/// equivalent input — and costs one key pass plus one value pass over the
/// aligned members. Only an object whose key sequence differs reaches the pair
/// match, which allocates nothing and is quadratic in the member count.
fn object_equal(a: &[(CompactStr, Value)], b: &[(CompactStr, Value)]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    if a.iter().zip(b).all(|((ka, _), (kb, _))| ka == kb) {
        return a.iter().zip(b).all(|((_, va), (_, vb))| va.equal(vb));
    }
    pairs_covered(a, b) && pairs_covered(b, a)
}

/// Whether every `(name, value)` pair of `x` has an equal pair in `y`.
fn pairs_covered(x: &[(CompactStr, Value)], y: &[(CompactStr, Value)]) -> bool {
    x.iter()
        .all(|(kx, vx)| y.iter().any(|(ky, vy)| ky == kx && vy.equal(vx)))
}

/// The number a boolean coerces to: `1` or `0`.
fn bool_number(flag: bool) -> Number {
    Number::Int {
        spelling: CompactStr::from(if flag { "1" } else { "0" }),
    }
}

/// Resolve a possibly-negative index against `len`. `None` when out of range.
///
/// ```
/// assert_eq!(structury::resolve_index(3, -1), Some(2));
/// assert_eq!(structury::resolve_index(3, 3), None);
/// ```
#[must_use]
pub fn resolve_index(len: usize, index: i64) -> Option<usize> {
    if index >= 0 {
        let i = usize::try_from(index).ok()?;
        (i < len).then_some(i)
    } else {
        let mag = usize::try_from(index.checked_neg()?).ok()?;
        len.checked_sub(mag)
    }
}
