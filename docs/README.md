# Introduction

The page introduces `structury`. It describes what the project is, what it aims
at, how it differs from other codecs, and what each remaining page covers.

## What structury is

`structury` is a family of codecs for structured data that work on borrowed
bytes. A request declares what to read, encode, or edit, and the codec performs
only that work, with no intermediate document unless the request asks for one.

The workspace has two crates. The `core/` crate, named `structury`, holds the
parts that work for any format, namely the demand vocabulary, the answer types,
the value model, the documents, the error types, and the host seam. The `json/`
crate, named `structury-json`, is the first codec, and it covers RFC 8259 text,
adjacent framing, NDJSON, JSON-seq, and a dialect with JSONC and a JSON5 subset.

The current release is `0.1.0-alpha.1`, and the API is not frozen.

## What it aims at

The aim is one codec family over one shared model, so a new format adds grammar
and framing instead of a new value tree, a new filter language, and a new host
protocol.

JSON is the first format. Later codec crates cover CSV and TSV, TOML, YAML,
XML, HTML, CBOR, MessagePack, and INI with Java properties and dotenv. Every
codec answers the same `Demand` values and produces the same `Value` and
`Document` types, so a request written once runs against any format, and a
value read from one format can be written in another.

## How it differs from other codecs

JSON tooling commonly takes one of two shapes. A document model builds a
complete tree and then queries the tree, which pays for every value you never
read and loses the original formatting. A hand-written streaming parser avoids
the tree cost, but the caller tracks the parser state by hand.

`structury` takes a third shape, and the differences show in the API.

- A request is data. A `Demand` states what to locate, and one pass answers
  every demand in the request. Because a demand is plain data and not a
  callback, code can clone it, compare it, and plan over it before any byte is
  read, and `Demand::shard` lists which demands can run in independent source
  ranges.
- A located value is a span into the source. `Document` holds a `ByteRange`
  into the caller buffer, so locating a value copies nothing, and bytes are
  copied only when you ask for a materialized `Form`. An absent path answers
  `Missing` and never `Value::Null`, so a null value and an absent key stay
  apart. `Number` keeps the authored spelling, so `1.50` and `-0` survive a
  round trip, and `as_f64` is the one lossy projection.
- Checking is a setting. `Strict` validates every value, `Structural` checks
  only the demanded values, and `Lazy` leaves value checks to `materialize`.
  Structure is always read. A write then refuses a `Document` that no `Strict`
  pass validated, which is the write gate.
- The host owns I/O, threads, and time. The codec starts no thread and performs
  no I/O, so a scan is a pure function from bytes and a request to a
  `ScanResult`. A host with a thread pool splits a source with `Plan` and folds
  the parts with `Drive` and `stitch`, and `Control` carries cancellation, a
  deadline, and a memory ceiling.

Writing and editing work the same way. `encode` writes a `Value` with the
canonical walk, and it writes a validated `Document` byte for byte unless a
canonical rewrite is requested. `edit` locates a list of `Edit` values in one
`Strict` pass and applies byte splices, with a re-encode of a DOM as the
fallback when a splice cannot express a change.

## The rest of the guide

Pages 1 to 9 describe the concepts that every codec shares, with the JSON codec
as the worked example. Page 10 walks through a first program, and page 11
collects the internal benchmark results.

1. [Overview](01-overview.md) states the problem, the design choices, the two
   crates, and the data flow.
2. [Core concepts](02-core-concepts.md) covers `Demand`, `Answer`, the
   one-answer-per-demand zip, `Strictness`, the value and document shapes,
   oracles, errors, and `Control`.
3. [Host seam and sharding](03-host-seam-and-sharding.md) covers host ownership
   of I/O and threads, `Drive`, `stitch`, the `Shard` law, and `Control`.
4. [Framing and dialects](04-framing-and-dialects.md) covers how a codec
   arranges values in a buffer and names the grammar it reads, with `JsonInput`
   and `Dialect` as the first instance.
5. [Edit and facts](05-edit-and-facts.md) covers how edits are located and
   applied as splices, why the output stays lossless, and how a dialect decides
   the read, the write, and the treatment of comment facts.
6. [Scan](06-scan.md) covers the lex layer, the one validating walk, streams,
   and the check level.
7. [Sharding](07-sharding.md) covers the plan, the cut scanner, eligibility,
   and the host protocol.
8. [Materialize and encode](08-materialize-and-encode.md) covers how answers
   become documents and bytes, and the write gate.
9. [Stability and internals](09-stability-and-internals.md) covers the semver
   policy, the seams hidden from docs, and what is not an extension point.
10. [Getting started](10-getting-started.md) walks from an empty crate to a
    first program.
11. [Performance](11-performance.md) covers where the codec is fast and where
    it is slow, with the internal benchmark results.

## Glossary

The guide uses a small vocabulary. The table gives each term a one-line meaning.

| Term | Meaning |
|---|---|
| `Answer` | The outcome of one demand: a document, a batch of columns, an oracle result, missing, or a mismatch. |
| `ByteRange` | A half-open `[start, end)` span into the source bytes. |
| `Columns` | A projected batch of cells, one row per selected element. |
| `Demand` | One piece of requested work. |
| Dialect | A named grammar a codec can read, such as JSONC. |
| `Document` | A located span with validation state, grammar provenance, and facts. |
| `Drive` | The core type that runs planned ranges and falls back to a serial scan on a hitch. |
| Fact | Metadata attached to a node, such as a comment. |
| `Form` | The artifact a materialize call produces: borrowed, owned, or value. |
| Framing | How one or more values are arranged in a buffer. |
| `GrammarTag` | The opaque grammar provenance a codec records on a validated document. |
| Hitch | A non-control failure in one parallel part, which makes the drive fall back to serial. |
| `Issue` | A recovering diagnostic that travels beside the answers. |
| Mark | One answer slot in the walk, one per demand. |
| Morsel | One planned range for a parallel scan. |
| `Oracle` | A property question answered during the walk, such as a count. |
| `Plan` | The codec type that cuts a source into ranges and re-enters per range. |
| Shard | The law that says whether a demand can run in independent ranges. |
| `stitch` | The core function that folds per-range results into one result. |
| `Strictness` | The check level: `Strict`, `Structural`, or `Lazy`. |
| Tape | The builder that turns a located span into an arena. |
| `Value` | The owned semantic tree. |
| Write gate | The rule that a document must be strictly validated before a byte-preserving write. |

## See also

- The workspace [README](../README.md) holds a runnable example and the
  project goals.
- Sibling crate summaries live in [`core/`](../core/) and [`json/`](../json/).
