//! Exact number spellings: parsing, comparison, and projection.

use core::cmp::Ordering;

use structury::{NonFinite, Number, NumericError};

#[test]
fn numeric_eq_aligns_scale() {
    let a = Number::parse("1.50").expect("1.50");
    let b = Number::parse("1.5").expect("1.5");
    assert!(a.numeric_eq(&b), "1.50 equals 1.5 by value");
    let c = Number::parse("100").expect("100");
    let d = Number::parse("1e2").expect("1e2");
    assert!(c.numeric_eq(&d), "100 equals 1e2 by value");
    assert!(!a.numeric_eq(&c));
    let zero = Number::parse("0").expect("0");
    let half = Number::parse("0.5").expect("0.5");
    let zero_point = Number::parse("0.0").expect("0.0");
    assert_eq!(zero.numeric_cmp(&half), Some(core::cmp::Ordering::Less));
    assert_eq!(half.numeric_cmp(&zero), Some(core::cmp::Ordering::Greater));
    assert!(zero.numeric_eq(&zero_point), "0 equals 0.0 by value");
    let neg_zero = Number::parse("-0").expect("-0");
    assert_eq!(neg_zero.numeric_cmp(&zero), Some(core::cmp::Ordering::Less));
    assert_eq!(zero.numeric_cmp(&neg_zero), Some(core::cmp::Ordering::Greater));
}

#[test]
fn signed_decimals_compare_by_value_and_preserve_spelling() {
    for (spelling, equivalent) in [
        ("+1.50", "1.5"),
        ("+0.5", "0.5"),
        ("+.5", "0.5"),
        ("+1.", "1"),
        ("+1e2", "100"),
        ("+0.0", "0"),
        ("-1.50", "-1.5"),
        ("-0.0", "-0"),
    ] {
        let signed = Number::parse(spelling).expect("signed decimal");
        let expected = Number::parse(equivalent).expect("equivalent value");
        assert_eq!(signed.spelling(), spelling);
        assert_eq!(
            signed.numeric_cmp(&expected),
            Some(core::cmp::Ordering::Equal),
            "{spelling}"
        );
        assert_eq!(
            expected.numeric_cmp(&signed),
            Some(core::cmp::Ordering::Equal),
            "{spelling}"
        );
        assert!(signed.numeric_eq(&expected), "{spelling}");
    }
    let half = Number::parse("+0.5").expect("half");
    let one = Number::parse("1").expect("one");
    assert_eq!(half.numeric_cmp(&one), Some(core::cmp::Ordering::Less));
    assert_eq!(one.numeric_cmp(&half), Some(core::cmp::Ordering::Greater));
}

#[test]
fn wide_integers_stay_exact_and_do_not_fit_i128() {
    let wide_text = "1234567890123456789012345678901234567890";
    let wide = Number::parse(wide_text).expect("wide");
    assert_eq!(wide.to_i128(), None);
    assert_eq!(wide.spelling(), wide_text);
    let in_range = Number::parse("-170141183460469231731687303715884105727").expect("i128 magnitude");
    assert_eq!(in_range.to_i128(), Some(i128::MIN + 1));
    let neg_zero = Number::parse("-0").expect("-0");
    assert_eq!(neg_zero.to_i128(), Some(0));
    assert!(neg_zero.spelling().starts_with('-'));
}

#[test]
fn non_finite_has_a_canonical_name_and_f64() {
    let infinity = Number::NonFinite(NonFinite::Infinity);
    assert!(infinity.is_non_finite());
    assert_eq!(infinity.spelling(), "Infinity");
    assert_eq!(Number::NonFinite(NonFinite::NegativeInfinity).spelling(), "-Infinity");
    assert_eq!(Number::NonFinite(NonFinite::NaN).spelling(), "NaN");
    assert_eq!(infinity.to_f64(), Some(f64::INFINITY));
    assert_eq!(
        Number::NonFinite(NonFinite::NegativeInfinity).to_f64(),
        Some(f64::NEG_INFINITY)
    );
    assert_eq!(Number::NonFinite(NonFinite::NaN).to_f64().map(f64::is_nan), Some(true));
    assert_eq!(infinity.numeric_cmp(&infinity), None);
}

#[test]
fn parse_is_base_ten_only() {
    for spelling in ["Infinity", "-Infinity", "+Infinity", "NaN", "-NaN", "inf"] {
        assert_eq!(Number::parse(spelling), Err(NumericError::InvalidDigit), "{spelling}");
    }
}

#[test]
fn parse_rejects_a_non_digit_integer_body() {
    assert_eq!(Number::parse("x"), Err(NumericError::InvalidDigit));
    assert_eq!(Number::parse("12abc"), Err(NumericError::InvalidDigit));
    assert_eq!(Number::parse("+"), Err(NumericError::Empty));
    assert_eq!(Number::parse("-"), Err(NumericError::Empty));
    assert!(Number::parse("007").is_ok(), "leading zeroes stay a grammar concern");
    assert!(Number::parse("+12").is_ok(), "a JSON5 sign is a grammar concern");
    assert!(Number::parse("-0").is_ok());
}

#[test]
fn lossy_accessors_are_explicit() {
    assert_eq!(Number::parse("42").expect("int").to_u64(), Some(42));
    assert_eq!(Number::parse("-1").expect("int").to_u64(), None);
    assert_eq!(Number::parse("1.5").expect("decimal").to_f64(), Some(1.5));
    assert_eq!(Number::parse("1e400").expect("wide").to_f64(), Some(f64::INFINITY));
    assert_eq!(Number::NonFinite(NonFinite::NaN).to_f64().map(f64::is_nan), Some(true));
    let Number::Decimal(decimal) = Number::parse("1.50").expect("decimal") else {
        panic!("expected a decimal");
    };
    assert_eq!(decimal.scale(), 2);
    assert!(!decimal.negative());
    assert_eq!(decimal.coefficient(), (false, String::from("150")));
}

#[test]
fn numeric_cmp_orders_across_int_and_decimal() {
    let order = |a: &str, b: &str| Number::parse(a).expect(a).numeric_cmp(&Number::parse(b).expect(b));
    for (a, b, expected) in [
        ("2", "10", Ordering::Less),
        ("10", "9", Ordering::Greater),
        ("2", "2.5", Ordering::Less),
        ("2.0", "2", Ordering::Equal),
        ("1e2", "99", Ordering::Greater),
        ("100", "1e2", Ordering::Equal),
        ("-2", "-10", Ordering::Greater),
        ("0.09", "0.1", Ordering::Less),
    ] {
        assert_eq!(order(a, b), Some(expected), "{a} vs {b}");
    }
}

#[test]
fn coefficient_strips_the_point_and_leading_zeroes() {
    for (spelling, scale, negative, digits) in [
        ("1.50", 2, false, "150"),
        ("1.00", 2, false, "100"),
        ("0.05", 2, false, "5"),
        ("-0.5", 1, true, "5"),
        ("0.0", 1, false, "0"),
    ] {
        let Number::Decimal(decimal) = Number::parse(spelling).expect(spelling) else {
            panic!("{spelling} is a decimal");
        };
        assert_eq!(decimal.scale(), scale, "{spelling}");
        assert_eq!(decimal.negative(), negative, "{spelling}");
        assert_eq!(decimal.coefficient(), (negative, String::from(digits)), "{spelling}");
    }
}

#[test]
fn integer_accessor_boundaries() {
    assert_eq!(
        Number::parse("9223372036854775807").expect("i64 max").to_i64(),
        Some(i64::MAX)
    );
    assert_eq!(
        Number::parse("-9223372036854775808").expect("i64 min").to_i64(),
        Some(i64::MIN)
    );
    assert_eq!(
        Number::parse("18446744073709551615").expect("u64 max").to_u64(),
        Some(u64::MAX)
    );
    assert_eq!(
        Number::parse("18446744073709551616").expect("u64 max + 1").to_u64(),
        None
    );
    assert_eq!(
        Number::parse("170141183460469231731687303715884105727")
            .expect("i128 max")
            .to_i64(),
        None
    );
    assert_eq!(Number::parse("1.5").expect("decimal").to_i64(), None);
}

#[test]
fn parse_rejects_empty_and_bad_exponents() {
    assert_eq!(Number::parse(""), Err(NumericError::Empty));
    assert_eq!(Number::parse("1e"), Err(NumericError::InvalidExponent));
    assert_eq!(Number::parse("1e+"), Err(NumericError::InvalidExponent));
    assert_eq!(Number::parse("1ex"), Err(NumericError::InvalidExponent));
}

#[test]
fn numeric_cmp_is_none_with_a_non_finite_operand() {
    let one = Number::parse("1").expect("1");
    for non_finite in [NonFinite::Infinity, NonFinite::NegativeInfinity, NonFinite::NaN] {
        let value = Number::NonFinite(non_finite);
        assert_eq!(one.numeric_cmp(&value), None, "{non_finite:?}");
        assert_eq!(value.numeric_cmp(&one), None, "{non_finite:?}");
        assert_eq!(value.numeric_cmp(&value), None, "{non_finite:?}");
        assert!(!one.numeric_eq(&value));
    }
}

#[test]
fn parse_shapes_and_errors_are_pinned_for_odd_spellings() {
    for spelling in ["0", "-0", "+0", "00", "1", "-1", "+1", "12345"] {
        let number = Number::parse(spelling).expect("integer spelling");
        assert!(matches!(number, Number::Int { .. }), "{spelling}");
        assert_eq!(number.spelling(), spelling);
    }

    for (spelling, scale) in [
        ("12.99", 2),
        ("+1.", 0),
        (".5", 1),
        ("-.5", 1),
        ("+0.50", 2),
        ("00.50", 2),
        ("1e2", -2),
        ("1E2", -2),
        ("1e+2", -2),
        ("1e-2", 2),
        ("1.5e-3", 4),
        ("1.7976931348623157e308", -292),
    ] {
        let number = Number::parse(spelling).expect(spelling);
        let Number::Decimal(decimal) = &number else {
            panic!("{spelling} should be a decimal");
        };
        assert_eq!(decimal.scale(), scale, "{spelling}");
        assert_eq!(number.spelling(), spelling, "the spelling is kept");
    }

    for spelling in [
        "", "+", "-", ".", "e2", "1e", "x", "12abc", "1_000", "0x10", "Infinity", "1.5x", "1.2.3", "1e2x",
    ] {
        assert!(Number::parse(spelling).is_err(), "{spelling} must refuse");
    }
}
