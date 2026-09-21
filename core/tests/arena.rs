//! The shared arena: node layout, navigation, and detached subtrees.

use structury::{BorrowedDocument, Node, OwnedDocument, Value, ValueKind};

/// One `Node` is 12 bytes; a field added to any variant trips this at compile time.
const _: () = assert!(core::mem::size_of::<Node>() == 12);

#[test]
fn refused_detach_is_a_null_document() {
    let empty = OwnedDocument::from_parts(Vec::new(), Vec::new(), String::new(), 0);
    assert_eq!(empty.to_value(), Value::Null);
}

#[test]
fn detach_preserves_value() {
    let source = b"{\"a\":[\"b\",7]}";
    let document = BorrowedDocument::from_parts(
        source,
        vec![
            Node::Object { edge: 0, len: 2 },
            Node::Str { off: 2, len: 1 },
            Node::Array { edge: 2, len: 2 },
            Node::Str { off: 7, len: 1 },
            Node::Number { off: 10, len: 1 },
        ],
        vec![1, 2, 3, 4],
        String::new(),
        0,
    );
    let detached = document.root().detach();
    assert_eq!(detached.to_value(), document.to_value());
}

#[test]
fn member_is_last_wins_over_duplicate_keys() {
    // `{"a":1,"a":2}` hand-built so the object keeps both members; the codec's
    // arena builder collapses duplicates, but `from_parts` does not.
    let source = b"{\"a\":1,\"a\":2}";
    let document = BorrowedDocument::from_parts(
        source,
        vec![
            Node::Object { edge: 0, len: 2 },
            Node::Str { off: 2, len: 1 },
            Node::Number { off: 5, len: 1 },
            Node::Str { off: 8, len: 1 },
            Node::Number { off: 11, len: 1 },
        ],
        vec![1, 2, 3, 4],
        String::new(),
        0,
    );
    assert_eq!(
        document.root().member("a").and_then(structury::ArenaValue::to_i64),
        Some(2)
    );
    assert_eq!(document.root().members().count(), 2);
}

#[test]
fn a_cyclic_arena_reads_as_null_instead_of_aborting() {
    let document =
        BorrowedDocument::from_parts(b"[]", vec![Node::Array { edge: 0, len: 1 }], vec![0], String::new(), 0);
    assert_eq!(document.to_value(), Value::Array(vec![Value::Null]));
    assert_eq!(
        document.detach().to_value(),
        Value::Null,
        "a cycle is refused, not copied"
    );
}

#[test]
fn an_out_of_range_edge_reads_as_null_not_the_first_node() {
    let document = BorrowedDocument::from_parts(
        b"[1]",
        vec![Node::Array { edge: 5, len: 2 }, Node::Bool(true)],
        Vec::new(),
        String::new(),
        0,
    );
    assert_eq!(document.to_value(), Value::Array(vec![Value::Null, Value::Null]));
    assert_eq!(
        document.root().element(0).map(structury::ArenaValue::kind),
        Some(ValueKind::Null)
    );
}
