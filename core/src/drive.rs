//! The host-drive seam: scan planned part ranges, stitch the results, and fall
//! back to the serial answer when a part reports a non-control failure. A
//! [`ErrorClass::Control`] stop is the host's own stop and surfaces.

use alloc::vec::Vec;

use crate::demand::Demand;
use crate::error::{Error, ErrorClass};
use crate::scan::{ByteRange, ScanResult};
use crate::stitch::stitch;

/// A planned host drive: ordered part ranges executed with a hitch → serial
/// fallback. [`Drive::eligible`] is the `Shard` law's structural half; the codec
/// starts no thread.
#[derive(Clone, Debug)]
pub struct Drive {
    parts: Vec<ByteRange>,
}

impl From<Vec<ByteRange>> for Drive {
    /// A drive over `parts` in the given order (not sorted). Empty is accepted;
    /// [`Drive::run`] then calls `serial` directly.
    fn from(parts: Vec<ByteRange>) -> Self {
        Self { parts }
    }
}

impl Drive {
    /// Whether a request may fan out per the `Shard` law's structural half: every
    /// demand is parallel (`Shard::is_parallel`). A format codec may impose extra
    /// framing or dialect restrictions on top (the JSON codec's plan does), so use
    /// the codec's plan as the gate; this is the half a host can check alone.
    #[must_use]
    pub fn eligible(demands: &[Demand]) -> bool {
        demands.iter().all(|demand| demand.shard().is_parallel())
    }

    /// Execute the drive: call `scan_part` for each part in plan order, stitch
    /// the results, and fall back to `serial` when a part reports a non-control
    /// error. A *hitch* is any such non-control part error: parts already scanned
    /// are discarded and only `serial`'s answer is returned. A
    /// [`ErrorClass::Control`] error returns immediately without calling
    /// `serial`. An empty drive calls `serial` directly. A codec plan can expose
    /// a controlled per-part scan (the JSON codec's `Plan::scan_controlled`), so
    /// `scan_part` can report a host stop.
    ///
    /// # Errors
    ///
    /// The control stop a part reported, or `serial`'s own codec failure.
    ///
    /// # Panics
    ///
    /// When two parts bind different sources, through [`stitch`].
    pub fn run<'src, F, S>(&self, mut scan_part: F, serial: S) -> Result<ScanResult<'src>, Error>
    where
        F: FnMut(ByteRange) -> Result<ScanResult<'src>, Error>,
        S: FnOnce() -> Result<ScanResult<'src>, Error>,
    {
        if self.parts.is_empty() {
            return serial();
        }
        let mut collected = Vec::with_capacity(self.parts.len());
        for &part in &self.parts {
            match scan_part(part) {
                Ok(result) => collected.push(result),
                Err(error) if error.class() == ErrorClass::Control => return Err(error),
                Err(_) => return serial(),
            }
        }
        Ok(stitch(collected))
    }
}
