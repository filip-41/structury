# structury-json

The JSON codec of the structury family. It plugs into the core and answers the
same `Demand` values for JSON bytes, so a request written for one codec carries
to the others. This crate owns every JSON-specific decision.

## What this crate is for

A JSON request is a slice of `Demand` values, and one pass over the source
answers each demand. The crate reads four arrangements of values and three
grammars, and it writes JSON back behind a validation gate.

## Main responsibilities

### Framing and dialects

`JsonInput` says how values are arranged in a buffer: one `Text` value,
`Adjacent` values, newline-delimited `Ndjson`, or RFC 7464 `JsonSeq` records.
`Dialect` says which grammar the bytes follow: `Rfc8259`, `Jsonc` with comments
and trailing commas, or `Json5` with a subset of JSON5. The codec records the
grammar on a validated document as an opaque `GrammarTag`, and a byte-preserving
write refuses a document validated under another grammar.

### The scan

The codec walks the source once. Each demand is matched against the structure
the walk must read anyway, and each demand gets its own answer. Unread regions
are located without value checks under the default `Structural` setting, and
`Lazy` leaves value checks to `materialize`. `Strict` validates every value and
is required for a byte-preserving write. A stream is a virtual array of
top-level values, and `scan_each` visits one record at a time.

### Oracles and predicates

An `Oracle` answers a property during the walk, such as a count, a kind, member
names, or a string length, without building a value. `Predicate` is the small
expression language behind filters.

### Materialize and encode

`materialize` turns an answer into a `BorrowedDocument`, an `OwnedDocument`, or
a `Value`, selected by `Form`. `encode` writes a `Value` with the canonical
walk, or a validated `Document` byte for byte when the grammar matches the
requested dialect. The canonical walk also re-emits retained comment facts.

### Edit and facts

`edit` locates a list of `Edit` values in one `Strict` pass and applies byte
splices, so the bytes outside the changed region stay untouched. `edit_document`
keeps the comment facts of the input and remaps them through the edits. When a
splice cannot express a change, the codec falls back to a DOM re-encode.

### Sharding

`Plan` cuts the source into independent ranges for the host to scan, and the
core's `Drive` and `stitch` fold the per-range results. The codec starts no
thread, so the host owns the pool.

## Using it

```rust
use structury::{Demand, Step};
use structury_json::{Dialect, Form, JsonInput, MaterializeOptions, Materialized, ScanRequest, materialize, scan};

fn main() {
    let src = br#"{"users":[{"id":1,"name":"a"},{"id":2,"name":"b"}]}"#;
    let demands = [Demand::Path {
        steps: vec![Step::key("users"), Step::index(1), Step::key("id")],
        nested: None,
    }];
    let request = ScanRequest::new(JsonInput::Text, &demands);
    let result = scan(src, &request).expect("valid JSON");
    let materialized = materialize(
        &result.answers[0],
        MaterializeOptions::new(Dialect::Rfc8259, Form::Value),
    )
    .expect("located value");
    let Materialized::Value(value) = materialized else {
        unreachable!("Form::Value yields a value");
    };
    assert_eq!(value.as_i64(), Some(2));
}
```

Runnable examples live in [`examples/`](examples), and the
[guide](https://github.com/filip-41/structury/blob/main/docs/README.md)
explains the scan, the dialects, the write path, and the edit path in longer
prose.

## Examples

- [scan.rs](examples/scan.rs) scans a nested demand and reads the located value.

## Platform

`no_std` with `alloc`. The codec starts no thread and performs no I/O, so the
host supplies bytes and time. Document stores use `alloc::sync::Arc`, so a
target needs pointer-width atomics.

## Feature `byte-scan`

This crate enables `structury/byte-scan`, so a dependent compiles the SIMD
stop-set kernels and their `unsafe` even though the core denies `unsafe_code` by
default. There is no opt-out today.

## API stability

The current release is `0.1.0-alpha.1`, so nothing is frozen. Codec-coupled
enums such as `Edit` and `FactOp` stay without `#[non_exhaustive]`, so adding a
variant is a visible breaking change. Request and option structs and result
enums carry `#[non_exhaustive]` and builder methods, so a new setting stays
additive.

## License

MIT or Apache-2.0, at your option. The license files live at the repository
root.
