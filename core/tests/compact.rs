//! `CompactStr`: inline storage, the heap boundary, and `str` behavior.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use structury::CompactStr;

/// `CompactStr` stays exactly `size_of::<String>()`, so `Value::Str` is not a regression.
const _: () = assert!(core::mem::size_of::<CompactStr>() == core::mem::size_of::<String>());

fn hash_of<T: Hash + ?Sized>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn boundary_lengths_roundtrip() {
    for n in [0usize, 1, 21, 22, 23, 200] {
        let s = "a".repeat(n);
        let c = CompactStr::from(s.as_str());
        assert_eq!(c.len(), n);
        assert_eq!(c.as_str(), s);
        assert_eq!(c.as_bytes(), s.as_bytes());
    }
}

#[test]
fn equality_order_and_hash_follow_str() {
    let mut v = [CompactStr::from("b"), CompactStr::from("aa"), CompactStr::from("a")];
    v.sort();
    assert_eq!(
        v.iter().map(CompactStr::as_str).collect::<Vec<_>>(),
        vec!["a", "aa", "b"]
    );
    assert_eq!(CompactStr::from("k00"), "k00");
    assert_ne!(CompactStr::from("k0"), "k00");
}

#[test]
fn hash_borrow_deref_and_display_follow_str() {
    let edge = "x".repeat(22);
    let heap = "y".repeat(200);
    for text in ["", "a", edge.as_str(), heap.as_str()] {
        let compact = CompactStr::from(text);
        assert_eq!(hash_of(&compact), hash_of(text), "hash agrees for {text:?}");
        assert_eq!(compact.to_string(), text, "Display agrees for {text:?}");
        assert_eq!(&*compact, text, "Deref agrees for {text:?}");
        assert_eq!(compact.as_ref(), text, "AsRef agrees for {text:?}");

        let mut map = HashMap::new();
        map.insert(compact, 7_u8);
        assert_eq!(map.get(text), Some(&7), "Borrow<str> lookup for {text:?}");
    }
}

#[test]
fn owned_and_borrowed_constructors_agree() {
    let heap = "z".repeat(23);
    for text in ["", "short", heap.as_str()] {
        assert_eq!(CompactStr::from(text), CompactStr::from(String::from(text)));
        assert_eq!(CompactStr::from(text).as_str(), text);
    }
}
