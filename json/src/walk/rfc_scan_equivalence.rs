use super::*;
use alloc::boxed::Box;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

/// The RFC tokenizer walk must answer exactly like the dialect lex walk with the
/// RFC dialect: same success, same end, same marks. Error offsets may differ.
fn assert_strategies_agree(src: &[u8], demand: &Demand) {
    for strictness in [Strictness::Lazy, Strictness::Structural, Strictness::Strict] {
        let one = core::slice::from_ref(demand);
        let new = scan_root_with::<RfcScan, NoRec, false, NoTrace>(
            src,
            0,
            one,
            strictness,
            crate::lex::MAX_NESTING,
            Dialect::Rfc8259,
            &[],
            false,
            false,
            &NO_CONTROL,
            NoTrace,
        );
        let old = scan_root_with::<DialectScan, NoRec, false, NoTrace>(
            src,
            0,
            one,
            strictness,
            crate::lex::MAX_NESTING,
            Dialect::Rfc8259,
            &[],
            false,
            false,
            &NO_CONTROL,
            NoTrace,
        );
        match (new, old) {
            (Ok((nm, ne, _, _, _)), Ok((om, oe, _, _, _))) => {
                assert_eq!(ne, oe, "end differs for {demand:?} {strictness:?} on {src:?}");
                assert_eq!(
                    format!("{nm:?}"),
                    format!("{om:?}"),
                    "marks differ for {demand:?} {strictness:?} on {src:?}"
                );
            }
            (Err(n), Err(o)) => {
                assert_eq!(
                    format!("{n:?}"),
                    format!("{o:?}"),
                    "error differs for {demand:?} {strictness:?} on {src:?}"
                );
            }
            (n, o) => {
                panic!("success differs for {demand:?} {strictness:?} on {src:?}: new={n:?} old={o:?}")
            }
        }
    }
}

fn spaced_boundaries(src: &[u8], trivia: &[u8]) -> Vec<Vec<u8>> {
    let punct = |byte: u8| matches!(byte, b'{' | b'}' | b'[' | b']' | b':' | b',');
    let mut variants = Vec::new();
    for i in 0..=src.len() {
        if (i > 0 && punct(src[i - 1])) || src.get(i).is_some_and(|byte| punct(*byte)) {
            let mut variant = Vec::with_capacity(src.len() + trivia.len());
            variant.extend_from_slice(&src[..i]);
            variant.extend_from_slice(trivia);
            variant.extend_from_slice(&src[i..]);
            variants.push(variant);
        }
    }
    variants
}

#[test]
fn spaced_boundaries_match_dialect_lex() {
    let bases: &[&[u8]] = &[
        br#"{"a":1}"#,
        br#"{"a":[1,2],"b":{"c":true}}"#,
        br#"[{"x":"y"},null,false,1.5]"#,
        br#"{"e":{},"f":[]}"#,
    ];
    let demands: &[Demand] = &[
        Demand::Whole,
        Demand::path(vec![Step::Key("a".into())]),
        Demand::path(vec![Step::Key("b".into()), Step::Key("c".into())]),
        Demand::Oracle(Oracle::Count),
        Demand::Project {
            path: structury::Path::root(),
            fields: vec!["a".into(), "b".into()],
        },
    ];
    for base in bases {
        for trivia in [&b" "[..], b"\t", b"\n", b"\r", b"  \n"] {
            for src in spaced_boundaries(base, trivia) {
                for demand in demands {
                    assert_strategies_agree(&src, demand);
                }
            }
        }
    }
}

#[allow(clippy::too_many_lines)] // exhaustive differential corpus; splitting would obscure coverage
#[test]
fn rfc_scan_answers_match_dialect_lex_on_rfc_inputs() {
    const ALPHABET: &[u8] = b"{}[],:\"\\ \t\n0123456789.eE+-truefalsn abrowsid";
    const TOKENS: &[&[u8]] = &[
        b"{",
        b"}",
        b"[",
        b"]",
        b",",
        b":",
        b" ",
        b"\t",
        b"\n",
        b"\"a\"",
        b"\"\"",
        b"\"x\\n\"",
        b"1",
        b"-2",
        b".5",
        b"1e3",
        b"01",
        b"-",
        b"+1",
        b"e",
        b"true",
        b"false",
        b"null",
        b"\"\\u0041\"",
        b"\"\xff\"",
        b"0",
        b"9",
        b"-\"a\"",
    ];
    let demands: Vec<Demand> = vec![
        Demand::Whole,
        Demand::path(vec![Step::Key("a".into())]),
        Demand::path(vec![Step::Index(0)]),
        Demand::Oracle(Oracle::Kind),
        Demand::Oracle(Oracle::Count),
        Demand::Oracle(Oracle::DescendCount),
        Demand::Oracle(Oracle::MemberNames),
        Demand::Oracle(Oracle::MemberCount),
        Demand::Oracle(Oracle::HasKey { key: "a".into() }),
        Demand::Project {
            path: structury::Path::root(),
            fields: vec!["a".into(), "b".into()],
        },
        Demand::Collection {
            fields: None,
            nested: None,
        },
        Demand::Filter {
            path: structury::Path::root(),
            predicate: Predicate::Eq {
                field: "a".into(),
                value: Value::Bool(true),
            },
            project: vec!["a".into()],
        },
        Demand::Filter {
            path: structury::Path::root(),
            predicate: Predicate::Gt {
                field: "a".into(),
                value: Value::Number(structury::Number::parse("2").expect("2")),
            },
            project: vec!["b".into()],
        },
        Demand::Slice {
            range: structury::Range {
                start: Some(0),
                end: Some(2),
            },
            nested: Some(Box::new(Demand::Project {
                path: structury::Path::root(),
                fields: vec!["a".into()],
            })),
        },
        Demand::Path {
            steps: vec![Step::Key("rows".into())],
            nested: Some(Box::new(Demand::Slice {
                range: structury::Range {
                    start: Some(0),
                    end: Some(2),
                },
                nested: Some(Box::new(Demand::Project {
                    path: structury::Path::root(),
                    fields: vec!["id".into()],
                })),
            })),
        },
    ];
    let fixed: &[&[u8]] = &[
        b"",
        b" ",
        b"null",
        b"true",
        b"false",
        b"0123",
        b"1.",
        b"1e",
        b"01",
        b"{}",
        b"[]",
        b"{\"a\":1}",
        b"{\"a\":1,}",
        b"{,}",
        b"[1,2,3]",
        b"[1,2,]",
        b"[[[[[[1]]]]]]",
        b"{\"a\":[{\"b\":true},null],\"c\":\"x\\ny\"}",
        b"{\"rows\":[{\"id\":1},{\"id\":2},{\"id\":3}]}",
        b"\"a\\u0041b\"",
        b"\"unterminated",
        b"\"\\\"\"",
        b"{\"a\":}",
        b"{\"a\" 1}",
        b"{\"a\":1 \"b\":2}",
        b"{\"a\":\"\\ud83d\\ude00\"}",
        b"{\"a\":\"\\ud83d\"}",
        b"[true,false,null,0,-0.5e10]",
        b"\"caf\xc3\xa9\"",
        b"\"\xff\"",
        b"{\"a\":1,\"a\":2}",
        b"{\"\\u0061\":1}",
        b".0",
        b"+1",
        b"e5",
        b"-.5",
        b"-e0",
        b"[.0,+1,e5]",
        b"{\"a\":.5}",
        b"{\"a\":+1}",
        b"{\"a\":-]",
        b"-",
        b"[-]",
    ];
    let mut state = 0x243f_6a88_85a3_08d3u64;
    let mut corpus: Vec<Vec<u8>> = fixed.iter().map(|s| s.to_vec()).collect();
    for _ in 0..3000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let len = (state % 40) as usize;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            bytes.push(ALPHABET[usize::try_from(state % ALPHABET.len() as u64).unwrap_or(0)]);
        }
        corpus.push(bytes);
    }
    // Token-level generation reaches value-start dispatches the byte
    // generator rarely hits (`.5` after `:`, `-e0` at a member value).
    for _ in 0..4000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let count = (state % 12) as usize + 1;
        let mut bytes = Vec::new();
        for _ in 0..count {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            bytes.extend_from_slice(TOKENS[usize::try_from(state % TOKENS.len() as u64).unwrap_or(0)]);
        }
        corpus.push(bytes);
    }
    for src in &corpus {
        for demand in &demands {
            assert_strategies_agree(src, demand);
        }
    }
}
