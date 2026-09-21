# Materialize and encode

An answer is not a value yet. The read side turns an `Answer` into an artifact,
either a document or a `Value`, and the write side turns an artifact back into
bytes. Both directions are codec-owned, because the codec knows its grammar and
its payloads, while the core owns the answer, the document, and the value types.

The JSON codec is the first implementation. Its code lives in
`json/src/materialize.rs`, `json/src/encode.rs`, `json/src/tape.rs`, and
`json/src/borrowed.rs`.

## Materialize an answer

Materialization follows the demand shape. A path answer becomes one value, a
projection becomes a batch of rows, and an oracle has no value to materialize.
The codec reads the bytes from the answer itself, because a `Document` binds its
source and a `Columns` batch binds the bytes that its spans index, so a span
cannot be read against the wrong buffer.

A form decides how much the read copies. The JSON codec's `Form` has three
values.

```rust
pub enum Form {
    Borrowed,   // default: zero-copy source-backed arena
    Owned,      // arena whose payloads are copied into document-owned storage
    Value,      // owned mutable tree, produced from the borrowed view
}
```

- `Borrowed` builds a `BorrowedDocument` through a `BorrowedPayload` strategy
  in `json/src/borrowed.rs`. A payload that is a contiguous source slice is
  stored as an offset into the logical join of `source` and `spill`, so a plain
  RFC 8259 document copies no payload byte. Escaped strings, decoded keys, and
  JSON5-normalized numbers spill.
- `Owned` builds an `OwnedDocument` and copies every payload into one `data`
  buffer with `OwnedPayload` in `json/src/tape.rs`.
- `Value` builds the borrowed view and then calls `to_value`, which produces
  the recursive `Value`.

`Materialized` is the result, and `into_value` consumes any form as a `Value`.

```rust,ignore
pub enum Materialized<'src> {
    Borrowed(BorrowedDocument<'src>),
    Owned(OwnedDocument),
    Value(Value),
}
```

An `Oracle`, `Missing`, or `TypeMismatch` answer is a shape error, because
there is no value to materialize. Those answers mean the question was answered
another way or there was no value.

The `materialize(answer, opts)` function in `json/src/materialize.rs` takes an
`Answer` and a `MaterializeOptions` with `dialect` and `form`. The doc comment
carries a `compile_fail` example that shows passing an unrelated buffer does not
compile. The `parse(src, opts)` function is the convenience wrapper for a single
text value. It wraps the whole input in a span `Document` and materializes the
document, so it always checks values.

### The tape builder and deferred checks

All forms go through one `Builder` in `json/src/tape.rs` parameterized by a
`Payload` strategy. The builder parses the located span in one pass with no
per-value heap allocation. It pushes `Node` values into a flat node arena while
it pushes child ids into `edges`, and it resolves objects with a Bloom-filter
gate that makes duplicate-key collapse with the last value winning low cost. It
sizes the arenas once from a density sample after 256 nodes with `SIZE_SAMPLE`,
so a dense document is not copied again by geometric growth and a sparse
projection does not reserve the whole input.

The builder always parses with `Check::Values`, and the deferred `Lazy` value
checks run there. The scan located the span under `Check::Locate`, and
materialize validates the bytes it builds. A number with no exact
representation, such as a JSON5 non-finite value, or invalid bytes becomes an
error here and not at scan time.

## Encode an artifact

The write side turns an artifact back into bytes. A codec can write from a tree
it holds or from a document that a strict pass already validated, and the source
decides which. The JSON codec's `encode(source, opts, out)` function in
`json/src/encode.rs` takes a `Source`.

```rust,ignore
pub enum Source<'a, 'src> {
    Value(&'a Value),                 // canonical walk
    Document(&'a Document<'src>),     // refused unless fully validated
}
```

### The write gate

A document is written only when a `Strict` pass checked every value. The JSON
codec refuses a `Source::Document` with `ErrorClass::Write` unless
`doc.is_fully_validated()` is true. A document located by a `Structural` or
`Lazy` scan can hold bytes that were never checked, so writing such a document
could produce invalid JSON. Only a `Strict` scan produces a validated document
with `Document::from_span_validated`, and only a sibling codec may call that
constructor.

A write that keeps bytes unchanged also requires the grammar of the document to
match the requested dialect. A `Strict` pass records its dialect as an opaque
`GrammarTag` on the document, and `encode` refuses a mismatch with a
`dialect-mismatch` write error instead of copying bytes that the requested
grammar does not admit, since a JSONC document copied under RFC 8259 options
would keep its comments. Pass `verbatim(false)` to ask for the canonical
rewrite instead.

### Verbatim and canonical writes

Once through the gate, `write_document` has two modes.

- Verbatim is the default when pretty printing is off, and it runs
  `out.extend_from_slice(doc.root_bytes())`. Because a `Strict` pass validated
  the whole span, copying is safe and exact, and it preserves formatting, key
  order, and authored number spellings byte for byte.
- Canonical walk runs when pretty printing is on, and `emit_span` walks the
  span and emits the value and any retained comment facts at their positions.
  The mode re-emits comments, and it also rewrites insignificant whitespace and
  pretty indentation.

`Source::Value` always takes the canonical walk with `write_value`, which
writes the compact or pretty form of the tree. A JSON5 non-finite number is
refused unless `opts.dialect` is Json5. Both the value walk and the span
emitter refuse past `MAX_NESTING` with a `nesting` limit error, so no output
path recurses without a bound.

### Parallel value encode

A top-level array value also encodes in parallel without changing bytes.
`plan_encode_value` in `json/src/encode_parallel.rs` cuts the array into
`ItemRange` chunks, the host encodes each chunk with `encode_value_chunk` on
its own thread, and `stitch_value_chunks` joins the parts with the separators
and brackets. The plan falls back to `ValuePlan::Serial` for a non-array, a
framed write, or fewer than `MIN_ITEMS_PER_PART` items per part, so a small
write never pays for fan-out. The [overview](01-overview.md) shows the host
loop.

### Options

`EncodeOptions` in `json/src/encode.rs` holds the settings.

- `pretty` and `indent` with `Indent::Spaces(u8)` or `Tab` choose formatting.
- `framing` with `ItemFraming::None`, `NdjsonLf`, or `JsonSeq` writes a
  terminator, or a `0x1E` prefix plus `\n` for JSON-seq.
- `verbatim` controls the memcpy path for a `Document`.
- `sort_keys` sorts object members by key on a value write, stably so duplicate
  keys keep their authored order.
- `ascii` escapes non-ASCII as `\uXXXX` with surrogate pairs past the BMP on a
  value write.
- `dialect` selects the grammar that the canonical walk reads and whether an
  authored glyph is re-emitted, and it also gates the verbatim path, which
  refuses a document validated under another dialect.

The convenience constructors `EncodeOptions::compact()` for verbatim output
with no framing in RFC 8259, which is also `Default`, and
`EncodeOptions::pretty()` cover the common cases, with `with_*` builders for
the rest.

### Fact-aware writing

A canonical walk can re-emit the comment facts that a document carries. The
JSON codec keeps a `FactCursor` that emits facts in glyph order as the walk
passes their positions. A fact without a glyph is rendered as `// text` plus a
newline, and a retained `//` line comment gets a terminating newline before the
next token so the following token is not swallowed. Facts with a glyph outside
the emitted span are skipped, and a dual foot and lead record emits once. The
mechanism lets an edit preserve comments while it changes a value.

Next is [Stability and internals](09-stability-and-internals.md).
See also [Introduction](README.md) and [Edit and facts](05-edit-and-facts.md).
