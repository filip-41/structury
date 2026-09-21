//! The source-backed document: zero-copy navigation and payload spill.

mod common;

use common::assert_navigates;
use structury::{BorrowedDocument, Node, NonFinite, Number, Value, ValueKind};

#[test]
fn view_navigates_without_node_ids() {
    let source = b"{\"a\":[\"b\",7]}";
    let document = BorrowedDocument::from_parts(
        source,
        vec![
            Node::Object { edge: 0, len: 2 },
            Node::Str { off: 2, len: 1 },
            Node::Array { edge: 2, len: 2 },
            Node::Str { off: 7, len: 1 },
            Node::Number { off: 10, len: 1 },
            Node::Bool(true),
        ],
        vec![1, 2, 3, 4],
        String::new(),
        0,
    );
    assert_navigates(document.root());
    assert_eq!(document.to_value(), document.root().to_value());
    assert_eq!(document.to_owned().to_value(), document.to_value());
}

#[test]
fn spill_payloads_read_after_the_source() {
    let source = b"[1,\"x\"]";
    let document = BorrowedDocument::from_parts(
        source,
        vec![
            Node::Array { edge: 0, len: 2 },
            Node::Number { off: 1, len: 1 },
            // Offset `source.len()` is the first spill byte.
            Node::Str { off: 7, len: 3 },
        ],
        vec![1, 2],
        String::from("caf"),
        0,
    );
    assert_eq!(document.root().element(1).expect("second").str(), "caf");
    assert_eq!(document.root().element(0).expect("first").number(), "1");
    assert_eq!(document.source(), source);
    assert_eq!(document.spilled_bytes(), 3);
    assert_eq!(document.to_owned().root().element(1).expect("second").str(), "caf");
}

#[test]
fn detach_whole_equals_owned_twin() {
    let source = b"{\"a\":[\"b\",7],\"c\":\"x\"}";
    // Builder layout: node ids are pre-order, edge blocks post-order (the
    // array's children land before the object's members).
    let document = BorrowedDocument::from_parts(
        source,
        vec![
            Node::Object { edge: 2, len: 2 },
            Node::Str { off: 2, len: 1 },
            Node::Array { edge: 0, len: 2 },
            Node::Str { off: 7, len: 1 },
            Node::Number { off: 10, len: 1 },
            Node::Str { off: 14, len: 1 },
            Node::Str { off: 17, len: 1 },
        ],
        vec![3, 4, 1, 2, 5, 6],
        String::new(),
        0,
    );
    let detached = document.detach();
    assert_eq!(detached.to_value(), document.to_owned().to_value());
    assert_eq!(detached, document.to_owned());
}

#[test]
fn an_empty_document_is_a_null_root() {
    let document = BorrowedDocument::from_parts(b"", Vec::new(), Vec::new(), String::new(), 0);
    assert_eq!(document.root().kind(), ValueKind::Null);
    assert_eq!(document.root().len(), 0);
    assert!(document.root().is_empty());
    assert_eq!(document.to_value(), Value::Null);
    assert_eq!(document.to_owned().to_value(), Value::Null);
    assert!(document.root().member("a").is_none());
    assert!(document.root().element(0).is_none());
}

#[test]
fn non_finite_round_trips() {
    let document = BorrowedDocument::from_parts(
        b"Infinity",
        vec![Node::NonFinite {
            off: 0,
            len: 8,
            value: NonFinite::Infinity,
        }],
        Vec::new(),
        String::new(),
        0,
    );
    let want = Value::Number(Number::NonFinite(NonFinite::Infinity));
    assert_eq!(document.to_value(), want);
    assert_eq!(document.to_owned().to_value(), want);
    assert_eq!(document.root().detach().to_value(), want);
}

#[test]
fn detach_of_a_non_root_subtree_copies_only_it() {
    // `[["x"],7]`
    let source = b"[[\"x\"],7]";
    let document = BorrowedDocument::from_parts(
        source,
        vec![
            Node::Array { edge: 0, len: 2 },
            Node::Array { edge: 2, len: 1 },
            Node::Str { off: 3, len: 1 },
            Node::Number { off: 7, len: 1 },
        ],
        vec![1, 3, 2],
        String::new(),
        0,
    );
    let first = document.root().element(0).expect("first");
    assert_eq!(first.detach().to_value(), Value::Array(vec![Value::Str("x".into())]));
}

#[test]
fn a_straddling_payload_reads_empty_and_is_refused() {
    // Logical buffer `abcZ`; bytes 2..4 cross the source/spill boundary.
    let document = BorrowedDocument::from_parts(
        b"abc",
        vec![Node::Str { off: 2, len: 2 }],
        Vec::new(),
        String::from("Z"),
        0,
    );
    assert_eq!(document.root().str(), "", "a straddle cannot be sliced contiguously");
    assert_eq!(document.to_owned().to_value(), Value::Null, "to_owned refuses it");
    assert_eq!(document.root().detach().to_value(), Value::Null, "detach refuses it");
}

#[test]
fn a_malformed_payload_is_refused_not_emptied() {
    let document = BorrowedDocument::from_parts(
        b"abc",
        vec![Node::Str { off: 99, len: 1 }],
        Vec::new(),
        String::new(),
        0,
    );
    assert_eq!(document.root().str(), "", "the read stays safe");
    assert_eq!(document.to_owned().to_value(), Value::Null, "to_owned refuses it");
    assert_eq!(document.root().detach().to_value(), Value::Null, "detach refuses it");
}

#[test]
fn a_null_document_has_one_canonical_shape() {
    let null = BorrowedDocument::from_parts(b"null", vec![Node::Null], Vec::new(), String::new(), 0);
    assert_eq!(null.to_value(), Value::Null);
    assert_eq!(null.to_owned(), null.detach());
}
