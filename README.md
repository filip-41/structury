# structury

`structury` provides codecs that work directly on borrowed bytes. You describe what to read, encode, or edit in a request, and the codec performs only that work. It creates no intermediate document unless you ask for one.

## Workspace layout

- [`structury`](core/) is the core that works for any format. It provides `Value`, `Document`, `Demand`, `Answer`, the error types, and the host seam where a format crate connects.
- [`structury-json`](json/) is the JSON codec. It supports RFC 8259 text and adjacent framing, NDJSON, JSON-seq, and a dialect that covers JSONC and a JSON5 subset.

The current release is `0.1.0-alpha.1`, and the API is not frozen, so names and behavior can still change.

## Design

The API follows a few design choices.

- A request describes the work to perform. The unit of a request is a `Demand`, and one pass over the source answers every demand in the request.
- Missing is not null. An absent path answers `Missing`, never `Value::Null`, so code can tell a null value from an absent key.
- A located value is a span into the source, and nothing is copied until you ask for a materialized form.
- `Number` holds the authored digits, so `1.50` and `-0` survive a round trip, and `as_f64` is the one lossy projection.

The codec starts no thread and performs no I/O. A host that owns a thread pool can split a source into ranges with `Plan` and fold the part results with `stitch`.

## Goals

The project aims to cover several formats over one shared model, with JSON as the first codec.

- Planned codec crates cover CSV and TSV, TOML 1.0 and 1.1, YAML 1.2.2, XML 1.0, HTML, CBOR and CBOR-sequence, MessagePack, and INI with Java properties and dotenv.
- Every codec uses the same model. `Demand`, `Answer`, `Value`, and `Document` do not depend on a format, so the same request runs against any codec, and a value read from one format can be written in another.
- Cross-format reads and writes come from composing codecs rather than from a conversion layer. For example, you will be able to read rows from a CSV file and encode them as JSON or TOML.

Only `structury` and `structury-json` exist today. The other formats are goals, not shipped code.

## Using it

Add the two crates:

```toml
[dependencies]
structury = "0.1.0-alpha.1"
structury-json = "0.1.0-alpha.1"
```

Then read a nested value:

```rust
use structury::{Demand, Step};
use structury_json::{Dialect, Form, JsonInput, MaterializeOptions, Materialized,
                     ScanRequest, materialize, scan};

fn main() {
    let src = br#"{"users":[{"id":1},{"id":2}]}"#;
    let demands = [Demand::Path {
        steps: vec![Step::key("users"), Step::index(1), Step::key("id")],
        nested: None,
    }];
    let request = ScanRequest::new(JsonInput::Text, &demands).with_dialect(Dialect::Rfc8259);
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

Read projected rows:

```rust
use structury::{Demand, Path, Value};
use structury_json::{Dialect, Form, JsonInput, MaterializeOptions, Materialized,
                     ScanRequest, materialize, scan};

fn main() {
    let src = br#"{"users":[{"id":1,"name":"a"},{"id":2,"name":"b"}]}"#;
    let demands = [Demand::Project {
        path: Path::key("users"),
        fields: vec!["id".into()],
    }];
    let request = ScanRequest::new(JsonInput::Text, &demands).with_dialect(Dialect::Rfc8259);
    let result = scan(src, &request).expect("valid JSON");
    let materialized = materialize(
        &result.answers[0],
        MaterializeOptions::new(Dialect::Rfc8259, Form::Value),
    )
    .expect("projected rows");
    let Materialized::Value(rows) = materialized else {
        unreachable!("Form::Value yields a value");
    };
    let rows = rows.as_array().expect("projected rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].member("id").and_then(Value::as_i64), Some(2));
}
```

## Examples

Runnable examples live in [`core/examples/`](core/examples) and
[`json/examples/`](json/examples), where [scan.rs](json/examples/scan.rs) scans
a nested demand and reads the located value.

- [values.rs](core/examples/values.rs) builds a small tree, walks it, and shows the two equality notions.
- [predicates.rs](core/examples/predicates.rs) filters rows with `Predicate`.
- [numbers.rs](core/examples/numbers.rs) parses spellings, compares them by value, and projects them to machine numbers.
- [resolve.rs](core/examples/resolve.rs) resolves an index, including a negative index from the end.

## Reading, writing, and editing

The JSON codec exposes functions for each direction of conversion.

- `scan` runs one pass over a request, and `scan_controlled` also polls a `Control`. `scan_each` visits one framed record at a time, and `validate` reports whether the bytes parse under a dialect. `materialize` turns an answer into a `Form`, and `parse` does the same for a whole input.
- `encode` writes a `Value` or a validated `Document` to bytes. A document passes the write gate only when a `Strict` scan validated every value.
- `edit` locates a list of `Edit` values in one `Strict` pass and applies byte splices. When a splice cannot express a change, `edit` falls back to re-encoding a DOM. `edit_document` keeps comment facts and returns a `Document`.
- `Plan` cuts a source into ranges for the host to scan, and `Drive` and `stitch` fold the part results. `ValuePlan` plans a parallel encode of a top-level array.

## What the JSON codec accepts

The `JsonInput` enum holds `Text` for one value, `Adjacent` for back-to-back values, `Ndjson` for newline-delimited values, and `JsonSeq` for RFC 7464 records. The `Dialect` enum holds `Rfc8259` for strict JSON, `Jsonc` for comments and trailing commas, and `Json5` for a JSON5 subset with single-quoted strings and bare keys. The `Demand` enum covers whole documents, paths, collections, slices, projections, and filters, plus the oracles that answer from the scan itself. The strictness setting chooses `Strict`, `Structural`, or `Lazy`, where `Structural` is the default.

## Documentation

- [Introduction](docs/README.md) introduces `structury` and maps the rest of the guide.
- [Getting started](docs/10-getting-started.md) walks from an empty crate to a first program.
- [Performance](docs/11-performance.md) collects the internal benchmark results.
- Crate summaries live in [`core/README.md`](core/README.md) and [`json/README.md`](json/README.md).
- `cargo doc --workspace --no-deps` builds the API reference.

## API stability

Types that a codec matches exhaustively stay without `#[non_exhaustive]`. The set includes `Demand`, `Answer`, `Oracle`, `Edit`, and others. Adding a variant then shows up as a visible breaking change. Request and option structs plus result enums carry `#[non_exhaustive]` and builder methods, so a new setting stays additive. Build them with `new` or `default` and the `with_*` setters instead of struct literals.

## Features and platform

`structury` runs as `no_std` with `alloc`, and the host owns I/O and threads. The `byte-scan` feature compiles the SIMD stop-set kernels. [`structury-json`](json/) enables the feature, and Cargo feature unification means every dependent of the codec compiles those kernels and their `unsafe` with no opt-out, even though `structury` denies `unsafe_code` by default. Document stores use `alloc::sync::Arc`, so a target needs pointer-width atomics, and `Control` also uses 64-bit atomics.

## Development

```sh
make gate     # fmt-check, clippy (native + cross), test, fuzz-check
make miri     # strict-provenance Miri over the byte-scan kernels (slow)
```

`cargo test --workspace` compiles the Rust blocks in this file, so a broken example fails the test suite.

Licensed under either of MIT or Apache-2.0, at your option.
