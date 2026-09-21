//! Source-backed whole-document build: the source-slice payload strategy.
//! An RFC 8259 document copies no payload byte.

use alloc::string::String;
use alloc::vec::Vec;

use structury::{BorrowedDocument, ErrorClass, Node};

use crate::tape::Payload;

/// Source-backed payload strategy: keep a source span when one spells the payload, spill the rest.
pub(crate) struct BorrowedPayload<'src> {
    source: &'src [u8],
    spill: String,
    /// Absolute start of the span being lexed, so a reused slice's offset is an absolute source offset.
    base: usize,
}

impl<'src> Payload<'src> for BorrowedPayload<'src> {
    type Doc = BorrowedDocument<'src>;

    fn new(src: &'src [u8]) -> Self {
        Self {
            source: src,
            spill: String::new(),
            base: 0,
        }
    }

    fn start_span(&mut self, base: usize) {
        self.base = base;
    }

    #[inline]
    fn store(&mut self, bytes: &[u8], source: Option<(usize, usize)>) -> Result<(u32, u32), structury::Error> {
        if let Some((pos, len)) = source {
            let off = u32::try_from(self.base + pos).map_err(|_| Self::overflow())?;
            let len = u32::try_from(len).map_err(|_| Self::overflow())?;
            return Ok((off, len));
        }
        let start = self.source.len() + self.spill.len();
        let off = u32::try_from(start).map_err(|_| Self::overflow())?;
        let len = u32::try_from(bytes.len()).map_err(|_| Self::overflow())?;
        if start + bytes.len() > u32::MAX as usize {
            return Err(Self::overflow());
        }
        self.spill
            .push_str(core::str::from_utf8(bytes).expect("payload bytes are UTF-8"));
        Ok((off, len))
    }

    /// Payload bytes at absolute `off..off + len` in `source ++ spill`.
    #[inline]
    fn payload(&self, off: u32, len: u32) -> &[u8] {
        let start = off as usize;
        let Some(end) = start.checked_add(len as usize) else {
            return &[];
        };
        if start < self.source.len() {
            self.source.get(start..end).unwrap_or(&[])
        } else {
            let spill_start = start - self.source.len();
            let Some(spill_end) = spill_start.checked_add(len as usize) else {
                return &[];
            };
            self.spill.as_bytes().get(spill_start..spill_end).unwrap_or(&[])
        }
    }

    fn overflow() -> structury::Error {
        structury::Error::new(
            ErrorClass::Limit,
            "borrowed-capacity",
            "borrowed view exceeds 32-bit offsets",
            0,
        )
    }

    fn into_doc(self, nodes: Vec<Node>, edges: Vec<u32>, root: u32) -> BorrowedDocument<'src> {
        BorrowedDocument::from_parts(self.source, nodes, edges, self.spill, root)
    }
}
