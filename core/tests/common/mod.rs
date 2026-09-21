//! Helpers shared by the core integration tests.
#![allow(dead_code, unused_imports)]

use structury::{Arena, ArenaValue, ByteRange, ValueKind};

/// Shared navigation assertions over a constructed document's root; the owned
/// and borrowed documents differ only in construction and their extra twin
/// checks.
pub(crate) fn assert_navigates<A>(root: ArenaValue<'_, A>)
where
    A: Arena + core::fmt::Debug + PartialEq,
{
    assert_eq!(root.kind(), ValueKind::Object);
    assert_eq!(root.len(), 2);
    let array = root.member("a").expect("a");
    assert_eq!(array.kind(), ValueKind::Array);
    assert_eq!(array.len(), 2);
    assert_eq!(array.element(0).expect("first").str(), "b");
    assert_eq!(array.element(1).expect("second").to_i64(), Some(7));
    assert_eq!(array.element(2), None);
    assert_eq!(root.member("missing"), None);
    assert_eq!(root.member_values().count(), 2);
    assert!(root.as_bytes().is_empty(), "an object has no payload");
    assert!(!array.is_empty());
    let names: Vec<&str> = root.members().map(|(name, _)| name).collect();
    assert_eq!(names, vec!["a", "b"]);
    let elements: Vec<ValueKind> = array.elements().map(ArenaValue::kind).collect();
    assert_eq!(elements, vec![ValueKind::String, ValueKind::Number]);
}

/// A half-open range, panicking when the caller inverts it.
pub(crate) fn range(start: usize, end: usize) -> ByteRange {
    ByteRange::try_new(start, end).expect("ordered")
}
