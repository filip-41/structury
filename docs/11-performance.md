# Performance

The numbers on this page come from a private comparison harness that runs the
same fixtures through structury and a set of other implementations. The harness
is internal for now, so treat the numbers as a general look at where the codec
is fast and where it is slow. Do not quote them as concrete results, because
they move with the host and the fixtures.

## Where the numbers come from

The harness compares the JSON codec with `serde_json`, `sonic-rs`, `simd-json`,
`yyjson`, C++ simdjson, and `RapidJSON`. Each measurement reports throughput in
GB/s over the input bytes and a ratio against the best of the other compared
tools. The fixtures are a generated document of about 20 MB with 25k rows and 44
members, plus cached real-world corpora: a whitespace-heavy catalog file, a
social file, and a 100k-record NDJSON stream.

The runs below were recorded on a quiet host, meaning no other work was
running. The host is an Apple M5 Max with 18 cores and 128 GiB of memory,
running macOS 26.5.1 on aarch64. A contended host moves these numbers, so the
regression gate uses floors rather than the recorded values.

## Where it wins

- Selective reads. A projection with a limit over the large fixture ran at
  164.6 GB/s, which is 66 times the best of the other compared tools. The walk
  stops once the window is answered, so the rest of the document is never read.
- Locate-only reads. A read that only locates spans and never builds a value
  ran at 2.7 to 3.0 GB/s, 3.1 to 3.3 times the best of the other compared
  tools.
- Projections and filters. A one-field projection ran at 2.74 GB/s (1.09 times
  the best of the other compared tools), a wide selection at 2.14 GB/s (1.55
  times), and filtered reads at 1.23 to 1.35 times. A default structural read
  ran at 2.52 GB/s, at parity with the best of the other compared tools.
- Memory. On the 28 MB input, a locate-only read used 28.6 MiB of peak memory
  against 274.3 MiB for simd-json, 129.3 MiB for C++ simdjson, and 95.1 MiB for
  yyjson. A full materialize used 69.3 MiB against 212.3 MiB for serde_json and
  309.8 MiB for simd-json.
- Writes and edits. A verbatim write, which validates the document and then
  copies it, ran at 2.43 GB/s on the generated fixture, while canonical rewrites
  ran at 0.68 GB/s for compact output and 0.55 GB/s for pretty output. Splice
  edits ran at 1.75 to 2.50 GB/s, which is 9.2 to 12.1 times a DOM mutate and
  serialize.
- Parallel work. With host threads over planned ranges, concurrent queries
  scaled 11.9 to 14.4 times the serial run, and a sharded scan scaled 9.7
  times, or 3.9 times including the plan build.

## Where it loses

- Full tree builds. A full materialize of the large fixture ran at 0.95 GB/s,
  0.48 times the best of the other compared tools, and the same read on the
  narrow fixture at 6.87 GB/s, 0.92 times. The arena builder costs about 7 ns
  per node where the other tools need about 4.7, so a full build is not the
  strong path.
- Whitespace-heavy files. A whitespace-heavy catalog file ran at 3.05 GB/s
  (0.61 times) and a social file at 3.18 GB/s (0.75 times), with the structural
  variants near 0.58 to 0.60 times. The scan is bound by whitespace runs and
  scalar key reads.
- Mixed multi-demand reads. Reads that issue several demands in one request ran
  at 0.69 to 0.83 times, because the shared scan dominates the work.
- Comment-heavy JSONC. A comment-heavy read ran at 0.56 GB/s. It has no
  counterpart in the other tools, because none of them retains comments, but it
  is the slowest workload.

## What it is good for

- Reading a few fields from a large document, especially with a limit.
- Projections, filters, and counts over arrays you do not want to materialize.
- Locating spans for a later pass, in a memory budget close to the input size.
- Verbatim writes and edits that keep formatting and comments.
- Hosts that own a thread pool and want decode or encode work spread across it.

## What it is not good for

- Building a full tree when the whole document is needed anyway. A lean DOM
  reader does that faster.
- Dense, whitespace-heavy, or key-heavy JSON, where the scan is the bound.
- Workloads that need a query language or schema validation. The codec answers
  Rust demands, not query strings.

## Reading the numbers

The numbers above are receipts from one quiet-host session, and the regression
gate records floors from repeated runs on the same fixtures. A regression fails
the build, but the comparative ratios are targets for a quiet host, not quotable
benchmarks. Publishable benchmark results will be released along with the new
codecs.
