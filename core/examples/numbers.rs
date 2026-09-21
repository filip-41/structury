//! Exact number spellings: parse, compare by value, and project.

use core::cmp::Ordering;

use structury::{NonFinite, Number};

/// One exact number from its authored spelling.
fn number(spelling: &str) -> Number {
    Number::parse(spelling).expect("a base-ten spelling")
}

fn main() {
    let authored = number("2.50");
    assert_eq!(authored.spelling(), "2.50");
    assert!(authored.numeric_eq(&number("2.5")), "one value, two spellings");
    assert!(authored != number("2.5"), "`==` keeps the spelling");
    assert_eq!(authored.to_f64(), Some(2.5));

    // Signed zero keeps its sign: `-0` sorts below `0` and is not numerically
    // equal to it, yet both project to the integer zero.
    let negative_zero = number("-0");
    assert_eq!(negative_zero.spelling(), "-0");
    assert_eq!(negative_zero.to_i64(), Some(0));
    assert!(!negative_zero.numeric_eq(&number("0")));
    assert_eq!(negative_zero.numeric_cmp(&number("0")), Some(Ordering::Less));

    // Ordering is numeric across the integer and decimal shapes. Projection to
    // a machine integer needs an integer spelling, so `1e1` compares as a
    // decimal but does not project.
    let ten = number("1e1");
    assert_eq!(ten.numeric_cmp(&number("9")), Some(Ordering::Greater));
    assert_eq!(ten.to_i64(), None);
    assert_eq!(number("10").to_i64(), Some(10));

    // A grammar that admits non-finite numbers constructs the value directly.
    // Spelling recognition and encoding belong to the codec.
    let infinity = Number::NonFinite(NonFinite::Infinity);
    assert!(infinity.is_non_finite());
    assert_eq!(infinity.spelling(), "Infinity");
    assert_eq!(infinity.to_f64(), Some(f64::INFINITY));
}
