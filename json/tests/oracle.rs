//! Oracle matrix: every oracle against array/object/string/scalar, a missing
//! path, and oracles nested under a `Collection`.

mod common;

use crate::common::*;
use structury::{Answer, Demand, Oracle, OracleAnswer, Step, ValueKind};
use structury_json::{Dialect, JsonInput};

fn answer<'a>(src: &'a [u8], demand: &Demand) -> Answer<'a> {
    one(src, JsonInput::Text, demand, Dialect::Rfc8259)
}

fn oracle<'a>(answer: &'a Answer<'_>) -> &'a OracleAnswer {
    match answer {
        Answer::Oracle(oracle) => oracle,
        other => panic!("expected an oracle answer, got {other:?}"),
    }
}

#[test]
fn count_is_containers_only() {
    assert_eq!(
        oracle(&answer(b"[1,2,3]", &Demand::Oracle(Oracle::Count))),
        &OracleAnswer::Count(3)
    );
    assert_eq!(
        oracle(&answer(br#"{"a":1,"b":2}"#, &Demand::Oracle(Oracle::Count))),
        &OracleAnswer::Count(2)
    );
    assert_eq!(
        oracle(&answer(b"[]", &Demand::Oracle(Oracle::Count))),
        &OracleAnswer::Count(0)
    );
    // A scalar has no child or member count.
    for src in [&b"\"abc\""[..], b"42", b"true", b"null"] {
        assert!(
            matches!(answer(src, &Demand::Oracle(Oracle::Count)), Answer::TypeMismatch { .. }),
            "{src:?}"
        );
    }
}

#[test]
fn count_reports_the_mismatched_kind() {
    for (src, kind) in [
        (&b"\"abc\""[..], ValueKind::String),
        (&b"42"[..], ValueKind::Number),
        (&b"true"[..], ValueKind::Bool),
        (&b"null"[..], ValueKind::Null),
    ] {
        assert!(
            matches!(
                answer(src, &Demand::Oracle(Oracle::Count)),
                Answer::TypeMismatch { actual } if actual == kind
            ),
            "{src:?}"
        );
    }
}

#[test]
fn descend_count_counts_every_value() {
    assert_eq!(
        oracle(&answer(b"[1,2,3]", &Demand::Oracle(Oracle::DescendCount))),
        &OracleAnswer::Count(4)
    );
    assert_eq!(
        oracle(&answer(br#"{"a":1}"#, &Demand::Oracle(Oracle::DescendCount))),
        &OracleAnswer::Count(2)
    );
    assert_eq!(
        oracle(&answer(b"[]", &Demand::Oracle(Oracle::DescendCount))),
        &OracleAnswer::Count(1)
    );
    for src in [&b"\"abc\""[..], b"42", b"true", b"null"] {
        assert_eq!(
            oracle(&answer(src, &Demand::Oracle(Oracle::DescendCount))),
            &OracleAnswer::Count(1),
            "{src:?}"
        );
    }
}

#[test]
fn kind_reports_each_variant() {
    for (src, kind) in [
        (&b"[1]"[..], ValueKind::Array),
        (&br#"{"a":1}"#[..], ValueKind::Object),
        (b"\"abc\"", ValueKind::String),
        (b"42", ValueKind::Number),
        (b"true", ValueKind::Bool),
        (b"null", ValueKind::Null),
    ] {
        assert_eq!(
            oracle(&answer(src, &Demand::Oracle(Oracle::Kind))),
            &OracleAnswer::Kind(kind),
            "{src:?}"
        );
    }
}

#[test]
fn member_oracles_are_object_only() {
    let src = br#"{"a":1,"a":2,"b":3}"#;
    assert_eq!(
        oracle(&answer(src, &Demand::Oracle(Oracle::MemberNames))),
        &OracleAnswer::MemberNames(vec![String::from("a"), String::from("b")])
    );
    assert_eq!(
        oracle(&answer(src, &Demand::Oracle(Oracle::MemberCount))),
        &OracleAnswer::Count(2)
    );
    assert_eq!(
        oracle(&answer(src, &Demand::Oracle(Oracle::HasKey { key: "a".into() }))),
        &OracleAnswer::HasKey(true)
    );
    assert_eq!(
        oracle(&answer(src, &Demand::Oracle(Oracle::HasKey { key: "z".into() }))),
        &OracleAnswer::HasKey(false)
    );
    for src in [&b"[1]"[..], b"\"a\"", b"1"] {
        for demand in [
            Demand::Oracle(Oracle::MemberNames),
            Demand::Oracle(Oracle::MemberCount),
            Demand::Oracle(Oracle::HasKey { key: "a".into() }),
        ] {
            assert!(
                matches!(answer(src, &demand), Answer::TypeMismatch { .. }),
                "{src:?} {demand:?}"
            );
        }
    }
}

#[test]
fn string_byte_length_is_decoded() {
    assert_eq!(
        oracle(&answer(b"\"abc\"", &Demand::Oracle(Oracle::StringByteLength))),
        &OracleAnswer::StringByteLength(3)
    );
    // `"\n"` is two source bytes but one decoded byte.
    assert_eq!(
        oracle(&answer(br#""\n""#, &Demand::Oracle(Oracle::StringByteLength))),
        &OracleAnswer::StringByteLength(1)
    );
    // "€" is three UTF-8 bytes.
    assert_eq!(
        oracle(&answer("\"€\"".as_bytes(), &Demand::Oracle(Oracle::StringByteLength))),
        &OracleAnswer::StringByteLength(3)
    );
    assert!(matches!(
        answer(b"42", &Demand::Oracle(Oracle::StringByteLength)),
        Answer::TypeMismatch {
            actual: ValueKind::Number
        }
    ));
}

#[test]
fn an_oracle_on_a_missing_path_is_missing() {
    let demand = Demand::Path {
        steps: vec![Step::Key("nope".into())],
        nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
    };
    assert!(matches!(answer(b"{\"a\":1}", &demand), Answer::Missing));
}

#[test]
fn oracle_variants_directly_in_collection_are_declined() {
    let text = br#"[{"a":1,"b":2},{"a":3}]"#;
    let oracles: Vec<(&str, Oracle)> = vec![
        ("count", Oracle::Count),
        ("descend-count", Oracle::DescendCount),
        ("kind", Oracle::Kind),
        ("has-key", Oracle::HasKey { key: "a".into() }),
        ("member-names", Oracle::MemberNames),
        ("member-count", Oracle::MemberCount),
        ("string-byte-length", Oracle::StringByteLength),
    ];
    for (name, o) in oracles {
        let demand = collection(None, Some(Demand::Oracle(o)));
        let m = morsels(text, core::slice::from_ref(&demand), Dialect::Rfc8259);
        let tag = sharded(text, core::slice::from_ref(&demand), Dialect::Rfc8259);
        println!("collection{{oracle:{name:18}}} morsels={m} {tag}");
        assert_eq!(m, 1, "direct Collection{{nested:oracle}} must be Serial (declined)");
    }
}

#[test]
fn oracle_wrapped_in_a_spent_path_in_collection() {
    let text = br#"[{"a":1,"b":2},{"a":3},{"a":4}]"#;
    let oracles: Vec<(&str, Oracle)> = vec![
        ("count", Oracle::Count),
        ("descend-count", Oracle::DescendCount),
        ("kind", Oracle::Kind),
        ("has-key", Oracle::HasKey { key: "a".into() }),
        ("member-names", Oracle::MemberNames),
        ("member-count", Oracle::MemberCount),
        ("string-byte-length", Oracle::StringByteLength),
    ];
    let mut bad = Vec::new();
    for (name, o) in oracles {
        let demand = collection(None, Some(spent_path(Demand::Oracle(o))));
        let m = morsels(text, core::slice::from_ref(&demand), Dialect::Rfc8259);
        let tag = sharded(text, core::slice::from_ref(&demand), Dialect::Rfc8259);
        println!("collection{{path[]{{oracle:{name}}}}} morsels={m} {tag}");
        if m != 1 {
            bad.push(format!("{name}(morsels={m}, {tag})"));
        }
    }
    assert!(
        bad.is_empty(),
        "spent Path{{[]}} evades the decline: {}",
        bad.join(" | ")
    );
}

#[test]
fn oracle_deeper_in_collection() {
    let text = br#"[{"x":{"a":1,"b":2}},{"x":{"a":3}},{"x":{"a":4}}]"#;
    let path_to_oracle = Demand::Path {
        steps: vec![Step::Key("x".into())],
        nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
    };
    let cases: Vec<(&str, Demand)> = vec![
        ("collection-path-oracle", collection(None, Some(path_to_oracle.clone()))),
        (
            "collection-collection-oracle",
            collection(None, Some(collection(None, Some(Demand::Oracle(Oracle::Count))))),
        ),
        (
            "collection-path-collection-oracle",
            collection(
                None,
                Some(Demand::Path {
                    steps: vec![Step::Key("x".into())],
                    nested: Some(Box::new(collection(None, Some(Demand::Oracle(Oracle::Count))))),
                }),
            ),
        ),
        (
            "collection-spent-path-oracle",
            collection(None, Some(spent_path(Demand::Oracle(Oracle::Count)))),
        ),
    ];
    for (name, demand) in &cases {
        let m = morsels(text, core::slice::from_ref(demand), Dialect::Rfc8259);
        let tag = sharded(text, core::slice::from_ref(demand), Dialect::Rfc8259);
        println!("{name:38} morsels={m} {tag}");
        assert_eq!(m, 1, "{name}: an oracle under a Collection still shards");
        assert_eq!(tag, "AGREE", "{name}: serial vs plan diverges");
    }
}
