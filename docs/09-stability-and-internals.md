# Stability and internals

The page covers the stability policy and the internal seams of the project. It
describes which public types are open and closed, which seams are hidden from
docs, the platform and feature constraints, and the items that are deliberately
not extension points.

The policy applies to the core and to every codec, and it is stated in
`core/src/lib.rs` and the workspace `README.md`.

## The semver growth policy

The current release is `0.1.0-alpha.1`, so nothing is frozen. The project still
distinguishes two kinds of public type, because the distinction changes what a
future release is allowed to do.

- Codec-coupled enums stay without `#[non_exhaustive]`. A codec matches those
  enums exhaustively, so adding a variant shows up as a visible breaking change
  instead of a silent wildcard arm. The set is `Demand`, `Answer`, `Oracle`,
  `Edit`, `FactOp`, `Node`, `Value`, `Number`, and `FactRole`. Code that matches
  one of those enums must handle every variant, and the requirement is the
  point.
- Request and option structs and result-style enums carry
  `#[non_exhaustive]`. A new setting is additive, so downstream code must build
  those types with `new` or `default` and the `with_*` builders instead of
  struct literals. The set includes `ScanRequest`, `EncodeOptions`,
  `EditOptions`, `MaterializeOptions`, `ScanResult`, `Issue`, `Form`,
  `Materialized`, `JsonInput`, `Indent`, `ItemFraming`, `Source`,
  `OracleAnswer`, `ErrorClass`, `NumericError`, `Dialect`, `Trace`,
  `TraceCounters`, `TraceEvent`, `AnswerKind`, and `CheckLevel`.

The rule of thumb asks whether a codec needs to know every variant to stay
correct. If the answer is yes, the enum is closed and a variant add is
breaking. If the answer is no, the enum is open and a variant add is minor.

## Seams hidden from docs

Some items must be `pub` for a sibling codec to build on them, but they are not
part of the supported surface. They carry `#[doc(hidden)]` and count as
internal to a codec.

- `structury::byte_scan` in `core/src/byte_scan.rs` holds the SIMD stop-set
  kernels behind the `byte-scan` feature. The lexer and cut scanner of the JSON
  codec use `prefix_len`. No external caller should use the module.
- `Document::from_span_validated` and `GrammarTag` in `core/src/document.rs`
  record the `Strict` result of a codec and the opaque grammar provenance that
  was validated. The verbatim write gate of the JSON codec compares the tag.
  The constructor does not perform validation, so a caller without validated
  bytes must use `from_span`.
- `Node`, `OwnedDocument::from_parts`, and `BorrowedDocument::from_parts` in
  `core/src/arena.rs`, `core/src/owned.rs`, and `core/src/borrowed.rs` form the
  construction seam of a format codec. `from_parts` does not check its arenas,
  so a caller can build a malformed document. An out-of-range edge or a cycle
  reads as `null` in `to_value`, a malformed payload is refused by
  `detach` and `to_owned`, and a `Node::Number` with a bad spelling still
  panics in `to_value`.
- `Arena` is public because `ArenaValue` is the navigation surface that callers
  read through, but a private supertrait seals the trait, so only the two
  documents implement it.

## `byte-scan` and `unsafe`

The `structury` crate denies `unsafe_code` at the crate root, yet it ships SIMD
kernels. The `byte-scan` feature resolves the tension.

- The `byte-scan` feature of `structury` compiles the kernels and exposes the
  module.
- `structury-json` enables `structury/byte-scan` in its `Cargo.toml`.
- Cargo feature unification means any dependent of the codec builds `structury`
  with the feature on, and therefore compiles the `unsafe` kernels, even though
  `structury` alone denies unsafe and even though the dependent did not ask for
  the feature. There is no opt-out today.

The kernels are written so each `unsafe` block has a SAFETY note, lanes are
walked with `as_chunks` so every load is a live `[u8; W]` by type, and the
AVX2 and SSE2 paths are checked byte-identical in tests. The `StopSet`
descriptor laws with `EQ_LEN <= 8` and `GE != Some(0)` are proved at compile
time by `Contract::CHECK`, which `prefix_len` references, so a violating set is
a build error and not a release panic.

## `no_std` and atomics

Both crates run as `#![no_std]` with `extern crate alloc`. The setting has two
platform consequences.

- Document stores keep their `Columns` cells and field names behind
  `alloc::sync::Arc`, so a target needs pointer-width atomics.
- `Control` in `core/src/control.rs` uses `AtomicU8` and `AtomicU64`, so a
  target needs 64-bit atomics.

Targets without those atomics are untested. The host owns I/O and threads, so
there are no I/O error classes and nothing in a codec starts a thread, as
described in [Host seam and sharding](03-host-seam-and-sharding.md).

## What is not an extension point

Explicit limits help because several public names look extensible.

- `Node` and the `from_parts` constructors are not a builder API. They exist so
  `structury-json` can assemble documents, and they skip validation, as
  described above.
- `Arena` is not implementable downstream. The private supertrait seals it.
- The `sealed` module of `StopSet` is a convention and not enforcement. The
  module and trait are public but hidden from docs, so a third-party crate can
  implement `Sealed` and then `StopSet`. What actually protects the kernels is
  `Contract::CHECK`. A violating set fails the build. The seal documents
  intent, while the contract gives the guarantee.
- `Document::from_span_validated` is not a way to bypass validation. It records
  the fact that a `Strict` pass already happened, and it does not make such a
  pass happen. Calling it on unvalidated bytes opens the write gate
  incorrectly.

Everything outside that list is the supported surface, namely the demand
vocabulary, the answers, the documents and their navigation, `materialize`,
`encode`, `edit`, and the shard `Plan`.

Next is [Introduction](README.md).
See also [Core concepts](02-core-concepts.md) and [Overview](01-overview.md).
