//! Format-neutral demand, values, and the host seam.
//!
//! [`Demand`] is the interface; missing is an [`Answer`], never [`Value::Null`].
//!
//! # API stability
//!
//! This is `0.1.0-alpha.1`; nothing is frozen. Types a codec matches exhaustively
//! ([`Demand`], [`Answer`], [`Oracle`], …) are deliberately **not**
//! `#[non_exhaustive]`, so adding a variant is a visible breaking change rather
//! than a silent wildcard. Request/option structs and result enums are
//! `#[non_exhaustive]` so a new knob is additive.
//!
//! # Platform
//!
//! `no_std` with `alloc`. The document stores are behind `alloc::sync::Arc`, so
//! the target needs pointer-width atomics; [`Control`] also uses 64-bit atomics.
//! Targets without those are untested.
//!
//! # Feature `byte-scan`
//!
//! Compiles the SIMD stop-set kernels and is what the `byte_scan` module exists
//! for. The codec enables it, and Cargo feature unification means any dependent
//! of the codec compiles `unsafe` even though this crate denies it by default.
//! There is no opt-out today.

#![no_std]
#![deny(missing_docs)]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod arena;
mod borrowed;
/// SIMD stop-set kernels, a format codec's private implementation detail, public
/// only behind the `byte-scan` feature. `#[doc(hidden)]`: an unstable internal
/// seam a sibling codec names, not an extension point, and the feature compiles
/// `unsafe` into a build that otherwise denies it.
#[cfg(feature = "byte-scan")]
#[doc(hidden)]
pub mod byte_scan;
#[cfg(not(feature = "byte-scan"))]
#[allow(dead_code, reason = "a format codec enables the byte-scan feature")]
mod byte_scan;
mod compact;
mod control;
mod demand;
mod document;
mod drive;
mod error;
mod number;
mod owned;
mod scan;
mod stitch;
mod value;

pub use arena::{Arena, ArenaValue, Node};
pub use borrowed::{BorrowedDocument, BorrowedValue};
pub use compact::CompactStr;
pub use control::Control;
pub use demand::{Demand, Name, Oracle, Path, Predicate, Range, Shard, Step, Strictness};
pub use document::{ColumnCell, Columns, Document, Fact, FactOwner, FactRole, GrammarTag};
pub use drive::Drive;
pub use error::{Error, ErrorClass, Issue};
pub use number::{Decimal, NonFinite, Number, NumericError};
pub use owned::{OwnedDocument, OwnedValue};
pub use scan::{Answer, ByteRange, OracleAnswer, ScanResult};
pub use stitch::stitch;
pub use value::{Value, ValueKind, resolve_index};

/// Compiles the crate README examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
