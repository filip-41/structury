# Edit and facts

An edit names a change to a source buffer. `structury` keeps the source and
applies each change as a splice, so the bytes outside the changed region survive
untouched. The alternative, parse the document into a tree and write the tree
back, loses the formatting and the comments that the tree did not model.

Editing is codec-owned, like framing and dialects. A codec defines its edit
vocabulary, locates the targets in its own grammar, and writes the output. The
core contributes the value model and the fact model, and the JSON codec is the
first to implement the edit path.

## How an edit is done

An edit names a target by path and an operation. A batch of edits is data, like
a demand, so it can be built, inspected, and replayed.

Three stages carry an edit from request to output.

1. Locate. The codec runs one strict pass with a demand per edit and reads the
   located spans. A strict pass is required because the planner must know that
   the bytes it will splice are valid.
2. Splice. Each change becomes a replacement of one source range, and the
   splices are applied to a copy of the source. Overlapping splices are refused
   as an error.
3. Fall back. When a splice cannot express a change, the codec re-encodes a DOM
   instead. The fallback drops formatting and comments, so it is a correctness
   net and not the preferred path.

## Lossless where it can be

The splice model is lossless by construction for every region it does not
touch. The output keeps formatting, key order, number spellings, and comments,
because those bytes never move. A tree rewrite has no such property, because the
writer emits what the tree holds and drops what it never modeled.

Some changes still need the fallback. Renaming an object member onto the key of
another existing member cannot be a splice, because the output would hold two
members with that key. A codec reports such a change as unplaceable and
re-encodes.

## What the dialect decides

The dialect sets three things for an edit.

- How the input is read. The locate pass reads the source under the request
  dialect, so comments and trailing commas are accepted where the grammar
  allows them.
- How the output is written and checked. The codec re-validates the spliced
  output under the same dialect, and the DOM fallback encodes under it. A
  commenting dialect can let a splice place a comma against a closer, so the
  check catches output the grammar would refuse.
- How facts are treated. A comment is metadata in the fact model, and a
  commenting dialect decides whether a fact can be written at all. Under a
  dialect with no comments, a fact edit is inert.

## The JSON codec's edit path

The JSON codec implements the three stages in `json/src/edit.rs`. It defines the
edit vocabulary, plans the splices, remaps the facts, and falls back to a DOM
re-encode.

### The edit vocabulary

`Edit` names a change by path.

```rust,ignore
pub enum Edit {
    Set           { path: Vec<Step>, value: Value },
    Insert        { path: Vec<Step>, value: Value },
    Delete        { path: Vec<Step> },
    ReplaceMember { path: Vec<Step>, key: String, value: Value },
    Clear         { path: Vec<Step> },
    Fact          { path: Vec<Step>, op: FactOp },
}
```

- `Set` replaces the value at `path`.
- `Insert` inserts at the last step with an array index or a new object member.
  A key that already exists replaces the value of that member.
- `Delete` removes the value at `path`, while an absent target is a no-op.
- `ReplaceMember` rewrites the key and value of a member and keeps its
  position.
- `Clear` empties the container at `path`, leaving `{}` or `[]`.
- `Fact` writes a comment on the node at `path`.

`FactOp` is `Insert` with role and text, `Replace` with role and text, or
`Clear` with role. Under `Dialect::Rfc8259` there are no comments, so every
`FactOp` is inert.

`EditOptions` with `dialect` sets the grammar that reads the input and
re-validates the output.

### One strict locate pass

The `plan_edits` function in `json/src/edit.rs` builds one `ScanRequest` with a
demand per edit, runs the request at `Strictness::Strict`, and reads the
located spans out of the answers. `Strict` is required because the planner must
know that the bytes it will splice are valid.

The demand for each edit is the parent of the changed slot, except for `Set`,
`Clear`, and `Fact`, which locate the target itself through `edit_requests`. A
leading `Demand::Whole` is added when facts are needed, namely for
`edit_document` or for an `Edit::Fact` under a commenting dialect, so the same
pass also returns the input facts. Locating the parent instead of each target
lets `Delete`, `Insert`, and `ReplaceMember` read the surrounding members and
decide comma placement.

An edit whose demand answers `Missing` has no splice. `Set` and `Insert` then
report `edit-unplaceable`, while `Delete` of an absent path is a no-op. A path
that answers with a value other than `Document` is a write error with the
message "edit path is not a value span".

### The splice planner

A `Splice` is a half-open source range `[from, to)` and the bytes that replace
the range.

```rust,ignore
struct Splice {
    index: usize,          // position in the edits list, the tie-break
    from: usize,
    to: usize,
    bytes: Vec<u8>,
    fact: Option<NewFact>,
}
```

Splices are collected and then sorted in descending order by `from`, and
overlapping splices are refused as a write error. Writing applies splices to a
copy of the source. Because splices run right to left, every splice that has
not run yet still has valid original offsets through `Plan::write_into`. The
output buffer is reserved up front from the total growth so a splice never
reallocates and copies the tail.

```text
src:   { "a": 1, "b": 2 }
              ^^^ Set "a" -> 9
splice: from=7 to=8 bytes="9"
out:   extend_from_slice(src); out.splice(7..8, "9")
       -> { "a": 9, "b": 2 }
```

### Comma handling

Deleting or appending a member must not leave a stranded comma. The
`comma_span` function extends a delete range to swallow the adjacent comma.
The `append_member` function inserts before the closer, drops the preceding
glue and any trailing comma first, and prefixes a comma when the container was
non-empty. The `plan_insert` function handles the empty container, index 0, and
append cases separately, so a new object key or array index lands with its
separating comma and never against a source trailing comma. The
`plan_replace_member` function rewrites the key token and the value token
independently, so the colon and surrounding whitespace stay in place. Renaming
onto the key of another existing member cannot be a splice because it would
leave two members with that key, so it falls back to the DOM.

For an array index `Insert`, the index is resolved with `insert_index`, which
allows insertion at `len` for append but rejects out-of-range positions.

### Span remap

If the caller keeps facts with `edit_document`, every input fact glyph span and
owner span is shifted through the splices with `remap_one` and `map_span`. A
glyph that straddles or sits inside a removed region is dropped and never cut
in half, while a surviving glyph and its owner follow the splice. The remap
runs over splices in ascending order and carries a running delta, so each
original span maps through the edits before it. A newly written fact glyph
lives in output space, so it is appended after every remap instead of passing
through the remap. Finally the facts are stably sorted by glyph start to
restore the ascending-order invariant.

### Validation and the DOM fallback

Under a commenting dialect a splice can place a comma against a closer because
trailing commas are legal, so `needs_validation` keeps a `Strict`
re-validation of the output. If validation fails, or if the planner reported
`edit-unplaceable`, `edit` re-expresses the whole request as a DOM application
instead.

```text
fallback_encode:
  parse(src) ─► Form::Value
  apply each Edit to the Value (apply_dom)
  encode(Source::Value) ─► bytes
  validate Strict
```

The DOM fallback drops formatting and comments because it re-encodes
canonically, so it is a correctness net and not the preferred path. It is
refused when any edit is a `Fact` write, because the DOM has no facts. A
request error with overlapping edits or with deleting or inserting at the root
stays fatal, and only the `edit-unplaceable` code can fall back.

### Fact writes

The `plan_fact` function handles `FactOp`.

- Insert places the comment by role. A `CommentLead` inserts before the key of
  the member, where `member_insert_at` walks back over trivia and `:` and the
  key. A `CommentFoot` inserts on its own line after the value. A
  `CommentInline` inserts on the line of the value. Empty text is a no-op.
- Replace rewrites the existing comments of that role on the node.
  `same_place_comment` reuses the authored shape when it fits, where a `//`
  span stays a line comment and a `/* */` span stays a block comment, and
  falls back to canonical `//` lines otherwise. Empty replacement clears the
  comments.
- Clear removes the comments of that role.
- A `CommentFoot` is a no-op for replace and clear because a foot shares the
  glyph of the lead of the following node, and rewriting the shared glyph
  would corrupt the pair.

`edit_document` returns a `Document` over the output buffer with the remapped
facts attached through `set_facts`. A fact glyph straddling a splice is
dropped, so the document is always internally consistent.

Next is [Scan](06-scan.md).
See also [Introduction](README.md) and [Framing and dialects](04-framing-and-dialects.md).
