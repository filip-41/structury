# Host seam and sharding

The page covers the boundary between a codec and the outside world. The host
owns I/O, threads, and time, while the codec supplies a plan and a fold that
behaves the same on every run. It explains `Drive`, `stitch`, the `Shard` law,
and where `Control` is polled.

The core-side pieces are `core/src/drive.rs`, `core/src/stitch.rs`, and the
`Shard` type in `core/src/demand.rs`.

## No thread starts in the codec

`structury` runs as `no_std` with `alloc`. It does not spawn threads, open
files, or read clocks on its own. A scan is a pure function from bytes and a
request to a `ScanResult`, unless the caller passes a `Control` with a `now`
function that the codec may call at a boundary. Parallelism is expressed as
data with a list of `ByteRange` parts, and the host runs those parts. Only the
host knows whether it has a thread pool, an async runtime, or a single core.

The `Demand::shard` method exists for that reason. It answers whether ranges
can run independently and how partial answers combine, so the host asks instead
of the codec guessing.

## The Shard law

`Shard` in `core/src/demand.rs` has three values.

```rust
pub enum Shard {
    Serial,   // one range observes the whole node
    Concat,   // per-range answers concatenate in document order
    Sum,      // per-range answers are partial totals to add
}
```

`Demand::shard` returns the law for one demand. It is a whitelist that was
proved correct, not a guess. Only shapes that fold correctly are parallel, and
everything else is `Serial`, which stays correct and runs without parallel
support.

```text
Project / Filter with an all-key path          -> Concat
Collection { nested: None, .. }                -> Concat
Collection { fields: None, nested: Some(...) } -> Concat if the nested is a
                                                  root Project/Filter, else Serial
Path { key-only steps, nested: Count }         -> Sum
Oracle(Count) at the root                      -> Sum
anything else                                  -> Serial
```

The conditions rest on `key_steps`, which requires every step to be a `Key`,
and on checking for a spent `Path{[]}` wrapper. A `Slice` is never parallel
because a range depends on the total length. `Shard::is_parallel` means not
`Serial`, and `Drive::eligible` requires every demand in a request to be
parallel.

## Drive with parts and serial fallback

`Drive` in `core/src/drive.rs` owns the ordered part ranges.

```rust,ignore
pub struct Drive { parts: Vec<ByteRange> }
```

It is built from `Vec<ByteRange>`. `Drive::eligible(demands)` is the structural
half of the Shard law. `Drive::run` takes two closures, one that scans a single
part and one that scans the whole document serially, and runs the following
protocol.

```text
for part in parts:
    match scan_part(part):
        Ok(result)                       -> collect
        Err(e) if e.class == Control      -> return Err(e)   // the host's own stop
        Err(_)                           -> return serial() // hitch: fall back
stitch(collected)
```

A control stop is special. It means the host cancelled, so it surfaces instead
of falling back. Any other part failure is treated as a hitch, perhaps because
the cut guess was wrong for the input, and the whole request runs again
serially. Correctness never depends on the cut being right.

## Stitch as one fold that works for any format

`stitch` in `core/src/stitch.rs` takes the ordered per-part `ScanResult` values
and combines them answer by answer. The parts share one plan, so all parts have
the same number of demands, and a part that is shorter than the first
contributes `Answer::Missing` in the missing slots. Folding one answer index
across parts works as follows.

- `Columns` batches merge with `append_rows` into one batch in document order,
  following the `Concat` law.
- `Oracle::Count` values add with saturating arithmetic, following the `Sum`
  law.
- A `Document` is pushed as a cell into a `Columns` batch with the synthetic
  `"$"` field, so a whole-element projection keeps row order.
- Anything else keeps the first answer that is not a `Columns` batch or count.

Two details matter for safety. First, `stitch` asserts that every part indexes
the same source bytes with `same_source`, because an answer must not apply to
different bytes. Second, an empty part list yields an empty result, and a single
part returns unchanged, which keeps the common serial case fast.

```text
Plan { ranges: [A][B][C] }
   host scans A ─► answers[a0 a1 a2]
   host scans B ─► answers[b0 b1 b2]
   host scans C ─► answers[c0 c1 c2]
                       │
        stitch per index│  (Concat / Sum)
                       ▼
              answers[Aa Bb Cc , ...]
```

## Where Control is polled

A controlled scan runs with `CONTROLLED = true`, which is a const generic on
the walk in `json/src/walk/mod.rs`. The poll in `check_control` runs at loop
heads, once per direct child of a container and once per framed stream record,
namely at the top of an array element loop, at the top of an object member
loop, and at the start of each record. It never runs per byte, so the cost on
the hot path is a load and a compare per child or record. When `CONTROLLED` is
false the poll and the `Control` borrow fold away entirely.

A fused `Count` oracle polls once per counted child, while a fused
`DescendCount` checks once at entry and then walks its subtree without polling.
A host that fans a plan out can call `Plan::scan_controlled` per morsel in
`json/src/shard.rs`, so a part reports its own stop to `Drive`.

The three stop conditions run in order.

1. `stop` is non-zero, meaning the host cancelled.
2. `deadline` is set and `now()` has reached or passed the deadline.
3. `used` has reached or passed `ceiling`, meaning live memory measured by the
   host hit the ceiling.

Each condition becomes a distinct `ErrorClass::Control` code with values
`cancelled`, `deadline-exceeded`, and `memory-exceeded` at the boundary offset.
There is no partial answer, because a control stop is a refusal.

Next is [Framing and dialects](04-framing-and-dialects.md).
See also [Introduction](README.md) and [Core concepts](02-core-concepts.md).
