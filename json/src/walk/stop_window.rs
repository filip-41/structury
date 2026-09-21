use super::*;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

fn window(start: i64, end: i64, nested: Option<Demand>) -> Demand {
    Demand::Slice {
        range: structury::Range {
            start: Some(start),
            end: Some(end),
        },
        nested: nested.map(Box::new),
    }
}

fn path(key: &str, nested: Demand) -> Demand {
    Demand::Path {
        steps: vec![Step::Key(String::from(key))],
        nested: Some(Box::new(nested)),
    }
}

fn project(fields: &[&str]) -> Demand {
    Demand::Project {
        path: structury::Path::root(),
        fields: fields.iter().map(|field| String::from(*field)).collect(),
    }
}

fn fingerprint(answer: &Answer<'_>) -> String {
    match answer {
        Answer::Columns(columns) => {
            let cells = columns
                .cells()
                .iter()
                .map(|cell| format!("{cell:?}"))
                .collect::<Vec<_>>()
                .join(",");
            format!("columns[{}]({cells})", columns.fields().join("|"))
        }
        Answer::Document(doc) => format!("document{:?}", doc.root()),
        other => format!("{other:?}"),
    }
}

fn walk(src: &[u8], demands: &[Demand], allow_stop: bool) -> (Vec<String>, Vec<usize>, usize, bool) {
    let (answers, end, stopped, _, _) = scan_root_with::<DialectScan, NoRec, false, NoTrace>(
        src,
        0,
        demands,
        Strictness::Structural,
        crate::lex::MAX_NESTING,
        Dialect::Rfc8259,
        &[],
        allow_stop,
        false,
        &NO_CONTROL,
        NoTrace,
    )
    .expect("scan");
    let marks = answers.iter().map(fingerprint).collect();
    let rows = answers
        .iter()
        .map(|answer| match answer {
            Answer::Columns(columns) => columns.rows(),
            _ => 0,
        })
        .collect();
    (marks, rows, end, stopped)
}

#[test]
fn multi_slice_stop_matches_full_walk() {
    let src = br#"[{"id":1,"name":"a"},{"id":2,"name":"b"},{"id":3,"name":"c"},{"id":4,"name":"d"},{"id":5,"name":"e"},{"id":6,"name":"f"},{"id":7,"name":"g"},{"id":8,"name":"h"}]"#;
    let demands = vec![
        window(0, 2, Some(project(&["id"]))),
        window(3, 6, Some(project(&["name"]))),
        window(1, 4, None),
    ];
    let (full_marks, full_rows, _, full_stopped) = walk(src, &demands, false);
    let (cut_marks, cut_rows, end, cut_stopped) = walk(src, &demands, true);
    assert!(!full_stopped, "allow_stop=false must walk the whole array");
    assert!(cut_stopped, "all-Slice demands must stop");
    assert_eq!(full_marks, cut_marks, "stopping changed an answer");
    assert_eq!(full_rows, cut_rows);
    assert_eq!(cut_rows, vec![2, 3, 3], "each Slice keeps its own window");
    assert!(end < src.len(), "stop point must precede the array end");
}

#[test]
fn stop_point_is_max_window_end() {
    let src = b"[0,1,2,3,4,5,6,7,8,9]";
    let demands = vec![window(0, 2, None), window(3, 6, None), window(1, 4, None)];
    let (full_marks, _, full_end, full_stopped) = walk(src, &demands, false);
    let (cut_marks, cut_rows, end, cut_stopped) = walk(src, &demands, true);
    assert!(!full_stopped);
    assert!(cut_stopped);
    assert_eq!(full_marks, cut_marks);
    assert_eq!(cut_rows, vec![2, 3, 3]);
    assert_eq!(end, 2 * 6, "need = max(2, 6, 4) = 6 elements");
    assert!(end < full_end);
}

/// A demand that is not a bounded `Slice` at the stopping array must keep the
/// walk going: the stop unwinds everything.
#[test]
fn sibling_demand_blocks_stop() {
    let src = br#"{"rows":[{"id":1},{"id":2},{"id":3}],"meta":{"ok":true}}"#;
    let demands = vec![
        path("rows", window(0, 2, Some(project(&["id"])))),
        Demand::Path {
            steps: vec![Step::Key(String::from("meta"))],
            nested: None,
        },
    ];
    let (full_marks, _, _, _) = walk(src, &demands, false);
    let (cut_marks, _, _, cut_stopped) = walk(src, &demands, true);
    assert!(!cut_stopped, "a sibling demand must block the stop");
    assert_eq!(full_marks, cut_marks);
    assert!(
        cut_marks[1].starts_with("document"),
        "sibling must stay resolved, got {}",
        cut_marks[1]
    );
}

#[test]
fn multi_slice_through_shared_key_stops() {
    let src = br#"{"users":[{"id":1},{"id":2},{"id":3},{"id":4}],"meta":{"ok":true}}"#;
    let demands = vec![
        path("users", window(0, 2, Some(project(&["id"])))),
        path("users", window(1, 3, Some(project(&["id"])))),
    ];
    let (full_marks, _, _, full_stopped) = walk(src, &demands, false);
    let (cut_marks, cut_rows, _, cut_stopped) = walk(src, &demands, true);
    assert!(!full_stopped);
    assert!(cut_stopped, "shared-key all-Slice demands must stop");
    assert_eq!(full_marks, cut_marks);
    assert_eq!(cut_rows, vec![2, 2]);
}

#[test]
fn public_scan_reaches_stop_for_multi_slice() {
    use crate::scan::{JsonInput, ScanRequest, scan};
    let src = b"[0,1,2,3,4,5,6,7,8,9] trailing";
    let demands = vec![window(0, 2, None), window(1, 4, None)];
    let req = ScanRequest {
        input: JsonInput::Text,
        demands: &demands,
        strictness: Strictness::Structural,
        max_nesting: crate::lex::MAX_NESTING,
        dialect: Dialect::Rfc8259,
        facts: false,
    };
    let result = scan(src, &req).expect("multi-slice request stops before trailing junk");
    assert_eq!(result.answers.len(), 2);
}

/// A retained head within 8 bytes of EOF must still match: the stored prefix is
/// exact, so a partial word load may only lose the fast path, never the answer.
#[test]
fn head_within_eight_bytes_of_eof_still_matches() {
    let bytes = br#"{"a":1}"#;
    // Head `"a":` at 1 with its value at 4 (len 3): only 6 bytes remain.
    let head = Head::new(bytes, 1, 4, Some(0));
    assert!(head_prefix_eq(&head, bytes, 1));
    assert!(!head_prefix_eq(&head, bytes, 5), "a different head must not match");

    // A longer head spanning both words, still with fewer than 8 bytes left
    // after its first word.
    let bytes = br#"{"abcdefghij":1}"#;
    let head = Head::new(bytes, 1, 14, Some(0));
    assert!(head_prefix_eq(&head, bytes, 1));
}
