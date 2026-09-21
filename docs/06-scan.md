# Scan

A scan reads a buffer once and turns a slice of `Demand` values into one
`Answer` per demand. Every codec owns its scan, because the grammar and the
structure are format-specific. The core supplies the demand vocabulary, the
answer model, and the fold, while the codec supplies the lexer and the walk.
The JSON codec is the first implementation, and the page uses it as the worked
example.

The JSON codec's scan lives in `json/src/scan.rs`, `json/src/lex/`, and
`json/src/walk/`.

## The entry points

A codec exposes entries for the common ways to run a scan: plain, traced,
controlled, traced and controlled, per record, per record with issues, and
validation only. The JSON codec has seven in `json/src/scan.rs`.
All seven build a `ScanRequest` with `ScanRequest::new(input, demands)` plus
`with_*` builders and dispatch on `JsonInput`.

- `scan(src, req)` runs one pass with no host control.
- `scan_traced(src, req)` runs the same pass and returns a `Trace` of what the
  walk did alongside the answers.
- `scan_controlled(src, req, control)` runs the same pass and polls `Control`
  at container and record boundaries.
- `scan_traced_controlled(src, req, control)` combines the two.
- `scan_each(src, req, visit)` visits each framed value and its answers as the
  walk goes, instead of returning one combined `ScanResult`.
- `scan_each_with_issues(src, req, visit)` visits like `scan_each` and returns
  the recovered per-record issues, so a malformed final record is reported
  instead of dropped.
- `validate(src, dialect)` runs a `Strict` scan for a `Whole` demand and
  discards the answers, so it returns only grammar success.

```text
scan / scan_traced / the controlled pair (one dispatch)
  │
  ├─ JsonInput::Text ─► scan_text   ──► walk::scan_root
  └─ Adjacent/Ndjson/JsonSeq ─► scan_stream ──► frames + per-record walker
```

`scan_text` in `json/src/scan.rs` strips a UTF-8 BOM, skips leading trivia, and
calls `walk::scan_root`. If the scan is not `Strict` it sets `allow_stop`, so a
bounded `Slice` window can stop the walk once its last element is read, while a
`Strict` scan never stops early because it must validate every byte. After the
walk it checks that only trivia remains, and leftover non-whitespace becomes
`json.syntax` trailing-content through `error::trailing_content`.

There is also a fused facts path with `scan_text_fused`. The path runs when a
request collects comment facts and holds a `Whole` demand. It drives the same
walk with a `Recorder` sink so comments are recorded as trivia is skipped,
instead of running a second pass. Facts are covered in
[Framing and dialects](04-framing-and-dialects.md).

## The walk trace

The answers say what each demand produced, not what the walk read on the way.
`scan_traced` and `scan_traced_controlled` return a `Trace` beside the answers:
one `TraceEvent::Mark` per demand with the answer kind and span, a
`TraceEvent::Skip` for each region located without being walked, and a
`TraceEvent::Stop` when the all-slice early stop fires. `TraceCounters` counts
the demands, answered marks, skip regions and their bytes, control polls, and
stops. A skip carries a `CheckLevel`, either `Locate` or `Values`, and a region
re-read at another level counts twice.

Tracing follows the seam pattern of the control flag: the sink is a const
generic over `NoTrace` and `Recorder` in `json/src/trace.rs`, so the plain
entries compile it away. A refusal returns no partial trace, because the
refusal offset lives on the error. A stream runs an extra walker for an
`Index`-scoped demand, and its events append after the main walk's.

```rust
use structury::Demand;
use structury_json::{CheckLevel, JsonInput, ScanRequest, TraceEvent, scan_traced};

let demands = [Demand::path(vec![structury::Step::Key("a".into())])];
let request = ScanRequest::new(JsonInput::Text, &demands);
let (_result, trace) = scan_traced(br#"{"a":1,"b":2}"#, &request).expect("valid");
let skips: Vec<_> = trace
    .events
    .iter()
    .filter_map(|event| match event {
        TraceEvent::Skip { check, .. } => Some(*check),
        _ => None,
    })
    .collect();
// The wanted member locates at value-check level, the unread one at locate level.
assert_eq!(skips, vec![CheckLevel::Values, CheckLevel::Locate]);
```

## The lex layer

A codec's lexer moves over a value without necessarily understanding the
contents of the value. Two levels are possible: locate the end of the value, or
check the value. The JSON codec's `Check` setting chooses between them.

```rust
pub(crate) enum Check {
    Locate,   // structure only: find the end of the value
    Values,   // RFC value checks: grammar, UTF-8, escapes, numbers
}
```

The skip engine in `json/src/lex/skip.rs` is monomorphized over three const
booleans, namely `RFC` for the dense RFC 8259 byte-class table, `VALUES` for
value checking against locating, and `COUNT` for also counting preorder nodes.
For that reason a locate scan and a strict scan are two specializations of one
function, and the RFC path folds out the dialect branches entirely.
`MAX_NESTING = 256` is the default container bound, and exceeding the bound is
`json.limit`.

Key entry points are `skip_value` for skipping leading trivia then one value,
`skip_present` for a value whose first byte is already known, and
`skip_value_counting` for also returning the preorder count. Numbers, strings,
and literals each route to their own lexer submodule, so a skip never
re-implements grammar. Strings have a SIMD content run with
`prefix_len::<StringContent>` and a locate run that resolves an escaped quote by
backslash parity with `quote_is_escaped` in `json/src/lex/string.rs`.

## The demand walk

The walk matches the demands against the structure it has to read anyway, and
each demand gets its own answer. The walk seeds one answer slot per demand,
called a mark in the source, and fills the slot when the demand matches. A
borrowed view of the demand guides the walk, so a path is consumed one step at
a time instead of being re-resolved at every node.

In the JSON codec, `walk::scan_root` in `json/src/walk/mod.rs` is where demands
meet structure. It picks a `Scan` strategy with `RfcScan` in
`json/src/walk/rfc.rs` for RFC 8259 and `DialectScan` in
`json/src/walk/dialect.rs` for JSONC and JSON5, and it walks once.

```text
demands ─► hits_for ─► Vec<Hit{ idx, View }>
                          │  View::from_demand maps each Demand to a walk view
                          ▼
marks = vec![Answer::Missing; demands.len()]
walk_node(root_hits)
  │
  ├─ array  ─► walk_array  ─► element loop ─► fold_row ─► Columns / Document
  ├─ object ─► walk_object ─► member loop  ─► apply_object_row ─► Columns
  │                                              (Key/Project/Filter match on bytes)
  ├─ scalar ─► skip_scalar ─► Document or TypeMismatch
  └─ oracle ─► fill_oracle ─► OracleAnswer
                          │
                          ▼
answers: N marks, one per demand
```

A `View` is the borrowed form of a `Demand` used during the walk, with `Whole`,
`Path`, `Collection`, `Slice`, `Project`, `Filter`, and `Oracle`, plus a
`Record` view used only by the fused facts walk. As the walk descends,
`flatten_here` unwraps a spent `Path{[]}` to its nested demand, and
`enter_array` and `enter_object_bytes` consume one step at a time so a path is
never re-resolved from scratch per node.

The array element loop needs care. Each element starts with its own answer
slot, and a per-element `Project` or `Filter` accumulates into a row
accumulator that waits on the walker stack with `open_rows`, `fold_row`, and
`close_rows` in `json/src/walk/rows.rs`. For that reason a projection holds one
row per element, with an absent row for an element that is not an object.
`RowShape` encodes the one-row-per-element law, where `Projected` always
contributes a row, `Filtered` contributes zero or one row, and `Whole`
contributes the own answer of the element.

For plain object member access, the walk keeps a `Members` map with last value
winning and a `BTreeMap` index only past 16 members through `INDEX_THRESHOLD`
in `json/src/walk/member.rs`. For uniform arrays of objects it also retains a
row layout with the previous row member heads, so the next identical row skips
key tokenizing entirely. The optimization does not show in the API, and it
exists because it dominated the profile on the common array of same-shaped
objects.

## How demands map to work

The demand kinds cover the common questions, and each codec decides how they
map onto its own structure. The JSON codec maps them as follows.

- `Whole` skips or checks the entire node and answers a `Document` with its
  span.
- `Path` follows static steps. At an object it matches member keys by comparing
  bytes when the key is a plain double-quoted run and by decoding only when the
  key is escaped or dialect-spelled, while at an array it resolves an index. A
  wrong kind answers `TypeMismatch` with `actual`, and an absent key answers
  `Missing`.
- `Collection` maps elements, where `fields: Some` becomes a per-element
  `Project` and `nested` becomes a per-element demand.
- `Slice` resolves `range` against the array length and keeps or replays the
  windowed elements.
- `Project` and `Filter` navigate to an array and produce a `Columns` batch.
  Equality compares a demanded value against the member source span, so only
  scalar `Eq` and `Ne` operands can be served, while a container operand is a
  `predicate-operand` shape refusal and not a silent non-match. The DOM path
  with `Predicate::matches` does compare containers structurally.
- `Oracle` is filled during the walk with `fill_oracle`, where `Count` counts
  direct children, `DescendCount` walks the subtree, and `MemberNames` and
  `HasKey` come from the member map.

## Streams

A stream is a virtual array of top-level values, so the same demands apply and
the walk can treat each record as an element. The codec owns the framing, and
the walk can visit one record at a time.

In the JSON codec, adjacent, NDJSON, and JSON-seq behave as a virtual array
with `scan_stream` in `json/src/scan.rs`. The frames module locates each
record, then a persistent `StreamWalker` in `json/src/walk/stream.rs` reuses
its per-record buffers across records, so allocations scale with the demand
set and not with the record count. A row demand that no record answered gets
the empty batch that the same demand would get on an empty text array.
`scan_each` visits one record and its answers at a time, while a fixed answer
set is folded across records by the `StreamPlan`.

## Strictness

The check level is a request setting, and it does not change which structure
the walk reads. In the JSON codec, `Walker::unread` and `Walker::demanded` in
`json/src/walk/mod.rs` choose the `Check` per region.

```text
Strict      unread = Values, demanded = Values   (validate everything)
Structural  unread = Locate, demanded = Values   (check only what was asked)
Lazy        unread = Locate, demanded = Locate   (defer all value checks)
```

`json/src/scan.rs` also derives the lex check from strictness in `check_of`,
used by stream and framing paths that skip values directly. A demanded value is
still located under `Lazy`, and its bytes are only checked later when
`materialize` builds the value. A `Document` from a pass other than `Strict` is
not fully validated, which is why writes refuse such documents.

Next is [Sharding](07-sharding.md).
See also [Introduction](README.md) and [Framing and dialects](04-framing-and-dialects.md).
