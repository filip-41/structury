# Sharding

A host can answer one request over independent source ranges in parallel, even
though a codec starts no thread. The codec supplies a plan: it cuts the source
into ranges, decides which requests are eligible, and folds the per-range
answers back into one result. The host runs the ranges on its own threads.

Sharding is codec-owned, like framing and dialects. A codec knows where its
records and elements end, so it owns the cut and the eligibility rules. The core
owns the fold with `Drive` and `stitch`, described in
[Host seam and sharding](03-host-seam-and-sharding.md). The JSON codec is the
first implementation, in `json/src/shard.rs`.

## The plan

A plan owns the cut and the per-range scan. The JSON codec's `Plan` holds the
parts, the retained request, and a projection compiled once.

```rust,ignore
pub struct Plan {
    parts: Parts,          // Elements { spans, close_end } | Packed(ranges)
    len: usize,
    demands: Vec<Demand>,  // the request, retained so scan() need not take it again
    strictness: Strictness,
    max_nesting: u32,
    dialect: Dialect,
    input: JsonInput,
    facts: bool,
    window: Window,        // per-morsel demand projection, compiled once
}
```

`Plan::build(src, req)` is the entry point. It first checks `shard_eligible`
as described below. If the request is not eligible it returns a single
whole-document range. Otherwise it dispatches on input.

- `Text` resolves the array that the common path of the demands points at with
  `array_start`, cuts the array body into top-level element spans with
  `element_ranges`, and builds a plan from those spans when there are at least
  two elements and the tail validates. Otherwise it falls back to one range.
- NDJSON, JSON-seq, and Adjacent pack whole records into morsels of
  `FIRST_SHARD_BYTES` (256 KiB) or more with the frame partitioners
  `partition_ndjson`, `partition_json_seq`, and `partition_adjacent`.

`FIRST_SHARD_BYTES` is the first-cut morsel size. It is chosen for time to
first byte, so the host can start on the first chunk without waiting to index
the whole input.

The public protocol methods are as follows.

- `Plan::ranges(target)` returns morsel ranges of about `target` bytes, or one
  full range when packing yields at most one range.
- `Plan::scan(src, range)` scans one planned range and returns its
  `ScanResult`. A full-document range delegates to the ordinary `scan`, a text
  range is a run of root-array elements scanned as an adjacent body, and a
  stream range is scanned per record.
- `Plan::elements()` returns the indexed top-level element count, or 0 for
  streams and fallback.
- `Plan::array_start(src, req)` returns the `[` that a shard-eligible demand
  resolves to.
- `Plan::cut`, `Plan::host_ranges`, and `Plan::from_summaries` form the host
  protocol described below.

The `Window` inside `Plan` compiles the per-morsel demand once. A `Sum` demand
is flagged because the window value count will answer it, and every other
demand is mapped onto one record with `consumed_row_demand`. The single
compilation avoids re-cloning every `Project` or `Filter` field and predicate
per morsel.

## The cut scanner

The cut has to find array-level commas and the closing `]` without being fooled
by brackets inside strings or comments. In the JSON codec it is a small state
machine over a byte block with `scan_block`, driven by `ScanState`.

```rust
enum ScanState {
    Normal,
    String { quote: u8, escaped: bool },
    LineComment,
    BlockComment { star: bool },
    Slash,      // the block ended on '/'; the next byte decides whether a comment opens
}
```

The `advance_trivia` function consumes string and comment state with SIMD runs
using the same stop sets that the lexer uses. In `Normal` state the scanner
tracks bracket depth and records two things, namely array-level comma
candidates only at the running minimum depth of the block, which drops field
commas, and `]` closers with their local depth. It also accumulates a net
`depth_delta`, the end state, and the first lex fault such as a lone `/`, an
unterminated string, or an open block comment. The `BlockScan` result is the
summary of one block.

The key property is that the state carried across a block boundary is enough to
resume exactly. A block that starts mid-string or mid-comment is rescanned under
the incoming state with `elements_from_summaries`. The cut works for every
dialect because a `/` or a single quote outside a string is invalid in every
dialect that the scanner reads, so those bytes only occur where tracked string
state suppresses structure.

## Cut summaries and the host protocol

A host that reads the source in blocks can cut as it goes, without waiting for
the whole buffer. The codec summarizes each block, and the summaries turn into
ranges in order.

`CutSummary { block, scan: BlockScan }` is the cut summary of one host block in
the JSON codec. The protocol works as follows.

```text
host: partition the array body into contiguous blocks (its own I/O choice)
host: for each block in ascending order:  summary = Plan::cut(src, block)
      summaries ─────────────────────────────────────────────► Plan
                                                    Plan::host_ranges(src, req, summaries, target)
                                                      or
                                                    Plan::from_summaries(src, start, summaries, req)
                                                          │
                                                          ▼
                                                   ranges / Plan::scan
```

`Plan::host_ranges` returns ranges for the host to run, while
`Plan::from_summaries` builds a retained `Plan` so later queries are an index
lookup plus a scan. The `elements_from_summaries` function folds the ordered
summaries. It tracks global depth, pushes an element at each array-level comma,
and stops at the matching closer. Only a block that begins inside a string or
comment is rescanned, while a normal block uses its summary directly.
`Plan::cut` is the per-block call of the host, and it ignores lex faults
because the serial planner raises those faults from `Plan::build`. The
summaries must cover the array in ascending order as `host_ranges` expects.

## Eligibility

Eligibility is the core `Shard` law plus any codec-specific exceptions. The
core law says which demands can run in independent ranges and how their partial
answers fold, and it is described in
[Host seam and sharding](03-host-seam-and-sharding.md). The JSON codec adds two
framing exceptions.

- A commenting dialect cannot cut an Adjacent or JSON-seq buffer, because the
  adjacent and JSON-seq framers are RFC-only. NDJSON is fine because its frame
  boundary is a newline and not a grammar decision.
- A stream with a key-spine demand is not eligible, where a key-spine demand
  means a `Path` demand or a `Project` or `Filter` demand with a non-empty
  path. A stream is a virtual array and not the object holding a text array, so
  a key spine has no record-wise shard law here.

After those checks, eligibility is exactly
`structury::Drive::eligible(req.demands)`, meaning every demand reports a
parallel `Demand::shard` of `Concat` or `Sum`. Anything else uses one serial
range.

## End to end

```text
Plan::build(src, req)
   │  eligible?  no ─► one range: [0, len)
   │  yes
   ▼
array_start / frame partition ─► element spans or packed ranges
   │
   ▼
Plan { parts, demands, window }
   │
   ├─ Plan::ranges(target) ─► [r0, r1, r2]
   │        host runs Plan::scan(src, ri) per range (any threads it likes)
   │             each returns ScanResult with N answers in demand order
   │        structury::Drive::run / stitch folds them into one ScanResult
   │             Control stop surfaces; any other hitch falls back to serial
   │
   └─ Plan::host_ranges / from_summaries for host-built block summaries
```

`Plan::scan` keeps spans in `src` coordinates. The request and demand
projection were compiled at build time, so re-entry allocates only answers.

Next is [Materialize and encode](08-materialize-and-encode.md).
See also [Introduction](README.md) and [Host seam and sharding](03-host-seam-and-sharding.md).
