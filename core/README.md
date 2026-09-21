# structury

The core that works for any format in the structury family. Format crates such as
[`structury-json`](https://github.com/filip-41/structury/tree/main/json) depend
on the core for the data model, the demand vocabulary, and the host seam. The
core defines what an answer is. It never reads bytes directly, so it ships no
parser and no format rules of its own.

## What the core is for

A codec that answers demands reads only the work a request asks for. The caller
describes that work with a `Demand`, the codec touches the bytes it needs, and
the result holds one `Answer` per demand in demand order. Work nobody asked for
is never materialized, and a path that is not there stays missing instead of
turning into null.

The core holds the parts every such codec shares, so a new format does not need
its own value tree, its own filter language, or its own host protocol. A codec
plugs into the types below and adds the format rules on top.

## Main responsibilities

### The owned data model

`Value` is a tree of null, booleans, strings, exact numbers, arrays, and
objects. `Number` keeps the authored spelling and compares numerically on
demand, so `2.50` and `2.5` are one value with two spellings. `CompactStr` keeps
short strings inline, and short strings cover the common case for object keys.

Equality has two notions. `==` compares by value and ignores object member
order, while `strict_equal` keeps both spelling and member order. `compare`
orders two values when an order exists. The
[values example](examples/values.rs) walks through the behavior.

### Documents over borrowed bytes

`Document` is a borrowed view over a byte range that a codec has already
validated, and it carries the grammar of that validation pass as a `GrammarTag`.
A codec uses the tag to refuse a write that keeps bytes unchanged when the
grammar does not match its options. `BorrowedDocument` is the view without
copies that a whole-buffer read produces.
`OwnedDocument` and `Arena` move the same view across threads or past the source
buffer, and both share one node layout, so navigation code does not care which
one it holds.

### The demand interface

`Demand` is the request. Variants cover a whole document, a path, a
collection, a slice, a projection, a filter, and the low cost oracles that answer
from the scan itself, such as a count or a kind. `Path` and `Step` build the key
and index chains, and `Predicate` is the small expression language behind
filters, with equality, ordering, and the boolean operators.

### Answers and errors

A scan produces a `ScanResult` with one `Answer` per demand. An answer holds a
located document, a batch of columns, an oracle result, a missing path, or a
type mismatch. Errors carry a class such as `Shape`, `Control`, or `Write`, and
recoverable problems travel beside the answers as `Issue` values.

### The host seam

A host can split a document into parts, scan the parts on its own threads, and fold
the results. `Drive` runs a list of part ranges and falls back to a serial scan
when a part reports a failure other than a control stop. `stitch` is the fold
itself, and `Control` carries the cancellation flag, the deadline, and the
memory ceiling that a host sets. The `byte-scan` feature adds the stop-set kernels
that codecs use to scan raw bytes.

## Using it

```rust
use structury::{Number, Predicate, Value};

fn main() {
    let row = Value::Object(vec![
        ("name".into(), Value::Str("ada".into())),
        ("score".into(), Value::Number(Number::parse("91").expect("a spelling"))),
    ]);

    let passing = Predicate::Ge {
        field: "score".into(),
        value: Value::Number(Number::parse("85").expect("a spelling")),
    };
    assert!(passing.matches(&row));
}
```

Runnable examples live in [`examples/`](examples), and the
[guide](https://github.com/filip-41/structury/blob/main/docs/README.md)
explains the demand model, the data model, and the host seam in longer prose.

## Examples

- [values.rs](examples/values.rs) builds a small tree, walks the tree, and shows the
  two equality notions.
- [predicates.rs](examples/predicates.rs) filters rows with `Predicate`.
- [numbers.rs](examples/numbers.rs) parses spellings, compares spellings by value,
  and projects spellings to machine numbers.
- [resolve.rs](examples/resolve.rs) resolves an index, including a negative index
  that counts from the end.

## Missing is not null

An absent path reports `Answer::Missing` and never `Value::Null`. Null is a
value that a document can hold, so folding the two together would lose the difference
between "the document says null" and "the document has nothing there". The same
rule runs through the oracle answers and the stitch fold.

## Feature `byte-scan`

The `byte-scan` feature compiles the SIMD stop-set kernels behind `prefix_len`
and `lane8`. The string, whitespace, and comment scans of a codec run on those
kernels. The module is `#[doc(hidden)]` because it is an internal seam that a sibling
codec names, not an extension point. Cargo feature unification means any
dependent of a codec compiles those kernels, including their `unsafe`, even
though the core denies `unsafe_code` by default. There is no opt-out today.

## Platform

The crates run as `no_std` with `alloc`. Document stores use `alloc::sync::Arc`, so a
target needs pointer-width atomics, and `Control` also uses 64-bit atomics. Targets
without those atomics are untested.

## API stability

The current release is `0.1.0-alpha.1`, so nothing is frozen. Every type that a codec matches
exhaustively, such as `Demand`, `Answer`, and `Oracle`, stays without
`#[non_exhaustive]`. Adding a variant there shows up as a visible breaking change instead
of a silent wildcard. Request and option structs and the result enums carry
`#[non_exhaustive]` and builder methods, so a new setting stays additive.

## License

MIT or Apache-2.0, at your option. The license files live at the repository
root.
