//! Compact owned UTF-8 string: short strings stay inline, longer ones spill to
//! the heap. Built only from `str`, so the inline bytes are always valid UTF-8.

use alloc::boxed::Box;
use alloc::string::String;
use core::borrow::Borrow;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::ops::Deref;

/// Bytes held before spilling; with the length byte this keeps [`CompactStr`] at `size_of::<String>()`.
const INLINE_CAP: usize = 22;

/// Owned UTF-8 string with inline storage for short values and a heap fallback.
/// Equal, ordered, and hashed as its [`str`] contents.
#[derive(Clone)]
pub struct CompactStr(Repr);

#[derive(Clone)]
enum Repr {
    Inline { len: u8, bytes: [u8; INLINE_CAP] },
    Heap(Box<str>),
}

impl CompactStr {
    fn from_str(s: &str) -> Self {
        let bytes = s.as_bytes();
        match u8::try_from(bytes.len()) {
            Ok(len) if usize::from(len) <= INLINE_CAP => {
                let mut buf = [0u8; INLINE_CAP];
                buf[..bytes.len()].copy_from_slice(bytes);
                Self(Repr::Inline { len, bytes: buf })
            }
            _ => Self(Repr::Heap(Box::from(s))),
        }
    }

    /// Borrowed contents.
    ///
    /// # Panics
    ///
    /// Panics if inline bytes are not UTF-8. Constructors only accept `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            Repr::Inline { len, bytes } => {
                core::str::from_utf8(&bytes[..usize::from(*len)]).expect("CompactStr inline bytes are UTF-8")
            }
            Repr::Heap(s) => s,
        }
    }

    /// Raw UTF-8 bytes without re-validating the inline form.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match &self.0 {
            Repr::Inline { len, bytes } => &bytes[..usize::from(*len)],
            Repr::Heap(s) => s.as_bytes(),
        }
    }

    /// Length in bytes.
    ///
    /// `as_bytes` cannot be `const` on this channel — range indexing is not yet
    /// const-stable (rust-lang #143874) — so the length is computed directly.
    #[must_use]
    #[allow(clippy::cast_lossless, reason = "usize::from is not const; a u8 lossless cast is")]
    pub const fn len(&self) -> usize {
        match &self.0 {
            Repr::Inline { len, .. } => *len as usize,
            Repr::Heap(s) => s.len(),
        }
    }

    /// Whether the string is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Deref for CompactStr {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for CompactStr {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for CompactStr {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for CompactStr {
    fn from(s: &str) -> Self {
        Self::from_str(s)
    }
}

impl From<String> for CompactStr {
    fn from(s: String) -> Self {
        if s.len() <= INLINE_CAP {
            Self::from_str(&s)
        } else {
            Self(Repr::Heap(s.into_boxed_str()))
        }
    }
}

impl PartialEq for CompactStr {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<str> for CompactStr {
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<&str> for CompactStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for CompactStr {}

impl Ord for CompactStr {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for CompactStr {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for CompactStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Display for CompactStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for CompactStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}
