//! The owned arena document: construction, navigation, and payloads.

mod common;

use common::assert_navigates;
use structury::{Node, NonFinite, Number, OwnedDocument, Value, ValueKind};

#[test]
fn view_navigates_without_node_ids() {
    let document = OwnedDocument::from_parts(
        vec![
            Node::Object { edge: 0, len: 2 },
            Node::Str { off: 0, len: 1 },
            Node::Array { edge: 2, len: 2 },
            Node::Str { off: 1, len: 1 },
            Node::Number { off: 2, len: 1 },
            Node::Bool(true),
        ],
        vec![1, 2, 3, 4],
        String::from("ab7"),
        0,
    );
    assert_navigates(document.root());
    assert_eq!(document.to_value(), document.root().to_value());
}

#[test]
fn str_is_empty_for_number_nodes() {
    let document = OwnedDocument::from_parts(
        vec![
            Node::Array { edge: 0, len: 2 },
            Node::Number { off: 0, len: 2 },
            Node::Str { off: 2, len: 3 },
        ],
        vec![1, 2],
        String::from("42foo"),
        0,
    );
    let number = document.root().element(0).expect("number");
    assert_eq!(number.str(), "");
    assert_eq!(number.number(), "42");
    let text = document.root().element(1).expect("string");
    assert_eq!(text.str(), "foo");
}

#[test]
fn an_empty_document_is_a_null_root() {
    let document = OwnedDocument::from_parts(Vec::new(), Vec::new(), String::new(), 0);
    assert_eq!(document.root().kind(), ValueKind::Null);
    assert_eq!(document.root().len(), 0);
    assert!(document.root().is_empty());
    assert_eq!(document.to_value(), Value::Null);
    assert!(document.root().member("a").is_none());
    assert!(document.root().element(0).is_none());
    assert_eq!(document.root().detach().to_value(), Value::Null);
}

#[test]
fn non_finite_round_trips() {
    let document = OwnedDocument::from_parts(
        vec![Node::NonFinite {
            off: 0,
            len: 8,
            value: NonFinite::Infinity,
        }],
        Vec::new(),
        String::from("Infinity"),
        0,
    );
    let want = Value::Number(Number::NonFinite(NonFinite::Infinity));
    assert_eq!(document.to_value(), want);
    assert_eq!(document.detach().to_value(), want);
    assert_eq!(document.root().number(), "Infinity");
}
