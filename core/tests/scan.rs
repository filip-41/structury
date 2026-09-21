//! `ByteRange`, `ScanResult`, and the answer vocabulary.

use structury::{Answer, ByteRange, OracleAnswer, ScanResult, ValueKind};

#[test]
fn byte_range_rejects_inverted_bounds() {
    assert!(ByteRange::try_new(3, 1).is_none());
    let range = ByteRange::try_new(1, 3).expect("ordered");
    assert_eq!(range.start(), 1);
    assert_eq!(range.end(), 3);
    assert_eq!(range.len(), 2);
    assert!(!range.is_empty());
}

#[test]
fn empty_range_is_allowed() {
    let range = ByteRange::try_new(4, 4).expect("empty");
    assert!(range.is_empty());
    assert_eq!(range.len(), 0);
}

#[test]
fn byte_range_orders_by_start_then_end() {
    let a = ByteRange::try_new(1, 2).expect("a");
    let b = ByteRange::try_new(1, 3).expect("b");
    let c = ByteRange::try_new(2, 2).expect("c");
    assert!(a < b && b < c);
}

#[test]
fn scan_result_new_keeps_its_answers() {
    let result = ScanResult::new(vec![Answer::Missing], Vec::new());
    assert_eq!(result.answers.len(), 1);
    assert!(result.issues.is_empty());
    assert!(matches!(result.answers[0], Answer::Missing));
}

#[test]
fn oracle_answers_compare_by_value() {
    assert_eq!(OracleAnswer::Count(3), OracleAnswer::Count(3));
    assert_ne!(OracleAnswer::Count(3), OracleAnswer::Count(4));
    assert_eq!(
        OracleAnswer::Kind(ValueKind::Array),
        OracleAnswer::Kind(ValueKind::Array)
    );
    assert_ne!(
        OracleAnswer::Kind(ValueKind::Array),
        OracleAnswer::Kind(ValueKind::Object)
    );
    assert_eq!(OracleAnswer::HasKey(true), OracleAnswer::HasKey(true));
    assert_ne!(OracleAnswer::HasKey(true), OracleAnswer::HasKey(false));
    assert_eq!(
        OracleAnswer::MemberNames(vec![String::from("a"), String::from("b")]),
        OracleAnswer::MemberNames(vec![String::from("a"), String::from("b")])
    );
    assert_ne!(OracleAnswer::StringByteLength(3), OracleAnswer::StringByteLength(4));
}

#[test]
fn type_mismatch_carries_the_found_kind() {
    let answer = Answer::TypeMismatch {
        actual: ValueKind::Array,
    };
    assert!(matches!(
        answer,
        Answer::TypeMismatch {
            actual: ValueKind::Array
        }
    ));
}
