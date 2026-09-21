//! JSON demand codec: one scan answers N [`structury::Demand`]s.
//!
//! [`Strictness`](structury::Strictness) selects the check level. Structural and
//! Lazy locate unread structure without value-checking it. Strict value-checks
//! every byte. Writes refuse a document that was not fully validated.
//! The codec starts no thread.
//!
//! The codec enables `structury/byte-scan`, so a dependent compiles the SIMD
//! kernels (and their `unsafe`) regardless of its own feature selection.

#![no_std]
#![deny(missing_docs)]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod borrowed;
mod dialect;
mod edit;
mod encode;
mod encode_parallel;
mod error;
mod facts;
mod framing;
mod lex;
mod materialize;
mod scan;
mod shard;
mod tape;
mod walk;

pub use dialect::Dialect;
pub use edit::{Edit, EditOptions, FactOp, edit, edit_document};
pub use encode::{EncodeOptions, Indent, ItemFraming, Source, encode, encode_document, encode_value};
pub use encode_parallel::{
    ItemRange, MIN_ITEMS_PER_PART, ValuePlan, encode_value_chunk, plan_encode_value, stitch_value_chunks,
};
pub use framing::{adjacent_prefix_len, complete_prefix_len, partition_adjacent, partition_json_seq, partition_ndjson};
pub use materialize::{Form, MaterializeOptions, Materialized, materialize, parse};
pub use scan::{JsonInput, ScanRequest, scan, scan_controlled, scan_each, scan_each_with_issues, validate};
pub use shard::{CutSummary, FIRST_SHARD_BYTES, Plan};

pub use lex::MAX_NESTING;

/// Compiles this crate's README examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct CrateReadmeDoctests;
