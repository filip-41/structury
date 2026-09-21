//! Frame partitions and the growing-tail holdback law.

mod common;

use structury_json::{
    JsonInput, adjacent_prefix_len, complete_prefix_len, partition_adjacent, partition_json_seq, partition_ndjson,
};

#[test]
fn ndjson_partition_tiles_whole_records() {
    let src = b"1\n22\n333\n";
    let parts = partition_ndjson(src, 4);
    assert!(parts.len() >= 2, "about-target morsels: {parts:?}");
    let covered: usize = parts.iter().map(|range| range.end() - range.start()).sum();
    assert_eq!(covered, src.len(), "morsels tile the source");
    for window in parts.windows(2) {
        assert!(window[0].end() <= window[1].start(), "ascending: {window:?}");
    }
}

#[test]
fn adjacent_and_seq_partitions_cover_values() {
    assert!(!partition_adjacent(b"1 2 3", 2).is_empty(), "adjacent values partition");
    assert!(
        !partition_json_seq(b"\x1e1\n\x1e22\n", 3).is_empty(),
        "seq records partition"
    );
}

#[test]
fn complete_prefix_holds_back_an_unterminated_tail() {
    assert_eq!(complete_prefix_len(b"1\n2\n", JsonInput::Ndjson), 4);
    assert_eq!(complete_prefix_len(b"1\n2", JsonInput::Ndjson), 2);
    assert_eq!(complete_prefix_len(b"1\r\n2\r\n", JsonInput::Ndjson), 6);
    assert_eq!(complete_prefix_len(b"", JsonInput::Ndjson), 0);
    assert_eq!(complete_prefix_len(b"\x1e1\n\x1e2", JsonInput::JsonSeq), 3);
    assert_eq!(complete_prefix_len(b"\x1e1\n\x1e2\n", JsonInput::JsonSeq), 6);
}

#[test]
fn complete_prefix_holds_nothing_back_without_a_frame_law() {
    assert_eq!(complete_prefix_len(br#"{"a":1}"#, JsonInput::Text), 7);
    assert_eq!(complete_prefix_len(b"1 2", JsonInput::Adjacent), 3);
}

#[test]
fn adjacent_prefix_holds_back_a_touching_tail() {
    use structury_json::{Dialect, MAX_NESTING};
    let prefix = |src: &[u8]| adjacent_prefix_len(src, MAX_NESTING, Dialect::Rfc8259);
    assert_eq!(prefix(b"1 2 3"), 4, "the touching 3 may extend");
    assert_eq!(prefix(b"1 2 3 "), 6, "trailing space seals the 3");
    assert_eq!(prefix(br#"{"a":1} {"b":"#), 8, "partial second value held");
    assert_eq!(prefix(br#"{"a":}"#), 0, "partial only value held");
    assert_eq!(prefix(b"12"), 0, "a bare number may extend");
    assert_eq!(prefix(b"12 "), 3, "trailing space seals the number");
    assert_eq!(prefix(b""), 0);
    assert_eq!(prefix(b"   "), 3, "whitespace drains");
}

#[test]
fn adjacent_scan_recovers_a_partial_tail() {
    use structury::{Answer, Demand, Strictness};
    use structury_json::{ScanRequest, scan, scan_each_with_issues};
    let demands = [Demand::Whole];
    let req = ScanRequest::new(JsonInput::Adjacent, &demands);
    let result = scan(b"1 2 {\"a\":", &req).expect("tail recovers");
    match &result.answers[0] {
        Answer::Columns(columns) => assert_eq!(columns.rows(), 2),
        other => panic!("{other:?}"),
    }
    assert_eq!(result.issues.len(), 1, "the partial tail is reported");
    let tail_code = scan(b"{\"a\":", &ScanRequest::new(JsonInput::Text, &demands))
        .expect_err("tail alone fails")
        .code();
    assert_eq!(result.issues[0].code, tail_code, "tail carries the walk's own code");

    let strict = ScanRequest::new(JsonInput::Adjacent, &demands).with_strictness(Strictness::Strict);
    let mut seen = 0usize;
    let issues = scan_each_with_issues(b"1 2 {\"a\":", &strict, |_| seen += 1).expect("tail recovers");
    assert_eq!(seen, 2);
    assert_eq!(issues.len(), 1);
}

#[test]
fn adjacent_feeder_loop_matches_one_shot() {
    use structury::Demand;
    use structury_json::{Dialect, Form, MaterializeOptions, ScanRequest, materialize, scan};
    let src = br#"{"a":1} {"b":[1,2]}"#;
    let demands = [Demand::Whole];
    let mut buf = src.to_vec();
    let mut values = Vec::new();
    for chunk in [8, 5, 100] {
        let take = chunk.min(buf.len());
        let mut feed: Vec<u8> = buf.drain(..take).collect();
        let n = adjacent_prefix_len(&feed, structury_json::MAX_NESTING, Dialect::Rfc8259);
        let head = feed.split_off(n);
        if n > 0 {
            let req = ScanRequest::new(JsonInput::Adjacent, &demands);
            let result = scan(&feed, &req).expect("complete prefix scans");
            assert!(result.issues.is_empty());
            for answer in &result.answers {
                let opts = MaterializeOptions::new(Dialect::Rfc8259, Form::Value);
                if let Ok(v) = materialize(answer, opts).map(structury_json::Materialized::into_value) {
                    values.push(v);
                }
            }
        }
        buf.splice(..0, head);
    }
    let req = ScanRequest::new(JsonInput::Adjacent, &demands);
    let tail = scan(&buf, &req).expect("finish scans the remainder");
    for answer in &tail.answers {
        let opts = MaterializeOptions::new(Dialect::Rfc8259, Form::Value);
        if let Ok(v) = materialize(answer, opts).map(structury_json::Materialized::into_value) {
            values.push(v);
        }
    }
    let oracle = scan(src, &req).expect("one-shot");
    let mut expected = Vec::new();
    for answer in &oracle.answers {
        let opts = MaterializeOptions::new(Dialect::Rfc8259, Form::Value);
        expected.push(
            materialize(answer, opts)
                .map(structury_json::Materialized::into_value)
                .expect("materialize"),
        );
    }
    // Each push answers its own virtual-array batch; the batches concatenate
    // to the one-shot batch.
    assert_eq!(values.len(), 2, "one batch per drained push plus finish");
    let flat: Vec<_> = values
        .iter()
        .flat_map(|v| match v {
            structury::Value::Array(items) => items.clone(),
            other => vec![other.clone()],
        })
        .collect();
    let oracle_flat: Vec<_> = expected
        .iter()
        .flat_map(|v| match v {
            structury::Value::Array(items) => items.clone(),
            other => vec![other.clone()],
        })
        .collect();
    assert_eq!(flat, oracle_flat);
}
