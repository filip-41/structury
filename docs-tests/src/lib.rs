//! Compiles the workspace README and the guide pages as doctests.
//!
//! The crate is not published. The workspace README and the `docs/` guide live
//! outside every package, so their examples are tested here and the published
//! crates stay self-contained.

/// Compiles the workspace README examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../README.md")]
pub struct WorkspaceReadmeDoctests;

/// Compiles the introduction examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/README.md")]
pub struct IntroductionDoctests;

/// Compiles the overview examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/01-overview.md")]
pub struct OverviewDoctests;

/// Compiles the core concepts examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/02-core-concepts.md")]
pub struct CoreConceptsDoctests;

/// Compiles the host seam examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/03-host-seam-and-sharding.md")]
pub struct HostSeamDoctests;

/// Compiles the framing and dialects examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/04-framing-and-dialects.md")]
pub struct FramingDoctests;

/// Compiles the edit and facts examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/05-edit-and-facts.md")]
pub struct EditDoctests;

/// Compiles the scan examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/06-scan.md")]
pub struct ScanDoctests;

/// Compiles the sharding examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/07-sharding.md")]
pub struct ShardingDoctests;

/// Compiles the materialize and encode examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/08-materialize-and-encode.md")]
pub struct MaterializeDoctests;

/// Compiles the stability and internals examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/09-stability-and-internals.md")]
pub struct StabilityDoctests;

/// Compiles the getting started examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/10-getting-started.md")]
pub struct GettingStartedDoctests;

/// Compiles the performance examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../docs/11-performance.md")]
pub struct PerformanceDoctests;
