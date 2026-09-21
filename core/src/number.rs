//! Exact integers and base-ten decimals
//!
//! Authored spelling is retained so encode can emit `1.50` and `-0` byte-exact. The numeric value is derived on demand.

use alloc::string::String;
use core::cmp::Ordering;
use core::fmt;

use crate::compact::CompactStr;

/// Why a number could not be built.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NumericError {
    /// No digits.
    Empty,
    /// A character was not a digit, sign, point, or exponent mark.
    InvalidDigit,
    /// Exponent missing, malformed, or out of `i32`.
    InvalidExponent,
    /// Scale overflowed `i32`.
    ScaleOverflow,
}

impl fmt::Display for NumericError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "number contains no digits",
            Self::InvalidDigit => "number contains an invalid digit",
            Self::InvalidExponent => "number has an invalid exponent",
            Self::ScaleOverflow => "number scale exceeds the supported range",
        })
    }
}

impl core::error::Error for NumericError {}

/// Exact finite base-ten: `coefficient * 10^-scale`, plus the authored spelling.
/// Derived equality compares spelling, not value (`1.50` stays distinct from
/// `1.5`). The coefficient is derived on demand, so parsing allocates nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decimal {
    /// Authored spelling, including trailing zeroes (`1.50`) and `-0`.
    spelling: CompactStr,
    scale: i32,
    negative: bool,
}

impl Decimal {
    /// Decimal scale: `digits * 10^-scale`.
    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.scale
    }

    /// Whether the authored value is signed negative (`-0` is negative).
    #[must_use]
    pub const fn negative(&self) -> bool {
        self.negative
    }

    /// Authored spelling, including trailing zeroes (`1.50`) and `-0`.
    #[must_use]
    pub fn spelling(&self) -> &str {
        self.spelling.as_str()
    }

    /// The signed coefficient digits with the point removed and leading zeroes
    /// stripped (`"0"` when the value is zero). Pair with [`Self::scale`] and
    /// [`Self::negative`] to reconstruct `(-1)^negative * coefficient * 10^-scale`.
    ///
    /// # Panics
    ///
    /// Never for a `Decimal` from [`Self::parse_owned`] or [`Number::parse`], a
    /// `Decimal` cannot be built any other way.
    #[must_use]
    pub fn coefficient(&self) -> (bool, String) {
        let text = self.spelling.as_str();
        let (mantissa, _) = split_exponent(text).expect("a Decimal spelling was validated at parse");
        let (negative, unsigned, _) = split_mantissa(mantissa).expect("a Decimal spelling was validated at parse");
        let mut digits = String::new();
        let mut started = false;
        for &byte in unsigned.as_bytes() {
            match byte {
                b'.' => {}
                b'0' if !started => {}
                _ => {
                    started = true;
                    digits.push(char::from(byte));
                }
            }
        }
        if !started {
            digits.push('0');
        }
        (negative, digits)
    }

    /// Parse a spelling the caller already owns.
    ///
    /// # Errors
    ///
    /// [`NumericError`] on an empty spelling, a malformed body, or an
    /// overflowing scale.
    pub fn parse_owned(spelling: CompactStr) -> Result<Self, NumericError> {
        let bytes = spelling.as_bytes();
        // One pass over the mantissa: its end (an exponent mark), the first
        // point, and whether it holds a digit at all. A malformed exponent is
        // reported before an empty mantissa, as the split order has always done.
        let negative = bytes.first() == Some(&b'-');
        let sign = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
        let mut i = sign;
        let mut point = None;
        let mut digits = 0usize;
        let mut end = bytes.len();
        let mut exp = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'0'..=b'9' => digits += 1,
                b'.' if point.is_none() => point = Some(i - sign),
                b'e' | b'E' => {
                    end = i;
                    let exponent = &spelling.as_str()[end + 1..];
                    if exponent.is_empty() {
                        return Err(NumericError::InvalidExponent);
                    }
                    exp = exponent.parse().map_err(|_| NumericError::InvalidExponent)?;
                    break;
                }
                // The integer arm refuses a non-digit body; the decimal arm
                // refuses a non-digit body past its marker the same way, so a
                // caller cannot smuggle bytes through a point or an exponent.
                _ => return Err(NumericError::InvalidDigit),
            }
            i += 1;
        }
        if end == sign || digits == 0 {
            return Err(NumericError::Empty);
        }
        let unsigned_len = end - sign;
        let point = point.unwrap_or(unsigned_len);
        let frac_digits = unsigned_len.saturating_sub(point + 1);
        let frac = i32::try_from(frac_digits).map_err(|_| NumericError::ScaleOverflow)?;
        let scale = frac.checked_sub(exp).ok_or(NumericError::ScaleOverflow)?;
        Ok(Self {
            spelling,
            scale,
            negative,
        })
    }
}

/// A non-finite number (`Infinity` / `NaN`), for a grammar that admits one. It
/// has no exact base-ten value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NonFinite {
    /// `Infinity`.
    Infinity,
    /// `-Infinity`.
    NegativeInfinity,
    /// `NaN`.
    NaN,
}

impl NonFinite {
    /// The value model's canonical name. A grammar may spell a non-finite number
    /// differently; recognition and encoding of such spellings belong to the codec.
    #[must_use]
    pub const fn spelling(self) -> &'static str {
        match self {
            Self::Infinity => "Infinity",
            Self::NegativeInfinity => "-Infinity",
            Self::NaN => "NaN",
        }
    }

    /// The IEEE-754 value this spelling stands for. Infinity is infinite and
    /// `NaN` is not a number, so this is the one lossy projection a non-finite
    /// value has; the spelling is canonical.
    #[must_use]
    pub const fn to_f64(self) -> f64 {
        match self {
            Self::Infinity => f64::INFINITY,
            Self::NegativeInfinity => f64::NEG_INFINITY,
            Self::NaN => f64::NAN,
        }
    }
}

/// Integer (`i128` or wide decimal digits), decimal, or a non-finite value, with
/// authored spelling. The numeric value is not cached; [`Number::to_i128`]
/// parses the spelling on demand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Number {
    /// Integer spelling (no `.` / `e` / `E`).
    Int {
        /// Authored spelling (`-0` stays signed).
        spelling: CompactStr,
    },
    /// Fraction or exponent, or an integer wider than `i128`.
    Decimal(Decimal),
    /// Non-finite `Infinity` / `NaN`, for a grammar that admits them.
    NonFinite(NonFinite),
}

impl Number {
    /// Parse a base-ten number spelling that already passed the lexer.
    ///
    /// # Errors
    ///
    /// [`NumericError`] on empty or overflowing scale.
    pub fn parse(spelling: &str) -> Result<Self, NumericError> {
        Self::parse_owned(CompactStr::from(spelling))
    }

    /// Parse a spelling the caller already owns.
    ///
    /// # Errors
    ///
    /// [`NumericError`] on an empty spelling, a malformed body, or an
    /// overflowing scale.
    pub fn parse_owned(spelling: CompactStr) -> Result<Self, NumericError> {
        let bytes = spelling.as_bytes();
        let sign = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
        if sign == bytes.len() {
            return Err(NumericError::Empty);
        }
        // The first non-digit past the sign decides the shape: a marker means a
        // decimal, anything else is a non-digit integer body.
        match bytes[sign..].iter().position(|b| !b.is_ascii_digit()) {
            None => Ok(Self::Int { spelling }),
            Some(offset) if matches!(bytes[sign + offset], b'.' | b'e' | b'E') => {
                Ok(Self::Decimal(Decimal::parse_owned(spelling)?))
            }
            Some(_) => Err(NumericError::InvalidDigit),
        }
    }

    /// `i64` when this is an integer that fits.
    #[must_use]
    pub fn to_i64(&self) -> Option<i64> {
        i64::try_from(self.to_i128()?).ok()
    }

    /// `u64` when this is a non-negative integer that fits.
    #[must_use]
    pub fn to_u64(&self) -> Option<u64> {
        u64::try_from(self.to_i128()?).ok()
    }

    /// Nearest `f64` for a finite spelling, or the non-finite value.
    ///
    /// **Lossy**: a decimal or a wide integer may round, and an out-of-range
    /// magnitude becomes infinite. [`Self::spelling`] is the exact value.
    #[must_use]
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Self::NonFinite(value) => Some(value.to_f64()),
            Self::Int { .. } | Self::Decimal(_) => self.spelling().parse().ok(),
        }
    }

    /// Authored spelling.
    #[must_use]
    pub fn spelling(&self) -> &str {
        match self {
            Self::Int { spelling } => spelling.as_str(),
            Self::Decimal(d) => d.spelling(),
            Self::NonFinite(value) => value.spelling(),
        }
    }

    /// Whether this is a non-finite number.
    #[must_use]
    pub const fn is_non_finite(&self) -> bool {
        matches!(self, Self::NonFinite(_))
    }

    /// `i128` when this is an integer that fits. Parses the spelling.
    #[must_use]
    pub fn to_i128(&self) -> Option<i128> {
        match self {
            Self::Int { spelling } => spelling.as_str().parse().ok(),
            Self::Decimal(_) | Self::NonFinite(_) => None,
        }
    }

    /// Mathematical equality: `1.50` equals `1.5`. Authored spelling may differ.
    #[must_use]
    pub fn numeric_eq(&self, other: &Self) -> bool {
        self.numeric_cmp(other) == Some(Ordering::Equal)
    }

    /// Mathematical compare. `None` when either side is non-finite.
    #[must_use]
    pub fn numeric_cmp(&self, other: &Self) -> Option<Ordering> {
        if self == other {
            return if self.is_non_finite() {
                None
            } else {
                Some(Ordering::Equal)
            };
        }
        self.numeric_cmp_different_spelling(other)
    }

    /// Mathematical compare of two numbers whose spellings are known to differ;
    /// a caller that already tested `==` skips re-testing it here.
    pub(crate) fn numeric_cmp_different_spelling(&self, other: &Self) -> Option<Ordering> {
        if self.is_non_finite() || other.is_non_finite() {
            return None;
        }
        if let (Self::Int { spelling: a }, Self::Int { spelling: b }) = (self, other) {
            return Some(cmp_int_spelling(a.as_bytes(), b.as_bytes()));
        }
        Parts::of(self)?.cmp(&Parts::of(other)?)
    }
}

/// A finite number normalized for comparison: `(-1)^negative * digits * 10^-scale`.
struct Parts<'a> {
    negative: bool,
    digits: Digits<'a>,
    scale: i32,
}

impl<'a> Parts<'a> {
    /// Normalize a finite number; `None` when it is non-finite.
    #[inline]
    fn of(number: &'a Number) -> Option<Self> {
        match number {
            Number::Int { spelling } => Some(Self {
                negative: spelling.as_bytes().first() == Some(&b'-'),
                digits: Digits::of_int(spelling.as_bytes()),
                scale: 0,
            }),
            Number::Decimal(decimal) => Some(Self {
                negative: decimal.negative(),
                digits: Digits::of(decimal.spelling.as_bytes()),
                scale: decimal.scale(),
            }),
            Number::NonFinite(_) => None,
        }
    }

    /// Order two finite numbers; signed zero keeps its sign (`-0` sorts below `0`).
    fn cmp(&self, other: &Parts<'_>) -> Option<Ordering> {
        match (self.negative, other.negative) {
            (true, false) => return Some(Ordering::Less),
            (false, true) => return Some(Ordering::Greater),
            _ => {}
        }
        let order = self.cmp_magnitude(other)?;
        Some(if self.negative { order.reverse() } else { order })
    }

    /// Order two finite magnitudes, ignoring their signs.
    fn cmp_magnitude(&self, other: &Parts<'_>) -> Option<Ordering> {
        let a = &self.digits;
        let b = &other.digits;
        match (a.is_zero(), b.is_zero()) {
            (true, true) => return Some(Ordering::Equal),
            (true, false) => return Some(Ordering::Less),
            (false, true) => return Some(Ordering::Greater),
            (false, false) => {}
        }
        // Same power of ten for the most significant digit decides the order;
        // otherwise the digit strings are compared, with a shorter one treated
        // as trailing zeroes.
        let a_exp = i64::try_from(a.len()).ok()? - 1 - i64::from(self.scale);
        let b_exp = i64::try_from(b.len()).ok()? - 1 - i64::from(other.scale);
        match a_exp.cmp(&b_exp) {
            Ordering::Equal => {}
            order => return Some(order),
        }
        let common = a.len().min(b.len());
        for i in 0..common {
            match a.byte(i).cmp(&b.byte(i)) {
                Ordering::Equal => {}
                order => return Some(order),
            }
        }
        if (common..a.len()).any(|i| a.byte(i) != b'0') {
            return Some(Ordering::Greater);
        }
        if (common..b.len()).any(|i| b.byte(i) != b'0') {
            return Some(Ordering::Less);
        }
        Some(Ordering::Equal)
    }
}

/// The significant digits of a base-ten spelling, in order. A single pass drops
/// the sign, the exponent, and the `.`, and skips leading zeroes, so a compare
/// never re-scans the spelling.
struct Digits<'a> {
    head: &'a [u8],
    tail: &'a [u8],
}

impl<'a> Digits<'a> {
    #[inline]
    fn of(bytes: &'a [u8]) -> Self {
        let mut i = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
        let mut point = None;
        let mut first = None;
        let mut end = bytes.len();
        while i < bytes.len() {
            match bytes[i] {
                b'e' | b'E' => {
                    end = i;
                    break;
                }
                b'.' => point = Some(i),
                b'1'..=b'9' if first.is_none() => first = Some(i),
                _ => {}
            }
            i += 1;
        }
        let Some(start) = first else {
            return Self { head: &[], tail: &[] };
        };
        match point {
            Some(p) if p > start && p < end => Self {
                head: &bytes[start..p],
                tail: &bytes[p + 1..end],
            },
            _ => Self {
                head: &bytes[start..end],
                tail: &[],
            },
        }
    }

    /// Digits of a plain integer spelling: the sign is dropped and leading
    /// zeroes skipped, with no marker scan — an [`Number::Int`] spelling has no
    /// fraction or exponent by construction.
    #[inline]
    fn of_int(bytes: &'a [u8]) -> Self {
        let unsigned = match bytes.first() {
            Some(b'-' | b'+') => &bytes[1..],
            _ => bytes,
        };
        let start = unsigned.iter().position(|b| *b != b'0').unwrap_or(unsigned.len());
        Self {
            head: &unsigned[start..],
            tail: &[],
        }
    }

    fn len(&self) -> usize {
        self.head.len() + self.tail.len()
    }

    fn is_zero(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    fn byte(&self, index: usize) -> u8 {
        if index < self.head.len() {
            self.head[index]
        } else {
            self.tail[index - self.head.len()]
        }
    }
}

/// Order two integer spellings by sign then magnitude, allocation-free. Integers
/// are by far the common predicate operand, so they take a direct
/// length-then-`memcmp` path instead of the general digit walk.
fn cmp_int_spelling(a: &[u8], b: &[u8]) -> Ordering {
    let (a_neg, b_neg) = (a.first() == Some(&b'-'), b.first() == Some(&b'-'));
    match (a_neg, b_neg) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let order = cmp_int_digits(int_digits(a), int_digits(b));
    if a_neg { order.reverse() } else { order }
}

/// The unsigned integer spelling with leading zeroes dropped (`"0"` for zero).
fn int_digits(spelling: &[u8]) -> &[u8] {
    let unsigned = match spelling.first() {
        Some(b'-' | b'+') => &spelling[1..],
        _ => spelling,
    };
    let digits = unsigned
        .iter()
        .position(|b| *b != b'0')
        .map_or(&[][..], |start| &unsigned[start..]);
    if digits.is_empty() { b"0" } else { digits }
}

/// Order two leading-zero-free integer digit strings.
fn cmp_int_digits(a: &[u8], b: &[u8]) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn split_exponent(spelling: &str) -> Result<(&str, i32), NumericError> {
    let Some((mantissa, exp)) = spelling.split_once(['e', 'E']) else {
        return Ok((spelling, 0));
    };
    if exp.is_empty() {
        return Err(NumericError::InvalidExponent);
    }
    let exp: i32 = exp.parse().map_err(|_| NumericError::InvalidExponent)?;
    Ok((mantissa, exp))
}

fn split_mantissa(mantissa: &str) -> Result<(bool, &str, usize), NumericError> {
    let first = mantissa.as_bytes().first().copied();
    let negative = first == Some(b'-');
    // A leading sign is one ASCII byte, so `1..` is a char boundary.
    let unsigned = if matches!(first, Some(b'-' | b'+')) {
        &mantissa[1..]
    } else {
        mantissa
    };
    if unsigned.is_empty() {
        return Err(NumericError::Empty);
    }
    let point = unsigned
        .as_bytes()
        .iter()
        .position(|b| *b == b'.')
        .unwrap_or(unsigned.len());
    Ok((negative, unsigned, point))
}
