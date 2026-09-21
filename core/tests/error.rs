//! The error surface: classes, offsets, codes, and the Issue channel.

use structury::{Error, ErrorClass, Issue};

#[test]
fn error_keeps_its_class_code_message_and_offset() {
    let error = Error::new(ErrorClass::Shape, "shape-mismatch", "not a row", 7);
    assert_eq!(error.class(), ErrorClass::Shape);
    assert_eq!(error.code(), "shape-mismatch");
    assert_eq!(error.message(), "not a row");
    assert_eq!(error.offset(), 7);
    assert_eq!(error.to_string(), "shape: not a row at byte 7 (shape-mismatch)");
}

#[test]
fn class_names_are_stable_and_distinct() {
    let classes = [
        (ErrorClass::Syntax, "syntax"),
        (ErrorClass::Number, "number"),
        (ErrorClass::Utf8, "utf8"),
        (ErrorClass::Escape, "escape"),
        (ErrorClass::Shape, "shape"),
        (ErrorClass::Limit, "limit"),
        (ErrorClass::Write, "write"),
        (ErrorClass::Control, "control"),
    ];
    let mut names: Vec<&str> = classes.iter().map(|(class, _)| class.as_str()).collect();
    for (class, name) in classes {
        assert_eq!(class.as_str(), name);
    }
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), classes.len(), "class names stay distinct");
}

#[test]
fn offset_saturates_at_u32_max() {
    let error = Error::new(ErrorClass::Limit, "deep", "too deep", usize::MAX);
    assert_eq!(error.offset(), u32::MAX);
}

#[test]
fn an_issue_carries_the_error_fields() {
    let error = Error::new(ErrorClass::Syntax, "trailing", "trailing content", 3);
    let issue = Issue::from(error.clone());
    assert_eq!(issue.class, error.class());
    assert_eq!(issue.offset, error.offset());
    assert_eq!(issue.code, error.code());
    assert_eq!(issue.message, error.message());
}
