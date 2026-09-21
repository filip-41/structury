# Overview

`structury` is a demand-driven codec family for structured data. A request
names the work, and the codec performs only that work on borrowed bytes, with
no intermediate document unless the request asks for one.

The page shows what the project is good for. It walks through the main paths
through the JSON codec, gives an example of each, and explains what the path
offers over the common alternatives. Later pages explain the machinery.

## Reading only what the request names

A read starts with a slice of `Demand` values. The walk matches the demands
against the structure it has to read anyway, and each demand gets its own
`Answer` when the pass ends.

```rust
use structury::{Demand, Step};
use structury_json::{Form, JsonInput, MaterializeOptions, Materialized, ScanRequest, materialize, scan};

let src = br#"{"server":{"host":"localhost","port":8080},"log_level":"info"}"#;
let demands = [Demand::Path {
    steps: vec![Step::key("server"), Step::key("port")],
    nested: None,
}];
let request = ScanRequest::new(JsonInput::Text, &demands);
let result = scan(src, &request).expect("valid JSON");
let port = materialize(
    &result.answers[0],
    MaterializeOptions::default().with_form(Form::Value),
)
.expect("a located value");
let Materialized::Value(value) = port else {
    unreachable!("Form::Value yields a value");
};
assert_eq!(value.as_i64(), Some(8080));
```

The codec reads `server.port` and the structure on the way to it, and it never
builds `server.host` or `log_level` into a value. A tree library reads the
whole buffer and builds every value, so the caller pays for regions nobody
looks at. A streaming parser can skip those regions, but the caller writes and
maintains the state machine that tracks strings, escapes, and nesting.

Materialization follows the demand shape. A `Path` answer materializes one
value, a `Project` answer materializes the projected rows, and an oracle
materializes nothing. The default `Form::Borrowed` keeps a contiguous payload
as an offset into the source, so a plain RFC 8259 read copies no payload byte,
and only escapes, decoded keys, and normalized numbers spill. An absent path
answers `Missing`. `Number` keeps the authored spelling, so `1.50` and `-0`
survive a round trip.

## Rows, filters, and counts

A projection or a filter answers a `Columns` batch rather than a value tree.
The batch holds one row per selected element, and each cell is a span into the
source or `Absent`.

```rust
use structury::{Answer, Demand, Number, Oracle, OracleAnswer, Path, Predicate, Step, Value};
use structury_json::{JsonInput, ScanRequest, scan};

let src = br#"{"users":[{"name":"ada","score":91},{"name":"bob","score":72}]}"#;
let score = Value::Number(Number::parse("80").expect("a spelling"));
let demands = [
    Demand::Filter {
        path: Path::key("users"),
        predicate: Predicate::Ge { field: "score".into(), value: score },
        project: vec!["name".into(), "score".into()],
    },
    Demand::Path {
        steps: vec![Step::key("users")],
        nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
    },
];
let result = scan(src, &ScanRequest::new(JsonInput::Text, &demands)).expect("valid JSON");
let Answer::Columns(matching) = &result.answers[0] else {
    unreachable!("a filter answers columns");
};
assert_eq!(matching.rows(), 1);
let Answer::Oracle(OracleAnswer::Count(2)) = &result.answers[1] else {
    unreachable!("a count oracle answers a count");
};
```

Answer 0 holds one row per user with a score of at least 80, and answer 1
holds the element count. A filter over a materialized array needs the rows
built first, and a null value is indistinguishable from a missing key. Here
`Columns` stores only the demanded cells, and the storage sits behind `Arc`, so
a clone of the batch shares it.

An oracle answers a property while the walk is already at the node. `Count`
counts children, `DescendCount` counts the whole subtree, and `Kind`, `HasKey`,
`MemberNames`, and `StringByteLength` answer from tokens the walk has already
read. A tree library must build the array before it can count the elements.

## Record streams

Adjacent values, NDJSON, and JSON-seq are a virtual array of top-level values,
so the same demands apply to a stream.

```rust
use structury::{Answer, ColumnCell, Demand, Path};
use structury_json::{JsonInput, ScanRequest, scan_each};

let src = b"{\"id\":1}\n{\"id\":2}\n";
let demands = [Demand::Project {
    path: Path::root(),
    fields: vec!["id".into()],
}];
let request = ScanRequest::new(JsonInput::Ndjson, &demands);
let mut records = 0;
scan_each(src, &request, |answer| {
    if let Answer::Columns(columns) = answer {
        if let Some(ColumnCell::Span(_)) = columns.cells().first() {
            records += 1;
        }
    }
})
.expect("well-formed frames");
assert_eq!(records, 2);
```

A whole-file parser must hold the stream in memory, or the caller loops over
lines and handles the framing rules alone. Blank lines, CRLF endings, and a
JSON-seq record separator are easy to get wrong. `structury` locates each
record and reuses the walker buffers across records, so allocation tracks the
demand set rather than the record count. The framing helpers report the longest
prefix that holds only complete values, so a host that feeds a growing buffer
can scan the complete part and hold back the tail.

## Edits that leave the rest alone

An edit names the target by path, and `edit` locates every target in one
`Strict` pass.

```rust
use structury::{Number, Step, Value};
use structury_json::{Dialect, Edit, EditOptions, edit};

let src = br#"{
  // the port to listen on
  "port": 8080,
}"#;
let edits = [Edit::Set {
    path: vec![Step::key("port")],
    value: Value::Number(Number::parse("9090").expect("a spelling")),
}];
let out = edit(src, &edits, EditOptions::new(Dialect::Jsonc)).expect("editable input");
let out = String::from_utf8(out).expect("UTF-8 output");
assert!(out.contains("// the port to listen on"));
assert!(out.contains("9090"));
```

The output keeps the comment and the formatting, and only the changed region
moves. A tool that parses into a tree and writes the tree back loses both. A
text patch keeps every other byte but has no idea where a member or an element
ends, so comma placement and escape handling are the caller's problem. Here
comma handling follows the grammar, `FactOp` writes, rewrites, and clears
comments, and `edit_document` keeps the facts of the input and moves them with
their nodes through the remap. When a splice cannot express a change, the codec
re-encodes a DOM as the fallback.

## Writes that keep the bytes

```rust
use structury::{Answer, Demand, Strictness};
use structury_json::{EncodeOptions, JsonInput, ScanRequest, Source, encode, scan};

let src = br#"{"port":8080}"#;
let demands = [Demand::Whole];
let request = ScanRequest::new(JsonInput::Text, &demands).with_strictness(Strictness::Strict);
let result = scan(src, &request).expect("valid JSON");
let Answer::Document(document) = &result.answers[0] else {
    unreachable!("Whole yields a document");
};
let mut out = Vec::new();
encode(Source::Document(document), &EncodeOptions::compact(), &mut out)
    .expect("a fully validated document");
assert_eq!(out.as_slice(), src.as_slice());
```

The document comes from a `Strict` scan, and `encode` copies its bytes for a
write that preserves formatting, key order, and number spellings. A serializer
built from a tree usually trusts the model it is handed, so an unvalidated or
invalid document can reach the output with no complaint. The write gate refuses
a `Document` that no `Strict` pass validated, and it refuses a document whose
grammar does not match the requested dialect. The canonical walk is available
for a rewrite, with pretty printing, record framing, and the retained comment
facts.

## Splitting work across threads

The codec starts no thread. It hands the host the ranges to run and the fold
that joins the results.

### Decode

```rust
use structury::{Demand, Drive, Path};
use structury_json::{FIRST_SHARD_BYTES, JsonInput, Plan, ScanRequest, scan};

let src = br#"{"users":[{"id":1},{"id":2},{"id":3}]}"#;
let demands = [Demand::Project { path: Path::key("users"), fields: vec!["id".into()] }];
let request = ScanRequest::new(JsonInput::Text, &demands);
let plan = Plan::build(src, &request).expect("valid JSON");
let drive = Drive::from(plan.ranges(FIRST_SHARD_BYTES));
let result = drive
    .run(|part| plan.scan(src, part), || scan(src, &request))
    .expect("valid JSON");
assert_eq!(result.answers.len(), 1);
```

A parser that owns its threads decides the pool the caller gets. Here the host
scans the ranges on whatever threads it has, and `Drive` folds the parts with
`stitch`. A wrong cut costs time, not correctness, because any non-control part
error makes `Drive` rerun the request serially. The cut scanner tracks strings
and comments, so a bracket inside a string is not a boundary. A host that reads
in blocks can summarize each block with `Plan::cut` and turn the summaries into
ranges with `Plan::host_ranges`, so the cut work spreads over the blocks.

### Encode

```rust
use structury::{Number, Value};
use structury_json::{
    EncodeOptions, ValuePlan, encode_value, encode_value_chunk, plan_encode_value,
    stitch_value_chunks,
};

let value = Value::Array(
    (0..1024)
        .map(|n| Value::Number(Number::parse(&n.to_string()).expect("a spelling")))
        .collect(),
);
let threads = 4;
let opts = EncodeOptions::compact();
let bytes = match plan_encode_value(&value, &opts, threads) {
    ValuePlan::Chunks(chunks) => {
        let parts: Vec<Vec<u8>> = chunks
            .iter()
            .map(|range| {
                let mut out = Vec::new();
                encode_value_chunk(&value, range, &opts, &mut out).expect("in bounds");
                out
            })
            .collect();
        stitch_value_chunks(&parts, &opts)
    }
    ValuePlan::Serial => encode_value(&value, &opts).expect("encodable value"),
};
assert_eq!(bytes, encode_value(&value, &opts).expect("encodable value"));
```

A serializer writes one buffer on one thread. `plan_encode_value` chunks a
top-level array into index ranges, the host encodes each range on its own
thread, and `stitch_value_chunks` joins the parts with the separators and
brackets. A value that is not an array, an array below the part size, or a
framed write stays on the serial path, so a small write never pays for
fan-out.

## Capping time and memory

```rust
use structury::{Control, Demand};
use structury_json::{JsonInput, ScanRequest, scan_controlled};

fn host_now() -> u64 {
    0
}

let src = br#"{"a":1}"#;
let demands = [Demand::Whole];
let request = ScanRequest::new(JsonInput::Text, &demands);
let ceiling_bytes = 1 << 20;
let deadline = 1_000;
let control = Control::new(ceiling_bytes, Some(deadline), host_now);
let result = scan_controlled(src, &request, &control);
assert!(result.is_ok());
// From another thread, when the work is no longer wanted:
control.cancel();
```

Most codecs can only be stopped by killing the thread or the process, and they
expose no memory ceiling. `Control` lets the host cancel a scan, set a deadline,
and cap memory. The codec checks those conditions at container and record
boundaries rather than per byte, and a stop is a distinct control error with no
partial answer. The host records measured live bytes in `used`, and the codec
refuses once `used` reaches `ceiling`. An uncontrolled request folds every poll
away at compile time, so the checks cost nothing when they are not wanted.

## What structury is good for

- Files and configuration
  - read a few fields from a large JSON or JSONC file
  - edit one value and keep the comments and the formatting
- Data sets
  - project or filter rows without building the whole array
  - count or inspect a collection with an oracle
- Streams
  - process NDJSON and JSON-seq records one at a time
- Hosts and services
  - spread decode and encode across a thread pool you own
  - cancel a scan or cap its memory on untrusted input

## Where it stops

- The codec performs no I/O, starts no thread, and reads no clock. The host
  supplies bytes and time.
- Only the JSON codec ships today. CSV and TSV, TOML, YAML, XML, HTML, CBOR,
  MessagePack, and INI are goals.
- A demand is a Rust value, not a query string. There is no JSONPath or JSON
  Pointer form, and no schema validation.
- A document read under `Structural` or `Lazy` needs a `Strict` pass before a
  byte-preserving write.

## How the pieces fit

```text
  caller
    │  bytes + ScanRequest { input, demands, strictness, dialect, … }
    ▼
  scan / scan_controlled / scan_each / scan_traced   (json/src/scan.rs)
    │
    │  one pass; each demand matched against the walk
    ▼
  ScanResult<'src>
    answers: Vec<Answer<'src>>   ← one per demand, same order
    issues:  Vec<Issue>          ← recovering diagnostics
    │
    ├── Answer::Document(span + validation + facts) ──► materialize ──► Form
    ├── Answer::Columns(projected cells)            ──► materialize ──► Form
    ├── Answer::Oracle(value computed on the walk)
    ├── Answer::Missing
    └── Answer::TypeMismatch { actual }
    │
    ├── encode(Source::{Value,Document})
    └── edit(src, &[Edit]) ──► new bytes

  parallel path where the host runs ranges:
    Plan::build(src, req) ──► ranges ──► host scans each range
        └── stitch(parts) folds the per-range ScanResults back into one
```

Next is [Core concepts](02-core-concepts.md).
See also [Introduction](README.md) and [Scan](06-scan.md).
