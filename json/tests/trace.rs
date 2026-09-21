//! Walk-trace behavior: traced scans answer like untraced ones, and the
//! trace shows marks, skips, stops, and poll counts.

mod common;

use structury::{ByteRange, Demand, Strictness};
use structury_json::{
    AnswerKind, CheckLevel, Dialect, JsonInput, ScanRequest, TraceEvent, scan, scan_traced, scan_traced_controlled,
};

fn text_req(demands: &[Demand], strictness: Strictness) -> ScanRequest<'_> {
    common::req_with(JsonInput::Text, demands, strictness, Dialect::Rfc8259)
}

#[test]
fn traced_matches_untraced_on_every_fixture() {
    let strictnesses = [Strictness::Lazy, Strictness::Structural, Strictness::Strict];
    for source in common::FIXTURES {
        for strictness in strictnesses {
            let demands = [Demand::Whole];
            let req = text_req(&demands, strictness);
            let plain = scan(source, &req).expect("untraced");
            let (traced, trace) = scan_traced(source, &req).expect("traced");
            assert_eq!(
                common::fingerprint(&traced),
                common::fingerprint(&plain),
                "{source:?} {strictness:?}"
            );
            assert_eq!(traced.issues, plain.issues);
            assert_eq!(trace.counters.demands, 1);
            assert_eq!(trace.counters.answered, 1, "{source:?} {strictness:?}");
        }
    }
}

#[test]
fn traced_matches_untraced_on_streams() {
    let src = b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
    let demands = [Demand::Whole];
    for input in [JsonInput::Ndjson, JsonInput::Adjacent, JsonInput::JsonSeq] {
        let framed: Vec<u8> = match input {
            JsonInput::JsonSeq => src
                .split(|&b| b == b'\n')
                .filter(|line| !line.is_empty())
                .flat_map(|line| [vec![0x1e], line.to_vec(), vec![b'\n']].concat())
                .collect(),
            _ => src.to_vec(),
        };
        let req = common::req(input, &demands, Dialect::Rfc8259);
        let plain = scan(&framed, &req).expect("untraced");
        let (traced, _) = scan_traced(&framed, &req).expect("traced");
        assert_eq!(common::fingerprint(&traced), common::fingerprint(&plain), "{input:?}");
    }
}

#[test]
fn whole_marks_its_span() {
    let demands = [Demand::Whole];
    let (_, trace) = scan_traced(br#"{"a":1}"#, &text_req(&demands, Strictness::Structural)).expect("traced");
    // A Whole mark locates its span at value-check level without descending,
    // then answers the document.
    assert_eq!(
        trace.events,
        vec![
            TraceEvent::Skip {
                start: 0,
                end: 7,
                check: CheckLevel::Values
            },
            TraceEvent::Mark {
                demand: 0,
                kind: AnswerKind::Document,
                span: ByteRange::try_new(0, 7),
            },
        ]
    );
    assert_eq!(trace.counters.demands, 1);
    assert_eq!(trace.counters.answered, 1);
    assert_eq!(trace.counters.polls, 0, "no control, no polls");
    assert_eq!(trace.counters.stops, 0);
}

#[test]
fn narrow_path_skips_the_unread_member() {
    let demands = [Demand::path(vec![structury::Step::Key("a".into())])];
    let (_, trace) = scan_traced(br#"{"a":1,"b":2}"#, &text_req(&demands, Strictness::Structural)).expect("traced");
    let marks: Vec<_> = trace
        .events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Mark { kind, span, .. } => Some((*kind, *span)),
            _ => None,
        })
        .collect();
    assert_eq!(marks, vec![(AnswerKind::Document, ByteRange::try_new(5, 6))]);
    let skips: Vec<_> = trace
        .events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Skip { start, end, check } => Some((*start, *end, *check)),
            _ => None,
        })
        .collect();
    // Both member values locate-skip: the wanted one at value-check level
    // before the walk descends into it, the unread one at locate level.
    assert_eq!(skips, vec![(5, 6, CheckLevel::Values), (11, 12, CheckLevel::Locate)]);
    assert_eq!(trace.counters.skipped_bytes, 2);
}

#[test]
fn slice_stop_records_the_stop() {
    let src = b"[0,1,2,3,4,5,6,7,8,9]";
    let demands = [
        Demand::Slice {
            range: structury::Range {
                start: Some(0),
                end: Some(2),
            },
            nested: None,
        },
        Demand::Slice {
            range: structury::Range {
                start: Some(1),
                end: Some(4),
            },
            nested: None,
        },
    ];
    let req = text_req(&demands, Strictness::Structural);
    let plain = scan(src, &req).expect("untraced");
    let (traced, trace) = scan_traced(src, &req).expect("traced");
    assert_eq!(common::fingerprint(&traced), common::fingerprint(&plain));
    assert_eq!(trace.counters.stops, 1);
    let stops: Vec<_> = trace
        .events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Stop { offset } => Some(*offset),
            _ => None,
        })
        .collect();
    // Windows need elements [0, 4): the walk stops at the fourth element
    // instead of the array end.
    assert_eq!(stops, vec![8]);
}

#[test]
fn cancelled_control_errors_and_benign_counts_polls() {
    let ctrl = common::control(1, 0, u64::MAX, None, || 0);
    let demands = [Demand::Whole];
    let req = text_req(&demands, Strictness::Structural);
    let error = scan_traced_controlled(common::FIXTURES[2], &req, &ctrl).expect_err("cancelled");
    assert_eq!(error.code(), "cancelled");
    // A refusal returns no trace at all; replay benignly to see the poll
    // counter move.
    let benign = common::benign();
    let (_, trace) = scan_traced_controlled(common::FIXTURES[2], &req, &benign).expect("benign");
    assert!(trace.counters.polls > 0, "polls are counted");
}

#[test]
fn stream_whole_records_per_record_skips() {
    let demands = [Demand::Whole];
    let req = common::req(JsonInput::Ndjson, &demands, Dialect::Rfc8259);
    let (result, trace) = scan_traced(b"1\n2\n3\n", &req).expect("traced");
    assert_eq!(result.answers.len(), 1);
    let skips: Vec<_> = trace
        .events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Skip { start, end, check } => Some((*start, *end, *check)),
            _ => None,
        })
        .collect();
    assert_eq!(
        skips,
        vec![
            (0, 1, CheckLevel::Locate),
            (2, 3, CheckLevel::Locate),
            (4, 5, CheckLevel::Locate)
        ]
    );
    assert_eq!(trace.counters.polls, 0, "no control, no polls");
}

#[test]
fn stream_index_demand_merges_its_extra_walker_trace() {
    // An `Index`-scoped demand on a stream runs its own walker for one record;
    // that walker's events merge into the trace after the main loop's.
    let demands = [Demand::path(vec![structury::Step::Index(1)])];
    let req = common::req(JsonInput::Ndjson, &demands, Dialect::Rfc8259);
    let plain = scan(b"1\n2\n3\n", &req).expect("untraced");
    let (traced, trace) = scan_traced(b"1\n2\n3\n", &req).expect("traced");
    assert_eq!(common::fingerprint(&traced), common::fingerprint(&plain));
    let skips: Vec<_> = trace
        .events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Skip { start, end, check } => Some((*start, *end, *check)),
            _ => None,
        })
        .collect();
    // Records 0 and 2 skip in the main loop, record 1 in its extra walker, so
    // its event appends after theirs.
    assert_eq!(
        skips,
        vec![
            (0, 1, CheckLevel::Locate),
            (4, 5, CheckLevel::Locate),
            (2, 3, CheckLevel::Values)
        ]
    );
    assert_eq!(trace.counters.skips, 3);
    assert_eq!(trace.counters.demands, 1);
    assert_eq!(trace.counters.answered, 1);
}

#[test]
fn tracing_twice_records_the_same_trace() {
    let demands = [Demand::Whole];
    let req = common::req(JsonInput::Ndjson, &demands, Dialect::Rfc8259);
    let (_, first) = scan_traced(b"1\n2\n3\n", &req).expect("traced");
    let (_, second) = scan_traced(b"1\n2\n3\n", &req).expect("traced");
    assert_eq!(first, second);
}

#[test]
fn fused_facts_path_traces() {
    let src = b"// lead\n[1] // trail\n";
    let demands = [Demand::Whole];
    let req = common::req_with(JsonInput::Text, &demands, Strictness::Strict, Dialect::Jsonc).with_facts(true);
    let plain = scan(src, &req).expect("untraced");
    let (traced, trace) = scan_traced(src, &req).expect("traced");
    assert_eq!(common::fingerprint(&traced), common::fingerprint(&plain));
    // The fused walk descends for trivia gaps; the record-only element
    // locates without answering.
    assert_eq!(
        trace.events,
        vec![
            TraceEvent::Skip {
                start: 9,
                end: 10,
                check: CheckLevel::Values
            },
            TraceEvent::Mark {
                demand: 0,
                kind: AnswerKind::Document,
                span: ByteRange::try_new(8, 11),
            },
        ]
    );
}
