# Framing and dialects

A codec reads a buffer, and two choices decide how the read works. Framing says
how one or more values are arranged in the buffer, and a dialect names the
grammar the bytes follow. Both choices belong to the format crate, because both
are part of what the format means. The core only records the grammar a codec
validated with, as an opaque `GrammarTag` on a validated `Document`, and the
byte-preserving write gate compares the tag.

The JSON codec is the first to define the two choices, through `JsonInput` and
`Dialect`. A second codec would define its own pair: one enum for the
arrangements of values and one for the grammars.

## Framing

A buffer holds one value, a run of adjacent values, or a stream of records. The
arrangement decides where one value ends and the next begins, so the codec owns
the boundary law.

For JSON, `JsonInput` in `json/src/scan.rs` is the arrangement.

```rust
pub enum JsonInput {
    Text,       // one complete JSON value (trailing RFC whitespace allowed)
    Adjacent,   // back-to-back JSON values; leftover after one is the next
    Ndjson,     // newline-delimited JSON values
    JsonSeq,    // RFC 7464 JSON text sequences (0x1E record separator)
}
```

`Text` is the ordinary case with exactly one value and then trivia to end of
file. The other three are streams, and the walk treats streams as a virtual
array of top-level values. The `framing` module owns their boundaries.

The `adjacent_values` function scans values back to back under `Check::Locate`
and records each span. The `ndjson_frame_end` function ends a record at `\n`,
`\r\n`, or end of file, where `NdjsonFrame` is the stop set of `\n` and `\r`,
and a line with only whitespace produces no frame. The `json_seq_ranges`
function handles RFC 7464 with an optional leading `0x1E` record separator and
then the payload of the record up to the next separator, where `JsonSeqFrame`
is the stop set for `0x1E`.

```text
Ndjson:   {"a":1}\n{"a":2}\n          → frames [{"a":1}], [{"a":2}]
JsonSeq:  \x1e{"a":1}\n\x1e{"a":2}\n  → payloads around each \x1e
Adjacent: {"a":1}{"a":2}              → two spans, no separator
```

For the shard path, the framing module also exposes three partition functions,
namely `partition_ndjson`, `partition_adjacent`, and `partition_json_seq`. The
functions pack whole values into morsels of at least a target byte size without
splitting a record. The shared packer is `pack_runs`. The functions are covered
in [Sharding](07-sharding.md).

One consequence for `Text` needs care. Adjacent, NDJSON, and JSON-seq parse a
stream of values, so a key-scoped path on a stream is a type error because the
virtual array has no keyed member. Trailing data in `Text` is `json.syntax`
trailing-content, while in a stream the next value is simply the next value.

## Dialects

A dialect names the grammar a codec reads. One codec can serve several dialects,
and the caller picks one per request. The core never interprets a dialect, so a
codec records the chosen grammar as an opaque `GrammarTag` on a document, and a
byte-preserving write refuses a document validated under another grammar. Later
codecs will use the same idea for the TOML versions, the YAML schemas, and other
grammar variants.

For JSON, `Dialect` in `json/src/dialect.rs` selects the grammar the lexer and
walk read.

```rust
pub enum Dialect {
    Rfc8259,   // strict RFC 8259: no comments, no trailing commas
    Jsonc,     // RFC 8259 + // and /* */ comments and trailing commas
    Json5,     // JSONC + single-quoted strings, bare keys, hex/signed numbers
}
```

The deltas are exactly as follows.

| Feature | Rfc8259 | Jsonc | Json5 |
|---|---|---|---|
| `//` and `/* */` comments | no | yes | yes |
| trailing comma before a closer | no | yes | yes |
| single-quoted strings | no | no | yes |
| bare identifier keys | no | no | yes |
| `+` and leading dot and hex and signed numbers | no | no | yes |
| `Infinity` and `NaN` as non-finite values | no | no | yes |

Three small predicates encode most of the behavior, namely `has_comments` where
comments are trivia, `trailing_commas` where a comma before `}` or `]` is
allowed, and `json5` for JSON5 scalars and bare keys. The lexer branches on the
predicates, while the RFC path is a dense byte-class table in
`json/src/lex/skip.rs` and the dialect path is the general trivia and string
and key walk.

Numbers are the one place where value semantics differ. A JSON5 `Infinity`,
`-Infinity`, or `NaN` has no exact base-ten value, so the codec classifies the
value with `non_finite_value` in `json/src/lex/number.rs` as
`Number::NonFinite(NonFinite)` in `core/src/number.rs`, while the core itself
parses only base-ten spellings. The `encode` function refuses to write a
non-finite number outside `Dialect::Json5`, as described in
[Materialize and encode](08-materialize-and-encode.md). Hex and signed JSON5
numbers are normalized to exact base-ten spelling when the tape is built, with
`normalize_json5_into` in `json/src/lex/number.rs` called from
`json/src/tape.rs`.

`Dialect` is `#[non_exhaustive]`, so a new grammar is a minor release and an
external `match` keeps a wildcard arm.

## Comments as facts

A commenting dialect can attach comments to nodes, and `structury` models them
as facts. A `Fact` is optional metadata on a node, described in
[Core concepts](02-core-concepts.md), and a comment never becomes parsed text.

The JSON codec has two collectors in `json/src/facts.rs`.

- `collect` is the standalone oracle. It walks trivia linearly and assigns each
  comment to neighboring nodes, and it returns facts in ascending glyph order.
  It runs when facts are requested but the fused walk is not used.
- `Recorder` is a `RecordSink` driven by the validating walk itself. The walk
  is monomorphized over the sink. `NoRec` is the facts-off instantiation where
  every operation is a no-op and the branch disappears, while `Recorder`
  records each comment as trivia is skipped and uses the node spans of the
  walk.

The role of a fact depends on position. A comment on the same line after a
value is a `CommentFoot` of that value, a comment before a value is a
`CommentLead` of the value that follows, and an interstitial comment can be
both and so produces two records. A comment with no sibling position is
`CommentInline`. The glyph span holds the authored comment bytes and is
retained only on the `Strict` path that supports editing. On `Structural` and
`Lazy` the fact still carries its decoded text but `source_span` is `None`, so
a canonical encode renders `// text` instead of the original glyph.

`ScanRequest::facts` turns collection on. The `edit_document` function implies
collection, while the `edit` function does not imply collection unless an
`Edit::Fact` needs the existing comments. The fused path with `fuses_facts` in
`json/src/scan.rs` runs when facts are on and the dialect has comments and the
request has a `Whole` demand, and the single pass then saves a second pass.

Next is [Edit and facts](05-edit-and-facts.md).
See also [Introduction](README.md) and [Scan](06-scan.md).
