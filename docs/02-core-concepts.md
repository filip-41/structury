# Core concepts

The page covers the concepts that every format codec shares. It describes what a
request asks for, how the answers line up with it, and the shapes that data
takes in the core.

The behavior lives across `core/src/`, in the modules for demands, scans,
values, documents, arenas, and control.

## Demands are data

A request is a slice of demands, built from the enum in `core/src/demand.rs`.

```rust,ignore
pub enum Demand {
    Whole,
    Path     { steps: Vec<Step>, nested: Option<Box<Demand>> },
    Collection { fields: Option<Vec<Name>>, nested: Option<Box<Demand>> },
    Slice    { range: Range, nested: Option<Box<Demand>> },
    Project  { path: Path, fields: Vec<Name> },
    Filter   { path: Path, predicate: Predicate, project: Vec<Name> },
    Oracle(Oracle),
}
```

The variants group by the kind of question they ask.

- `Whole` and `Path` locate one node. `Whole` is the entire document value, and
  `Path` follows a static key and index chain, then optionally asks a `nested`
  demand at the located node.
- `Collection` and `Slice` read part of a container. `Collection` treats the
  node as an array and keeps `fields` per object element, where `None` keeps
  every member, and it can apply a per-element `nested` demand. `Slice` keeps
  the array elements in `range`, optionally with a `nested` demand.
- `Project` and `Filter` read rows. `Project` navigates `path` to an array of
  objects and keeps `fields` per row. `Filter` keeps only rows matching a
  `Predicate`, projected to `project`, where an empty list keeps the whole row.
- `Oracle` computes a property such as count, kind, member names, or string
  length without building a value.

A `Step` is a `Key(String)` or an `Index(i64)`, and a negative index counts from
the end through `Step::index` and `resolve_index` in `core/src/value.rs`. `Path`
wraps a `Vec<Step>` and carries the `push`, `push_key`, and `push_index`
builders. `Range { start: Option<i64>, end: Option<i64> }` is a half-open
incoming window, and `Range::window(len)` resolves the window with clamping and
with support for negative bounds.

A demand is plain data, not a callback, so code can clone it, compare it, and
plan over it before any byte is read. Overlapping demands stay separate. Two
demands on the same path each get their own answer, and the walk does not merge
them at request time.

## One answer per demand

One pass yields a `ScanResult` as defined in `core/src/scan.rs`.

```rust,ignore
pub struct ScanResult<'src> {
    pub answers: Vec<Answer<'src>>,   // answers.len() == demands.len()
    pub issues:  Vec<Issue>,          // recovering diagnostics
}
```

The `Answer` enum is the outcome vocabulary.

```rust,ignore
pub enum Answer<'src> {
    Document(Document<'src>),
    Columns(Columns<'src>),
    Oracle(OracleAnswer),
    Missing,
    TypeMismatch { actual: ValueKind },
}
```

`Document` is the one value handle for a located span, `Columns` is a projected
batch stored row by row, `OracleAnswer` holds the computed property, `Missing`
means the path was absent, and `TypeMismatch` means an intermediate step met the
wrong kind and carries only the `ValueKind` that was found. The failing path
step is not repeated because the answer index points at the demand, and the
static path in the demand names the step.

The zip is strict. `answers[i]` is always the answer to `demands[i]`, and every
entry point documents and debug-asserts the rule. The rule keeps `stitch` and
`scan_each` well defined.

```rust
use structury::{Answer, Demand, Oracle, OracleAnswer, Step};
use structury_json::{JsonInput, ScanRequest, scan};

let src = br#"{"users":[{"id":1},{"id":2}]}"#;
let demands = [
    Demand::Path {
        steps: vec![Step::key("users"), Step::index(1), Step::key("id")],
        nested: None,
    },
    Demand::Path {
        steps: vec![Step::key("users")],
        nested: Some(Box::new(Demand::Oracle(Oracle::Count))),
    },
];
let result = scan(src, &ScanRequest::new(JsonInput::Text, &demands)).expect("valid JSON");
let Answer::Document(document) = &result.answers[0] else {
    unreachable!("a path answers a document");
};
assert_eq!(document.root_bytes(), b"2".as_slice());
let Answer::Oracle(OracleAnswer::Count(2)) = &result.answers[1] else {
    unreachable!("a count oracle answers a count");
};
```

## Strictness chooses the check level

`Strictness` in `core/src/demand.rs` controls how much value checking the walk
performs, without changing which structure the walk reads.

```rust
pub enum Strictness {
    Strict,      // structure + all values, document order
    Structural,  // structure + demanded values, demand order   (default)
    Lazy,        // structure only; value checks deferred
}
```

- `Strict` validates every value in the source. It is required for writes,
  because `Document::is_fully_validated` is true only for a `Strict` result,
  and `encode(Source::Document)` refuses anything else.
- `Structural` is the default. It locates the demanded values and checks those
  values, but it does not check values in unread regions. The walk uses the
  `Check::Locate` setting for unread structure and `Check::Values` for demanded
  structure, as seen in `Walker::unread` and `Walker::demanded` in
  `json/src/walk/mod.rs`.
- `Lazy` reads structure only and leaves value checks to `materialize`.

Structure is never skipped under any setting, because the grammar must be
understood to find the next token. What changes is whether the value of a token
is validated in place.

## A located value is a span

The located value of the core is a `ByteRange` in `core/src/scan.rs`. A
`ByteRange` is a half-open `[start, end)` pair with `start <= end`, constructed
through the checked `ByteRange::try_new`. `Document<'src>` in
`core/src/document.rs` is the faithful alternative to a tree.

```rust,ignore
pub struct Document<'src> {
    source: &'src [u8],
    root: ByteRange,
    fully_validated: bool,
    grammar: GrammarTag,
    facts: Vec<Fact<'src>>,
}
```

It holds the bytes, the root span, whether a `Strict` pass checked every value,
the grammar provenance that the codec recorded as an opaque `GrammarTag` that
the core never interprets, and any attached facts. The `root_bytes` method
slices the source, and `is_fully_validated` is the write gate. The two
constructors have names that keep the gate from flipping by accident.
`from_span` records not validated, while `from_span_validated` records the
`Strict` result of a codec and is `#[doc(hidden)]` because only a sibling codec
may call it.

Because a span is only meaningful against the bytes it indexes, the core binds
each batch to its source and refuses to mix sources. `Columns::append_rows` and
`Columns::absorb` call `assert_same_source` in `core/src/document.rs`, and the
check compares pointer identity, not content. `stitch` makes the same check when
it folds per-shard answers. A span that crossed sources would read the wrong
bytes, so the API treats mixing as a programming error instead of a recoverable
one.

## Facts

A `Fact` is optional metadata attached to a node. In JSON a fact is a comment,
while in another format a fact might be whitespace or an annotation. Facts
cannot change the meaning of a node. `FactRole` in `core/src/document.rs` has
three values.

```rust
pub enum FactRole {
    CommentLead,    // trivia before a node, attached to the node that follows
    CommentFoot,    // trivia after a node on its last line, attached to it
    CommentInline,  // trivia with no sibling position
}
```

`FactOwner` names the node that a fact attaches to by its source span, because a
document has no node table and the span serves as identity. `Fact` stores a
role, the comment text as a `Cow<'src, str>` that borrows valid UTF-8 in the
common case, an optional authored glyph span, and the owner. A fact with no
glyph span still renders as `// text` when a canonical walk encodes it.
`set_facts` requires glyph spans in ascending order, and disordered input is
refused with `ErrorClass::Write`.

## Columns are projected rows

`Columns<'src>` in `core/src/document.rs` is the answer to a `Project` or
`Filter` request. It is a batch of projected cells stored row by row.

```text
fields = ["id", "name"]           width = 2
cells  = [ Span, Span, Absent, Span, Span, Span ]   // 3 rows
           row0  row0   row1   row1  row2  row2
```

Each cell is a `ColumnCell`.

```rust,ignore
pub enum ColumnCell {
    Span(ByteRange),   // present value, as a span into the source
    Absent,            // key missing, not Value::Null
}
```

The cells and field names sit behind `Arc`, so cloning a batch for a second
demand shares storage. The `push` and `reserve` methods build a batch, while
`append_rows` and `absorb` merge two batches that name the same source, and
`retain_rows` keeps a window. `rows` equals `cells.len() / width()`. A synthetic
field named `"$"` means "the whole value" for a projection that keeps the
element itself.

## Oracles and predicates

An `Oracle` in `core/src/demand.rs` asks a property question that the walk can
answer while it is already at the node, without materializing the node.

```rust,ignore
pub enum Oracle {
    Count,                          // last-wins member or child count
    DescendCount,                   // preorder count of self + descendants
    Kind,                           // ValueKind of the located node
    HasKey { key: Name },           // member presence after last-wins
    MemberNames,                    // first-key, last-wins member names
    MemberCount,                    // last-wins member count
    StringByteLength,               // byte length of a located string
}
```

An oracle answer is `OracleAnswer` with `Count`, `Kind`, `HasKey`,
`MemberNames`, or `StringByteLength`. `DescendCount` is the only oracle that
walks the whole subtree, while `Count` counts direct children.

`Predicate` is a related vocabulary with a small boolean expression over object
fields, using `Eq`, `Ne`, `Gt`, `Lt`, `Ge`, `Le`, `And`, `Or`, and `Not`. Its
`matches(&Value)` compares numbers by mathematical value, compares objects by
member name ignoring order, and compares everything else structurally. The JSON
walk answers filters from source spans, so an equality operand must be a scalar;
a container operand is refused with `ErrorClass::Shape` instead of silently
never matching. Ordering operands compare numbers by value.

## Value is the semantic tree

`Value` in `core/src/value.rs` is the familiar recursive tree.

```rust,ignore
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    Str(CompactStr),
    Array(Vec<Value>),
    Object(Vec<(CompactStr, Value)>),
}
```

It has no `f64` arm. Numbers stay exact as a `Number` with an integer spelling,
a decimal, or a non-finite value, and a grammar that admits non-finite numbers
classifies those values while the core only carries them. Object members stay in
order of first appearance with later values replacing earlier ones, so duplicate
keys collapse the way JSON semantics say, and `Value::member` searches in
reverse. `Value::equal` is the semantic notion. Numbers compare by value and
objects compare by member name ignoring order, because RFC 8259 objects are
unordered, so `{"a":1,"b":2}` equals `{"b":2,"a":1}`, while
`Value::strict_equal` preserves member order and authored spelling.
`CompactStr` in `core/src/compact.rs` is an owned UTF-8 string that keeps up to
22 bytes inline and spills to the heap past that length, so small keys and
strings avoid an allocation.

`Value` is what a request for `Form::Value` returns, and it is a good output.
It is not the default, because building the tree copies and allocates where a
span would not.

## The arena documents

When a scan must materialize a tree, the core offers two documents over a shared
navigation layer in `core/src/arena.rs`, `core/src/owned.rs`, and
`core/src/borrowed.rs`.

- `OwnedDocument` holds `nodes`, `edges`, and a `data: String` payload buffer.
  Every payload is copied into `data`.
- `BorrowedDocument<'src>` holds `source`, `nodes`, `edges`, and a `spill:
  String`. Payload offsets address the logical buffer of `source` followed by
  `spill`, so a plain source slice is stored as an offset and copies nothing.

Both implement the sealed `Arena` trait with `node`, `edges`, `payload`, and
`payload_str`, and both expose their values as the same `ArenaValue<'a, A>`
read view with `OwnedValue` and `BorrowedValue`. A `Node` is one fixed-size enum
entry.

```rust,ignore
pub enum Node {
    Null,
    Bool(bool),
    Number    { off: u32, len: u32 },                    // spelling at payload[off..off+len]
    NonFinite { off: u32, len: u32, value: NonFinite },  // classified non-finite; payload is the codec's spelling
    Str       { off: u32, len: u32 },                    // decoded bytes at payload[off..off+len]
    Array     { edge: u32, len: u32 },                   // len child ids at edges[edge..]
    Object    { edge: u32, len: u32 },                   // len (key,value) pairs at edges[edge..]
}
```

`ArenaValue` is the public navigation surface with `kind`, `as_str`, `to_i64`,
`number`, `len`, `elements`, `members`, `member`, `element`, `to_value`, and
`detach`. `detach` copies only one subtree into a fresh owned document, so a
selection from a huge source never retains the whole buffer. Because arena
offsets are 32-bit, a detached payload past 4 GiB is a bounded refusal with an
empty document instead of a panic. A malformed arena with an out-of-range or
straddling payload, invalid UTF-8, or a cycle is refused the same way, while an
out-of-range edge or cycle reads as `null` in `to_value`. Scan paths already
refuse the equivalent overflow when they build the tape.

The `Arena` trait is sealed by a private supertrait, so only the two documents
implement it. `Node`, the `from_parts` constructors, and `ArenaValue::new` are
the construction seam of a codec and are `#[doc(hidden)]`, as described in
[Stability and internals](09-stability-and-internals.md).

## Errors and issues

A codec has two diagnostic channels as defined in `core/src/error.rs`.

- `Error` is fatal and holds a neutral `ErrorClass`, a `u32` byte offset, a
  stable machine `code`, and a human `message`. `ErrorClass` is `Syntax`,
  `Number`, `Utf8`, `Escape`, `Shape`, `Limit`, `Write`, or `Control`. There is
  no I/O class because the host owns I/O.
- `Issue` is the recovering channel, for example for a truncated trailing
  stream record. `ScanResult::issues` carries issues, and anything that could
  not be recovered becomes an `Error` instead.

Hot-path code raises a compact `SmallErr` internally and materializes the
public `Error` only on the way out in `json/src/error.rs`, so the fast path
avoids formatting and string work.

## Control

`Control` in `core/src/control.rs` is the handle that a host holds into a long
scan.

```rust,ignore
pub struct Control {
    pub stop: AtomicU8,          // 0 keeps going
    pub used: AtomicU64,         // bytes the host measured as live
    pub ceiling: u64,            // memory ceiling; u64::MAX is unlimited
    pub deadline: Option<u64>,   // host-clock deadline
    pub now: fn() -> u64,        // the host clock
}
```

The codec only reads the handle at container and record boundaries, never per
token, as seen in `check_control` in `json/src/walk/mod.rs`. A cancelled,
expired, or over-ceiling request becomes an `ErrorClass::Control` error with no
partial answer. The zero-control constant `NO_CONTROL` serves the uncontrolled
entry points, and the `CONTROLLED` const generic folds every poll away when it
is false.

## The vocabulary is format-neutral

`Demand`, `Answer`, `Oracle`, `Range`, `Predicate`, and `Shard` describe
questions and outcomes, not syntax. A JSON document and, for example, a TOML or
CBOR document can share all of them, with meanings such as "locate this path"
or "give me these fields" or "count this collection". The core owns the fold
with `Drive` and `stitch` because folding partial answers is also independent
of format. The only JSON-adjacent knowledge in the core is the `byte_scan`
stop-set kernel, and the kernel is generic over a `StopSet` descriptor and lives
behind a feature flag, as described in
[Stability and internals](09-stability-and-internals.md).

Next is [Host seam and sharding](03-host-seam-and-sharding.md).
See also [Introduction](README.md) and [Scan](06-scan.md).
