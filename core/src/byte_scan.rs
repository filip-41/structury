//! Stop-set longest-prefix scans: the NEON/SSE2/AVX2 kernels and their fallback.
//!
//! A `StopSet` names the bytes at which a scan must halt. `prefix_len` returns
//! the longest prefix holding none of them. One wide kernel per architecture
//! does whole lanes — NEON on `AArch64`, on x86-64 AVX2 when the runtime probe
//! allows it, the SSE2 baseline otherwise, with a 16-byte SSE2 remainder after
//! the AVX2 lanes. A clean run that ends at a lane edge is finished by one
//! overlapping lane where a kernel exists, else by a scalar tail; other targets
//! run the scalar predicate alone. `lane8` is the module's other export:
//! a portable 8-byte load for byte-at-a-time scans that are not stop sets.
//!
//! The set is compile-time data: `Contract::CHECK` proves the kernel laws
//! (`EQ_LEN <= 8`, `GE != Some(0)`) at every monomorphization, and the alignment
//! oracle checks `prefix_len` against the scalar `StopSet::stop` at every offset
//! and lane boundary, with the x86-only sweep pinning AVX2 to the SSE2 baseline.
//!
//! Lane walks use `as_chunks` so each kernel load is a live `[u8; W]`.

#![allow(
    unsafe_code,
    reason = "NEON/SSE2/AVX2 prefix_len kernels; every unsafe block has a SAFETY note"
)]

/// Seal for [`StopSet`]: `#[doc(hidden)]` so a format codec can name it to
/// register its stop sets.
///
/// This is a **convention, not an enforcement**: the module and trait are public so a third-party crate can still implement `Sealed`
/// and then `StopSet`. [`Contract`] is what keeps a violating set from breaking
/// the kernels — it fails the build, not just a debug assertion.
#[doc(hidden)]
pub mod sealed {
    /// Marker supertrait; implementing it is what makes a type a [`StopSet`](super::StopSet).
    pub trait Sealed {}
}

/// Compile-time proof that `S` satisfies the kernel contract: `EQ_LEN <= 8` (the
/// fixed `EQ` array) and `GE != Some(0)` (the NEON `GE - 1` underflow). Referenced
/// by [`prefix_len`], so a violating set is a build error rather than a release
/// panic or a silently wrong result.
#[doc(hidden)]
pub struct Contract<S>(core::marker::PhantomData<S>);

impl<S: StopSet> Contract<S> {
    /// Fails compilation when `S` breaks the kernel contract.
    pub const CHECK: () = {
        assert!(S::EQ_LEN <= 8, "StopSet::EQ_LEN must be <= 8");
        assert!(
            !matches!(S::GE, Some(0)),
            "StopSet::GE must not be Some(0): the NEON `GE - 1` would underflow"
        );
    };
}

/// A compile-time stop set: the bytes at which a [`prefix_len`] scan must halt.
///
/// A scan stops at exactly the first byte `b` with [`Self::hit`]`(b) == true` and
/// admits every byte before it; the generic alignment oracle checks this for every
/// declared set. `GE` is never `Some(0)` and `EQ_LEN <= 8`; [`Contract`] proves
/// both at compile time for every monomorphization.
///
/// The trait is **sealed by convention** via [`sealed::Sealed`] (see that module:
/// the seal is not enforceable across crates), and [`Contract`] is the hard
/// guarantee.
pub trait StopSet: sealed::Sealed + Copy {
    /// Exact-match stop bytes; only the first [`Self::EQ_LEN`] entries are live.
    const EQ: [u8; 8];
    /// Number of live entries in [`Self::EQ`].
    const EQ_LEN: u8;
    /// Halt on `byte < LT` (`None`: no lower bound). The C0-control shape is `Some(0x20)`.
    const LT: Option<u8>;
    /// Halt on `byte >= GE` (`None`: no upper bound). The non-ASCII shape is `Some(0x80)`.
    const GE: Option<u8>;
    /// True for an all-in-set run (the whitespace shape): a lane is clean iff every byte is one of [`Self::EQ`].
    const ALL: bool;

    /// Whether `byte` is in the stop set; the ground truth for the wide kernels.
    #[must_use]
    #[expect(
        clippy::inline_always,
        reason = "the scalar predicate must fold into the monomorphized scan tails exactly as \
                  the hand-written const predicates did, a non-inlined call per byte would \
                  change the generated code"
    )]
    #[inline(always)]
    fn hit(byte: u8) -> bool {
        if Self::EQ[..usize::from(Self::EQ_LEN)].contains(&byte) {
            return true;
        }
        if let Some(lt) = Self::LT
            && byte < lt
        {
            return true;
        }
        if let Some(ge) = Self::GE
            && byte >= ge
        {
            return true;
        }
        false
    }

    /// Whether a scan must stop at `byte`: [`Self::hit`], or its complement for an all-in-set run.
    #[must_use]
    #[expect(
        clippy::inline_always,
        reason = "see `hit`: the polarity wrapper must fold into the scan tails too"
    )]
    #[inline(always)]
    fn stop(byte: u8) -> bool {
        if Self::ALL { !Self::hit(byte) } else { Self::hit(byte) }
    }
}

/// Longest prefix of `bytes` containing no byte of stop set `S`: whole lanes
/// through the arch kernel, then a scalar tail shorter than a lane. There is
/// deliberately no scalar head — callers that need one run it themselves.
#[must_use]
#[inline]
pub fn prefix_len<S: StopSet>(bytes: &[u8]) -> usize {
    // Every monomorphization proves the kernel contract at compile time.
    let () = Contract::<S>::CHECK;
    let wide = {
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: AArch64 guarantees NEON. `wide` walks live `as_chunks` lanes.
            unsafe { aarch64::wide::<S>(bytes) }
        }
        #[cfg(target_arch = "x86_64")]
        {
            if x86_64::avx2() {
                // SAFETY: the AVX2 kernel requires the feature `avx2()` just verified.
                // `wide` walks live `as_chunks` lanes.
                unsafe { x86_64::avx2::wide::<S>(bytes) }
            } else {
                // SAFETY: x86-64 guarantees SSE2; `wide` walks live `as_chunks` lanes.
                unsafe { x86_64::sse2::wide::<S>(bytes) }
            }
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            0
        }
    };
    // A hitting wide lane already named the exact stop byte; only a clean
    // prefix that ended at a lane edge reaches the tail.
    if wide == bytes.len() || S::stop(bytes[wide]) {
        return wide;
    }
    // One overlapping lane covers the whole remainder: it re-reads up to 16
    // bytes the wide scan verified clean, so any hit it reports lies at or past
    // `wide`. The guard keeps it off inputs shorter than a lane, where the
    // scalar walk is already the whole cost.
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    {
        let rem = bytes.len() - wide;
        if (4..=16).contains(&rem) && bytes.len() >= 16 {
            let lane: &[u8; 16] = bytes[bytes.len() - 16..]
                .first_chunk::<16>()
                .expect("16-byte tail lane");
            #[cfg(target_arch = "aarch64")]
            // SAFETY: AArch64 guarantees NEON; `lane` is 16 live bytes by type.
            let hit = unsafe { aarch64::first_hit::<S>(lane) };
            #[cfg(target_arch = "x86_64")]
            // SAFETY: x86-64 guarantees SSE2; `lane` is 16 live bytes by type.
            let hit = unsafe { x86_64::sse2::first_hit::<S>(lane) };
            return match hit {
                Some(hit) => {
                    debug_assert!(
                        hit + rem >= 16,
                        "the overlap lane hit a byte the wide scan verified clean"
                    );
                    bytes.len() - 16 + hit
                }
                None => bytes.len(),
            };
        }
    }
    wide + bytes[wide..].iter().take_while(|byte| !S::stop(**byte)).count()
}

/// NEON `prefix_len` kernels.
#[cfg(target_arch = "aarch64")]
mod aarch64 {
    use core::arch::aarch64::{vbslq_u8, vceqq_u8, vcltq_u8, vdupq_n_u8, vld1q_u8, vminvq_u8, vmvnq_u8, vorrq_u8};

    const LANE_INDEX: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    /// First byte of a 16-byte lane that is in stop set `S`, or `None` when the
    /// lane is clean; for [`StopSet::ALL`] the hit is the first byte *not* in the set.
    /// # Safety
    ///
    /// Caller is in a `neon` target-feature context. `lane` is 16 live bytes by type.
    #[target_feature(enable = "neon")]
    pub(crate) unsafe fn first_hit<S: super::StopSet>(lane: &[u8; 16]) -> Option<usize> {
        // SAFETY: the caller guarantees 16 readable bytes.
        let v = unsafe { vld1q_u8(lane.as_ptr()) };
        let mut exceptional = vdupq_n_u8(0);
        let mut i = 0;
        while i < S::EQ_LEN {
            exceptional = vorrq_u8(exceptional, vceqq_u8(v, vdupq_n_u8(S::EQ[usize::from(i)])));
            i += 1;
        }
        if let Some(lt) = S::LT {
            exceptional = vorrq_u8(exceptional, vcltq_u8(v, vdupq_n_u8(lt)));
        }
        if let Some(ge) = S::GE {
            // `ge - 1` on the left reproduces the hand-written `0x7f < v` spelling of the non-ASCII shape. A set
            // declaring `GE = Some(0)` would underflow to `0xFF` and silently match nothing — the contract is GE >=
            // 1, which every current set satisfies. `saturating_sub` fails closer to the scalar predicate (`0 < v`)
            // than a wrap to `0xFF` (matches nothing) if a future set ever violates the law; the `debug_assert`
            // catches the violation under test.
            debug_assert!(ge >= 1);
            debug_assert!(S::EQ_LEN <= 8);
            exceptional = vorrq_u8(exceptional, vcltq_u8(vdupq_n_u8(ge.saturating_sub(1)), v));
        }
        // All-in-set (whitespace): a hit is a byte that missed the equality chain. Invert the mask so the select/min
        // below sees 0xFF at those positions, same as the ordinary stop-set polarity.
        let hit = if S::ALL { vmvnq_u8(exceptional) } else { exceptional };
        // SAFETY: `LANE_INDEX` is 16 live bytes.
        let indices = unsafe { vld1q_u8(LANE_INDEX.as_ptr()) };
        let selected = vbslq_u8(hit, indices, vdupq_n_u8(16));
        let first = vminvq_u8(selected);
        (first < 16).then_some(usize::from(first))
    }

    /// Longest prefix containing no byte of `S`, over whole 16-byte lanes.
    /// # Safety
    ///
    /// Caller is in a `neon` target-feature context. Each `as_chunks` lane is 16 live bytes.
    #[target_feature(enable = "neon")]
    pub(crate) unsafe fn wide<S: super::StopSet>(bytes: &[u8]) -> usize {
        let mut offset = 0_usize;
        let (chunks, _) = bytes.as_chunks::<16>();
        for lane in chunks {
            // SAFETY: neon target-feature; `lane` is 16 live bytes by type.
            if let Some(hit) = unsafe { first_hit::<S>(lane) } {
                return offset + hit;
            }
            offset += 16;
        }
        offset
    }
}

/// SSE2/AVX2 `prefix_len` kernels.
#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use core::sync::atomic::{AtomicU8, Ordering};

    /// Whether the current CPU exposes AVX2, probed once and cached. The wider
    /// 256-bit kernels run when it does, the SSE2 baseline otherwise.
    #[inline]
    pub fn avx2() -> bool {
        match AVX2.load(Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => {
                let detected = detect_avx2();
                AVX2.store(if detected { 1 } else { 2 }, Ordering::Relaxed);
                detected
            }
        }
    }

    static AVX2: AtomicU8 = AtomicU8::new(0);

    /// CPUID/XGETBV AVX2 detection, replicating `is_x86_feature_detected!("avx2")`
    /// (std-only; this crate is `no_std`). The leaf-7 query is gated on the max
    /// standard leaf: a CPU without leaf 7 echoes garbage in EBX, and bit 5 of it
    /// would dispatch AVX2 on a machine that cannot execute it.
    fn detect_avx2() -> bool {
        // Miri cannot execute the CPUID/XGETBV inline assembly, so it always
        // takes the SSE2 baseline.
        if cfg!(miri) {
            return false;
        }
        let basic = core::arch::x86_64::__cpuid(1);
        let osxsave = (basic.ecx & (1 << 27)) != 0;
        let avx = (basic.ecx & (1 << 28)) != 0;
        if !(osxsave && avx) {
            return false;
        }
        // SAFETY: XGETBV is safe to execute whenever OSXSAVE is set, which is exactly the guard checked above.
        let xcr0 = unsafe { core::arch::x86_64::_xgetbv(0) };
        if xcr0 & 0x6 != 0x6 {
            return false;
        }
        // Leaf 7 exists only when the max standard leaf reports it; std's replicated detector checks this before
        // querying, and so does this. (__cpuid(1).eax is the version signature, not the max leaf.)
        let max_leaf = core::arch::x86_64::__cpuid(0).eax;
        if max_leaf < 7 {
            return false;
        }
        let extended = core::arch::x86_64::__cpuid(7);
        (extended.ebx & (1 << 5)) != 0
    }

    /// The 128-bit baseline kernel family (SSE2 is an x86-64 guarantee).
    pub mod sse2 {
        #![expect(
            clippy::cast_ptr_alignment,
            reason = "every pointer cast here feeds `_mm_loadu_si128`, whose load has no alignment precondition"
        )]

        use core::arch::x86_64::{
            __m128i, _mm_cmpeq_epi8, _mm_cmpgt_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_or_si128, _mm_set1_epi8,
            _mm_setzero_si128, _mm_xor_si128,
        };

        const SIGN: i8 = i8::MIN; // 0x80

        /// Unsigned `v < c`, via the xor-sign trick (SSE2's comparison is signed).
        ///
        /// The constant is the LEFT operand; passing `v` first would compute the
        /// opposite predicate from the same two instructions.
        /// # Safety
        ///
        /// SSE2 is the x86-64 baseline this module is `cfg`-gated on.
        unsafe fn lt_u(v: __m128i, c: u8) -> __m128i {
            // SAFETY: SSE2 is guaranteed by the x86-64 baseline this module is
            // `cfg`-gated on, so these intrinsics are always available; they are
            // pure register operations and touch no memory.
            unsafe {
                _mm_cmpgt_epi8(
                    _mm_set1_epi8((c ^ 0x80).cast_signed()),
                    _mm_xor_si128(v, _mm_set1_epi8(SIGN)),
                )
            }
        }

        /// Unsigned `v >= c`.
        /// # Safety
        ///
        /// SSE2 is the x86-64 baseline this module is `cfg`-gated on.
        unsafe fn ge_u(v: __m128i, c: u8) -> __m128i {
            // SAFETY: `lt_u` needs only the SSE2 baseline this module is gated on,
            // and takes register values with no further precondition.
            let lt = unsafe { lt_u(v, c) };
            // SAFETY: SSE2 baseline as above; pure register operations.
            unsafe { _mm_cmpeq_epi8(lt, _mm_setzero_si128()) }
        }

        /// First byte of a 16-byte lane that is in stop set `S`, or `None` when the
        /// lane is clean; for [`super::super::StopSet::ALL`] the hit is the first
        /// byte *not* in the set.
        #[expect(
            clippy::inline_always,
            reason = "the fixed 16-byte lane kernel must fold into the scan loop; the lint's \
                      general size heuristic does not apply to the fixed-width SIMD compare chain"
        )]
        #[inline(always)]
        /// # Safety
        ///
        /// Caller is in an SSE2 context (x86-64 baseline). `lane` is 16 live bytes by type.
        pub(crate) unsafe fn first_hit<S: super::super::StopSet>(lane: &[u8; 16]) -> Option<usize> {
            // The two descriptor laws the trait documents, mirrored from the NEON twin: `GE` is never
            // `Some(0)` (it would make every byte a hit) and `EQ_LEN` indexes the fixed 8-byte array.
            debug_assert_ne!(S::GE, Some(0));
            debug_assert!(S::EQ_LEN <= 8);
            // SAFETY: the caller guarantees 16 readable bytes.
            let v = unsafe { _mm_loadu_si128(lane.as_ptr().cast::<__m128i>()) };
            // SAFETY: intrinsic calls are unsafe operations; the lane pointer
            // is valid and the surrounding kernels guarantee the loads.
            let mut exceptional = unsafe { _mm_setzero_si128() };
            let mut i = 0;
            while i < S::EQ_LEN {
                // SAFETY: register-only compares under the same target-feature
                // and live-lane precondition as the load.
                exceptional = unsafe {
                    _mm_or_si128(
                        exceptional,
                        _mm_cmpeq_epi8(v, _mm_set1_epi8(S::EQ[usize::from(i)].cast_signed())),
                    )
                };
                i += 1;
            }
            // SAFETY: each arm is register-only under the SSE2 baseline and
            // the live-lane load above; no further memory is touched.
            let range = match (S::LT, S::GE) {
                (Some(0x20), Some(0x80)) => {
                    // Signed negative lanes are non-ASCII and compare below 0x20.
                    unsafe { _mm_cmpgt_epi8(_mm_set1_epi8(0x20), v) }
                }
                (Some(lt), None) => unsafe { lt_u(v, lt) },
                (None, Some(ge)) => unsafe { ge_u(v, ge) },
                (Some(lt), Some(ge)) => unsafe { _mm_or_si128(lt_u(v, lt), ge_u(v, ge)) },
                (None, None) => unsafe { _mm_setzero_si128() },
            };
            // SAFETY: register-only combine of the compares above.
            exceptional = unsafe { _mm_or_si128(exceptional, range) };
            // SAFETY: register-only movemask of `exceptional`; sets the low 16 bits, one per lane.
            let mask = unsafe { _mm_movemask_epi8(exceptional) }.cast_unsigned();
            let bits = if S::ALL {
                // All-in-set: the mask is 0xFFFF when every byte is in the set; a hit is the first clear bit.
                (!mask) & 0xFFFF
            } else {
                mask
            };
            (bits != 0).then_some(bits.trailing_zeros() as usize)
        }

        /// Longest prefix containing no byte of `S`, over whole 16-byte lanes.
        #[expect(
            clippy::inline_always,
            reason = "the fixed-width lane walk must fold into the scan loop; a non-inlined call \
                      per lane would change the generated code (see `first_hit`)"
        )]
        #[inline(always)]
        /// # Safety
        ///
        /// Caller is in an SSE2 context. Each `as_chunks` lane is 16 live bytes.
        pub(crate) unsafe fn wide<S: super::super::StopSet>(bytes: &[u8]) -> usize {
            let mut offset = 0_usize;
            let (chunks, _) = bytes.as_chunks::<16>();
            for lane in chunks {
                // SAFETY: SSE2 baseline; `lane` is 16 live bytes by type.
                if let Some(hit) = unsafe { first_hit::<S>(lane) } {
                    return offset + hit;
                }
                offset += 16;
            }
            offset
        }
    }

    /// The 256-bit kernel family behind the runtime `avx2()` probe.
    pub mod avx2 {
        #![expect(
            clippy::cast_ptr_alignment,
            reason = "every pointer cast here feeds `_mm256_loadu_si256`, whose load has no alignment precondition"
        )]
        #![allow(
            unused_unsafe,
            reason = "rustc 1.96 marks the x86_64 intrinsics safe, so the kernels' explicit \
                      `unsafe` is redundant and the lint fires (checked on linux-gnu, \
                      apple-darwin and windows-msvc); `allow`, not `expect`, so a target or \
                      toolchain that still treats the intrinsics as unsafe, where the blocks \
                      are required, does not fail the build"
        )]

        use core::arch::x86_64::{
            __m256i, _mm256_cmpeq_epi8, _mm256_cmpgt_epi8, _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_or_si256,
            _mm256_set1_epi8, _mm256_setzero_si256, _mm256_xor_si256,
        };

        const SIGN: i8 = i8::MIN; // 0x80

        /// Unsigned `v < c`, via the xor-sign trick; see the SSE2 [`sse2::lt_u`] twin.
        ///
        /// # Safety
        ///
        /// Caller is in an AVX2-enabled function.
        #[target_feature(enable = "avx2")]
        #[inline]
        unsafe fn lt_u(v: __m256i, c: u8) -> __m256i {
            // SAFETY: called from `#[target_feature(enable = "avx2")]` kernels
            // only; pure register operations with no memory touched.
            unsafe {
                _mm256_cmpgt_epi8(
                    _mm256_set1_epi8((c ^ 0x80).cast_signed()),
                    _mm256_xor_si256(v, _mm256_set1_epi8(SIGN)),
                )
            }
        }

        /// Unsigned `v >= c`.
        ///
        /// # Safety
        ///
        /// Caller is in an AVX2-enabled function.
        #[target_feature(enable = "avx2")]
        #[inline]
        unsafe fn ge_u(v: __m256i, c: u8) -> __m256i {
            // SAFETY: `lt_u` is register-only under the same AVX2 target-feature
            // precondition as this function.
            let lt = unsafe { lt_u(v, c) };
            // SAFETY: register-only; `lt_u` already holds under the same target-feature precondition.
            unsafe { _mm256_cmpeq_epi8(lt, _mm256_setzero_si256()) }
        }

        /// First byte of a 32-byte lane that is in stop set `S`, or `None` when the
        /// lane is clean; for [`super::super::StopSet::ALL`] the hit is the first
        /// byte *not* in the set.
        #[target_feature(enable = "avx2")]
        /// # Safety
        ///
        /// Caller is in an AVX2-enabled function. `lane` is 32 live bytes by type.
        pub(crate) unsafe fn first_hit<S: super::super::StopSet>(lane: &[u8; 32]) -> Option<usize> {
            // The two descriptor laws the trait documents, mirrored from the NEON twin: `GE` is never
            // `Some(0)` (it would make every byte a hit) and `EQ_LEN` indexes the fixed 8-byte array.
            debug_assert_ne!(S::GE, Some(0));
            debug_assert!(S::EQ_LEN <= 8);
            // SAFETY: the caller guarantees 32 readable bytes.
            let v = unsafe { _mm256_loadu_si256(lane.as_ptr().cast::<__m256i>()) };
            let mut exceptional = _mm256_setzero_si256();
            let mut i = 0;
            while i < S::EQ_LEN {
                // SAFETY: register-only compares under the same target-feature
                // and live-lane precondition as the load.
                exceptional = unsafe {
                    _mm256_or_si256(
                        exceptional,
                        _mm256_cmpeq_epi8(v, _mm256_set1_epi8(S::EQ[usize::from(i)].cast_signed())),
                    )
                };
                i += 1;
            }
            let range = match (S::LT, S::GE) {
                (Some(0x20), Some(0x80)) => {
                    // Signed negative lanes are non-ASCII and compare below 0x20.
                    _mm256_cmpgt_epi8(_mm256_set1_epi8(0x20), v)
                }
                // SAFETY: register-only under the same target-feature / lane-valid
                // precondition as the load.
                (Some(lt), None) => unsafe { lt_u(v, lt) },
                (None, Some(ge)) => unsafe { ge_u(v, ge) },
                (Some(lt), Some(ge)) => unsafe { _mm256_or_si256(lt_u(v, lt), ge_u(v, ge)) },
                (None, None) => _mm256_setzero_si256(),
            };
            // SAFETY: register-only combine of the compares above.
            exceptional = unsafe { _mm256_or_si256(exceptional, range) };
            // SAFETY: register-only movemask of `exceptional`; 32 bits, all-ones when every byte matched.
            let mask = unsafe { _mm256_movemask_epi8(exceptional) }.cast_unsigned();
            let bits = if S::ALL { !mask } else { mask };
            (bits != 0).then_some(bits.trailing_zeros() as usize)
        }

        /// Longest prefix containing no byte of `S`: 32-byte lanes, then one 16-byte SSE2 remainder.
        /// # Safety
        ///
        /// Caller is in an AVX2-enabled function. Each `as_chunks` lane is 32 or 16 live bytes.
        #[target_feature(enable = "avx2")]
        pub(crate) unsafe fn wide<S: super::super::StopSet>(bytes: &[u8]) -> usize {
            let mut offset = 0_usize;
            let (chunks, rest) = bytes.as_chunks::<32>();
            for lane in chunks {
                // SAFETY: AVX2 target-feature; `lane` is 32 live bytes by type.
                if let Some(hit) = unsafe { first_hit::<S>(lane) } {
                    return offset + hit;
                }
                offset += 32;
            }
            let (rest16, _) = rest.as_chunks::<16>();
            for lane in rest16 {
                // SAFETY: AVX2 implies SSE2; `lane` is 16 live bytes by type.
                if let Some(hit) = unsafe { super::sse2::first_hit::<S>(lane) } {
                    return offset + hit;
                }
                offset += 16;
            }
            offset
        }
    }
}

/// Portable 8-byte SWAR lane for the scans that are not stop sets: one 8-byte
/// load per step, no arch intrinsics. `None` when fewer than 8 bytes remain at `at`.
#[must_use]
#[inline]
pub fn lane8(bytes: &[u8], at: usize) -> Option<u64> {
    bytes
        .get(at..)
        .and_then(|rest| rest.first_chunk::<8>().map(|&lane| u64::from_le_bytes(lane)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn mix(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// One test set per descriptor shape the kernels branch on.
    #[derive(Clone, Copy)]
    struct EqOnly;
    impl StopSet for EqOnly {
        const EQ: [u8; 8] = [b'"', b'{', b'[', b'}', b']', 0, 0, 0];
        const EQ_LEN: u8 = 5;
        const LT: Option<u8> = None;
        const GE: Option<u8> = None;
        const ALL: bool = false;
    }

    /// EQ chain plus a C0-control lower bound.
    #[derive(Clone, Copy)]
    struct EqLt;
    impl StopSet for EqLt {
        const EQ: [u8; 8] = [b'"', b'\\', 0x7f, 0, 0, 0, 0, 0];
        const EQ_LEN: u8 = 3;
        const LT: Option<u8> = Some(0x20);
        const GE: Option<u8> = None;
        const ALL: bool = false;
    }

    /// EQ chain plus the `(0x20, 0x80)` pair the SSE2/AVX2 kernels special-case.
    #[derive(Clone, Copy)]
    struct EqLtGe;
    impl StopSet for EqLtGe {
        const EQ: [u8; 8] = [b'"', b'\\', 0x7f, 0, 0, 0, 0, 0];
        const EQ_LEN: u8 = 3;
        const LT: Option<u8> = Some(0x20);
        const GE: Option<u8> = Some(0x80);
        const ALL: bool = false;
    }

    /// All-in-set run: the stop set is the complement of EQ.
    #[derive(Clone, Copy)]
    struct AllInSet;
    impl StopSet for AllInSet {
        const EQ: [u8; 8] = [b' ', b'\t', b'\n', b'\r', 0, 0, 0, 0];
        const EQ_LEN: u8 = 4;
        const LT: Option<u8> = None;
        const GE: Option<u8> = None;
        const ALL: bool = true;
    }

    /// Upper-bound only, over an empty EQ chain.
    #[derive(Clone, Copy)]
    struct GeOnly;
    impl StopSet for GeOnly {
        const EQ: [u8; 8] = [0; 8];
        const EQ_LEN: u8 = 0;
        const LT: Option<u8> = None;
        const GE: Option<u8> = Some(0x80);
        const ALL: bool = false;
    }

    /// Lower-bound only, over an empty EQ chain.
    #[derive(Clone, Copy)]
    struct LtOnly;
    impl StopSet for LtOnly {
        const EQ: [u8; 8] = [0; 8];
        const EQ_LEN: u8 = 0;
        const LT: Option<u8> = Some(0x20);
        const GE: Option<u8> = None;
        const ALL: bool = false;
    }

    /// General two-sided range, not the `(0x20, 0x80)` special case.
    #[derive(Clone, Copy)]
    struct Range;
    impl StopSet for Range {
        const EQ: [u8; 8] = [0; 8];
        const EQ_LEN: u8 = 0;
        const LT: Option<u8> = Some(0x10);
        const GE: Option<u8> = Some(0x70);
        const ALL: bool = false;
    }

    /// A single EQ byte.
    #[derive(Clone, Copy)]
    struct EqSingle;
    impl StopSet for EqSingle {
        const EQ: [u8; 8] = [0x1e, 0, 0, 0, 0, 0, 0, 0];
        const EQ_LEN: u8 = 1;
        const LT: Option<u8> = None;
        const GE: Option<u8> = None;
        const ALL: bool = false;
    }

    /// Every EQ slot live.
    #[derive(Clone, Copy)]
    struct EqFull;
    impl StopSet for EqFull {
        const EQ: [u8; 8] = *b"01234567";
        const EQ_LEN: u8 = 8;
        const LT: Option<u8> = None;
        const GE: Option<u8> = None;
        const ALL: bool = false;
    }

    /// All-in-set with range terms: the interaction the basic sets miss.
    #[derive(Clone, Copy)]
    struct AllInRange;
    impl StopSet for AllInRange {
        const EQ: [u8; 8] = [b' ', b'\t', b'\n', b'\r', 0, 0, 0, 0];
        const EQ_LEN: u8 = 4;
        const LT: Option<u8> = Some(0x20);
        const GE: Option<u8> = Some(0x80);
        const ALL: bool = true;
    }

    /// Lower bound one past zero, the narrowest bound a `GE - 1` spelling can
    /// underflow.
    #[derive(Clone, Copy)]
    struct GeOne;
    impl StopSet for GeOne {
        const EQ: [u8; 8] = [0; 8];
        const EQ_LEN: u8 = 0;
        const LT: Option<u8> = None;
        const GE: Option<u8> = Some(1);
        const ALL: bool = false;
    }

    /// The upper bound at the top of the byte range.
    #[derive(Clone, Copy)]
    struct GeMax;
    impl StopSet for GeMax {
        const EQ: [u8; 8] = [0; 8];
        const EQ_LEN: u8 = 0;
        const LT: Option<u8> = None;
        const GE: Option<u8> = Some(0xFF);
        const ALL: bool = false;
    }

    /// The lower bound at the top of the byte range.
    #[derive(Clone, Copy)]
    struct LtMax;
    impl StopSet for LtMax {
        const EQ: [u8; 8] = [0; 8];
        const EQ_LEN: u8 = 0;
        const LT: Option<u8> = Some(0xFF);
        const GE: Option<u8> = None;
        const ALL: bool = false;
    }

    impl sealed::Sealed for EqOnly {}
    impl sealed::Sealed for EqLt {}
    impl sealed::Sealed for EqLtGe {}
    impl sealed::Sealed for AllInSet {}
    impl sealed::Sealed for GeOnly {}
    impl sealed::Sealed for LtOnly {}
    impl sealed::Sealed for Range {}
    impl sealed::Sealed for EqSingle {}
    impl sealed::Sealed for EqFull {}
    impl sealed::Sealed for AllInRange {}
    impl sealed::Sealed for GeOne {}
    impl sealed::Sealed for GeMax {}
    impl sealed::Sealed for LtMax {}

    /// Every declared stop set, driven through the alignment oracle below.
    const DECLARED_SETS: &[&dyn SetOracle] = &[
        &EqOnly,
        &EqLt,
        &EqLtGe,
        &AllInSet,
        &GeOnly,
        &LtOnly,
        &Range,
        &EqSingle,
        &EqFull,
        &AllInRange,
        &GeOne,
        &GeMax,
        &LtMax,
    ];

    /// Object-safe shim so the oracle can drive heterogeneous sets from one loop.
    trait SetOracle {
        fn check(&self, bytes: &[u8], start: usize, end: usize);
        #[cfg(target_arch = "x86_64")]
        fn check_sse2_avx2_identity(&self, bytes: &[u8], start: usize, end: usize);
    }

    impl<S: StopSet> SetOracle for S {
        fn check(&self, bytes: &[u8], start: usize, end: usize) {
            let slice = &bytes[start..end];
            let wide = prefix_len::<S>(slice);
            let expected = slice.iter().take_while(|b| !S::stop(**b)).count();
            assert_eq!(
                wide,
                expected,
                "prefix_len mismatch for {} at {start}..{end} of {bytes:?}",
                core::any::type_name::<S>(),
            );
        }

        #[cfg(target_arch = "x86_64")]
        fn check_sse2_avx2_identity(&self, bytes: &[u8], start: usize, end: usize) {
            let slice = &bytes[start..end];
            // SAFETY: the caller gates on `x86_64::avx2()`, so both kernel
            // families' features are present; `wide` walks live `as_chunks` lanes.
            let sse2_run = unsafe {
                let wide = x86_64::sse2::wide::<S>(slice);
                wide + slice[wide..].iter().take_while(|b| !S::stop(**b)).count()
            };
            let avx2_run = unsafe {
                let wide = x86_64::avx2::wide::<S>(slice);
                wide + slice[wide..].iter().take_while(|b| !S::stop(**b)).count()
            };
            assert_eq!(
                sse2_run,
                avx2_run,
                "{} diverged at {start}..{end} of {bytes:?}",
                core::any::type_name::<S>(),
            );
        }
    }

    /// `prefix_len` must agree with the scalar predicate at every alignment and
    /// length up to the corpus cap, for every declared set.
    #[test]
    fn every_declared_set_agrees_with_its_scalar_predicate_at_every_alignment() {
        // Hand-picked adversarial seeds per shape, then pseudo-random corpora biased toward the sets' own bytes so runs
        // cross the 16-byte lane boundary and end in the scalar tail often.
        for set in DECLARED_SETS {
            let mut corpus: Vec<Vec<u8>> = vec![
                Vec::new(),
                b"a".to_vec(),
                b"\"".to_vec(),
                b"\\".to_vec(),
                b"\x1f".to_vec(),
                b"\x20".to_vec(),
                b"\x7f".to_vec(),
                b"\x80".to_vec(),
                b"plain text".to_vec(),
                b"{\"k\": 1}".to_vec(),
                b" \t\n\r a".to_vec(),
                b"a,b\"c\r\nd".to_vec(),
                b"<tag>&amp;</tag>".to_vec(),
                b"\x00\x1f\x7f\xff".to_vec(),
            ];
            let mut state = 0x9e37_79b9_7f4a_7c15_u64;
            for len in 0..48 {
                let mut bytes = Vec::with_capacity(len);
                for _ in 0..len {
                    let r = mix(&mut state);
                    bytes.push(match r % 8 {
                        0..=3 => b" \t\n\r\"'<&,;[{\x00\x7f\xef\xf0"[((r >> 8) % 16) as usize],
                        _ => ((r >> 16) & 0xFF) as u8,
                    });
                }
                corpus.push(bytes);
            }
            for bytes in &corpus {
                for start in 0..=bytes.len().min(3) {
                    for end in start..=bytes.len().min(start + 48) {
                        set.check(bytes, start, end);
                    }
                }
            }
        }
    }

    /// Each stop-set member placed at every position around the first and second
    /// lane boundaries, for every declared set.
    #[test]
    fn declared_sets_agree_at_lane_boundaries() {
        let terminators: &[u8] = &[
            b'"', b'\\', b'<', b'&', b',', b'\t', b'\r', b'\n', b'}', b']', 0x00, 0x01, 0x1f, 0x7f, 0x80, 0xff,
        ];
        for set in DECLARED_SETS {
            for &terminator in terminators {
                for position in 0..40 {
                    let mut bytes = vec![b'a'; 40];
                    bytes[position] = terminator;
                    set.check(&bytes, 0, bytes.len());
                }
            }
        }
    }

    /// The AVX2 kernels must be byte-identical to the SSE2 baseline, over every
    /// declared set. A no-op on a machine without AVX2.
    #[cfg(target_arch = "x86_64")]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the AVX2-vs-SSE2 byte-identity sweep drives the same adversarial corpora \
                  through every kernel pair sequentially — one long sequential test is the \
                  contract, and only an x86_64 clippy can even see it (the linux-amd64 lane)"
    )]
    fn avx2_and_sse2_kernels_are_byte_identical() {
        if !x86_64::avx2() {
            return;
        }
        let mut state = 0x2d1b_5a64_9f3c_77e1_u64;
        for set in DECLARED_SETS {
            for len in 0..160 {
                let mut bytes = Vec::with_capacity(len);
                for _ in 0..len {
                    let r = mix(&mut state);
                    bytes.push((r & 0xFF) as u8);
                }
                set.check_sse2_avx2_identity(&bytes, 0, len);
            }
        }
    }
}
