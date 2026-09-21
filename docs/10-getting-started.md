# Getting started

The page walks from an empty crate to a working program. It uses the JSON codec
as the example, and every block compiles and runs.

## Add the crates

The crates are published as `structury` and `structury-json`.

```toml
[dependencies]
structury = "0.1.0-alpha.1"
structury-json = "0.1.0-alpha.1"
```

## Read one value

A request is a slice of `Demand` values. This program reads `users[1].id`.

```rust
use structury::{Demand, Step};
use structury_json::{Form, JsonInput, MaterializeOptions, Materialized, ScanRequest, materialize, scan};

fn main() {
    let src = br#"{"users":[{"id":1},{"id":2}]}"#;
    let demands = [Demand::Path {
        steps: vec![Step::key("users"), Step::index(1), Step::key("id")],
        nested: None,
    }];
    let request = ScanRequest::new(JsonInput::Text, &demands);
    let result = scan(src, &request).expect("valid JSON");
    let materialized = materialize(
        &result.answers[0],
        MaterializeOptions::default().with_form(Form::Value),
    )
    .expect("located value");
    let Materialized::Value(value) = materialized else {
        unreachable!("Form::Value yields a value");
    };
    println!("users[1].id = {value}");
}
```

The scan walks the source once, answers the demand, and never builds the rest
of the document. The answer is a span into `src`, and the `materialize` call
turns it into an owned `Value`.

## Read rows

A `Project` demand reads one row per element, and `materialize` turns the batch
into a `Value` array.

```rust
use structury::{Demand, Path, Value};
use structury_json::{Form, JsonInput, MaterializeOptions, Materialized, ScanRequest, materialize, scan};

fn main() {
    let src = br#"{"users":[{"id":1,"name":"ada"},{"id":2,"name":"bob"}]}"#;
    let demands = [Demand::Project {
        path: Path::key("users"),
        fields: vec!["name".into()],
    }];
    let request = ScanRequest::new(JsonInput::Text, &demands);
    let result = scan(src, &request).expect("valid JSON");
    let materialized = materialize(
        &result.answers[0],
        MaterializeOptions::default().with_form(Form::Value),
    )
    .expect("projected rows");
    let Materialized::Value(rows) = materialized else {
        unreachable!("Form::Value yields a value");
    };
    for row in rows.as_array().expect("projected rows") {
        let name = row.member("name").and_then(Value::as_str).unwrap_or("");
        println!("{name}");
    }
}
```

Each cell in the answer is a span into the source or `Absent`, so a row that
lacks a field keeps the absence instead of a null.

## Stream records

A stream is a virtual array of top-level values, and `scan_each` visits one
record at a time.

```rust
use structury::{Answer, ColumnCell, Demand, Path};
use structury_json::{JsonInput, ScanRequest, scan_each};

fn main() {
    let src = b"{\"id\":1}\n{\"id\":2}\n";
    let demands = [Demand::Project {
        path: Path::root(),
        fields: vec!["id".into()],
    }];
    let request = ScanRequest::new(JsonInput::Ndjson, &demands);
    scan_each(src, &request, |answer| {
        if let Answer::Columns(columns) = answer {
            if let Some(ColumnCell::Span(span)) = columns.cells().first() {
                let id = &src[span.start()..span.end()];
                println!("id = {}", String::from_utf8_lossy(id));
            }
        }
    })
    .expect("well-formed frames");
}
```

A root `Project` reads the named fields from every record, and each visit is a
`Columns` batch with one row. The cell is a span into the source. The walker
reuses its buffers across records, so the allocation cost follows the demand set
and not the record count.

## Edit a document

An edit names a target by path. This program changes one value in a JSONC
document and keeps the comment and the formatting.

```rust
use structury::{Number, Step, Value};
use structury_json::{Dialect, Edit, EditOptions, edit};

fn main() {
    let src = br#"{
  // the port to listen on
  "port": 8080,
}"#;
    let edits = [Edit::Set {
        path: vec![Step::key("port")],
        value: Value::Number(Number::parse("9090").expect("a spelling")),
    }];
    let out = edit(src, &edits, EditOptions::new(Dialect::Jsonc)).expect("editable input");
    println!("{}", String::from_utf8(out).expect("UTF-8 output"));
}
```

The output keeps every byte outside the changed region, and the dialect decides
how the input is read and how the output is checked. [Edit and
facts](05-edit-and-facts.md) covers the path in depth.

## Where to go next

- [Overview](01-overview.md) tours the read, write, edit, and parallel paths,
  with what each is good for.
- [Core concepts](02-core-concepts.md) explains the demand, answer, and data
  shapes.
- [Framing and dialects](04-framing-and-dialects.md) covers the input
  arrangements and the grammars.
- [Scan](06-scan.md) and [Sharding](07-sharding.md) cover the walk and the
  parallel plan.
- [Performance](11-performance.md) collects the internal benchmark results.
