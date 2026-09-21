use super::{
    Answer, BTreeMap, ByteRange, Check, Dialect, Hit, Predicate, RecordSink, ResolvedPredicate, Scan, SmallErr, String,
    Vec, View, Walker, apply_object_row, error, num_cmp, skip_trivia_here, span_eq_value,
};

impl<'src, S: Scan, R: RecordSink, const CONTROLLED: bool> Walker<'src, '_, '_, S, CONTROLLED, R> {
    #[allow(clippy::too_many_lines)] // single-pass member loop; the retained-shape arm shares its locals
    pub(super) fn keep_object(
        &mut self,
        wanted_keys: &Wanted<'_>,
        hits: &[Hit<'_>],
        marks: &mut [Answer<'src>],
    ) -> Result<(), SmallErr> {
        let start = self.pos;
        self.enter_container(start)?;
        let bytes = self.bytes;
        let dialect = self.dialect;
        let child_depth = self.depth;
        let max = self.max;
        let demanded = self.demanded();
        let unread = self.unread();
        let mut pos = start + 1;
        // A retained shape reuses the previous row's member heads and slots; a
        // cold or mismatched layout is rebuilt from this row. `begin_row` (not
        // `clear`) keeps the demanded keys' entry slots stable, so the
        // content-keyed head index can keep writing them across rows.
        let mut using = self.row_layout.valid;
        let mut building = !using;
        if building {
            self.row_layout.positional.clear();
        }
        self.members.begin_row();
        let mut members_seen = 0usize;
        let mut seen_member = false;
        loop {
            self.poll(pos)?;
            pos = skip_trivia_here::<S>(bytes, pos, dialect)?;
            let Some(&byte) = bytes.get(pos) else {
                self.pos = pos;
                return Err(error::expected_key(pos));
            };
            if byte == b'}' {
                pos += 1;
                break;
            }
            if seen_member {
                if byte != b',' {
                    self.pos = pos;
                    return Err(error::expected_comma_object(pos));
                }
                pos = skip_trivia_here::<S>(bytes, pos + 1, dialect)?;
                if bytes.get(pos) == Some(&b'}') {
                    if dialect.trailing_commas() {
                        pos += 1;
                        break;
                    }
                    self.pos = pos;
                    return Err(error::trailing_comma(pos));
                }
            }
            let member_start = pos;
            if using {
                let matched = self
                    .row_layout
                    .positional
                    .get(members_seen)
                    .and_then(|head| head_matches::<S>(head, bytes, member_start).then_some((head.slot, head.len)));
                if let Some((slot, len)) = matched {
                    pos = member_start + len;
                    let val_start = pos;
                    let check = if slot.is_some() { demanded } else { unread };
                    pos = S::skip_present_at(bytes, pos, check, child_depth, max, dialect)?;
                    if let Some(slot) = slot {
                        let entry = &mut self.members.entries[slot];
                        entry.span = ByteRange::try_new(val_start, pos).expect("ordered");
                        entry.present = true;
                    }
                    seen_member = true;
                    members_seen += 1;
                    continue;
                }
                // The row diverges at `members_seen`: keep the prefix this row
                // matched and rebuild the rest of the retained order from it, so
                // the next row of the new shape takes the positional path.
                using = false;
                building = true;
                self.row_layout.positional.truncate(members_seen);
                self.row_layout.valid = false;
            }
            // Content-keyed probe: a rotated or partial row matches a retained
            // head by bytes, without re-tokenizing the key.
            if let Some((head, slot)) = self.row_layout.head_at::<S>(bytes, member_start) {
                pos = member_start + head.len;
                let val_start = pos;
                let check = if slot.is_some() { demanded } else { unread };
                pos = S::skip_present_at(bytes, pos, check, child_depth, max, dialect)?;
                if let Some(slot) = slot {
                    let entry = &mut self.members.entries[slot];
                    entry.span = ByteRange::try_new(val_start, pos).expect("ordered");
                    entry.present = true;
                }
                // A row rebuilding its retained order takes this head with it: a
                // rotated or partial row converges on the positional path from
                // the next identical row on.
                if building {
                    self.row_layout.positional.push(head);
                }
                seen_member = true;
                members_seen += 1;
                continue;
            }
            let (owned_key, key_inner) = if let Some((span, value_at)) = S::plain_member(bytes, pos) {
                pos = value_at;
                (None, Some(span))
            } else {
                let (owned, inner, key_end) = read_key::<S>(bytes, pos, dialect)?;
                pos = S::skip_trivia(bytes, key_end, dialect)?;
                if bytes.get(pos) != Some(&b':') {
                    self.pos = pos;
                    return Err(error::expected_colon(pos));
                }
                pos = S::skip_trivia(bytes, pos + 1, dialect)?;
                (owned, inner)
            };
            let key_bytes: &[u8] = match (&owned_key, key_inner) {
                (_, Some(span)) => bytes.get(span.start()..span.end()).unwrap_or(&[]),
                (Some(s), None) => s.as_bytes(),
                (None, None) => &[],
            };
            let val_start = pos;
            let wanted = wanted_keys.contains(key_bytes, hits);
            let check = if wanted { demanded } else { unread };
            // One strategy dispatch per member value; the kind-specific arms in
            // the scan would re-enter the same `value` match.
            pos = S::skip_present_at(bytes, pos, check, child_depth, max, dialect)?;
            seen_member = true;
            let span = ByteRange::try_new(val_start, pos).expect("ordered");
            let slot = if wanted {
                Some(
                    self.members
                        .insert(bytes, member_name(key_inner, key_bytes), span, true),
                )
            } else {
                None
            };
            match key_inner {
                Some(_) => {
                    let head = Head::new(bytes, member_start, val_start, slot);
                    if building {
                        self.row_layout.positional.push(head);
                    }
                    self.row_layout.learn(bytes, head);
                }
                None => {
                    // A decoded key has no source span to retain.
                    building = false;
                }
            }
            members_seen += 1;
        }
        self.pos = pos;
        self.depth = self.depth.saturating_sub(1);
        self.row_layout.valid = building || (using && members_seen == self.row_layout.positional.len());
        let obj_span = ByteRange::try_new(start, self.pos).expect("ordered");
        for h in hits {
            apply_object_row(
                &mut marks[h.idx],
                h.view,
                wanted_keys.resolved(h.idx),
                &self.members,
                self.bytes,
                obj_span,
                self.dialect,
                &mut self.scratch,
            );
        }
        Ok(())
    }
}

/// Object member name: source inner bytes when copy-safe, decoded otherwise.
pub(super) enum MemberKey {
    Plain(ByteRange),
    Decoded(String),
}

/// Build the key index only past this many members. Below it, a linear scan
/// beats a `BTreeMap` and its per-member key allocation.
const INDEX_THRESHOLD: usize = 16;

/// One last-wins member: its key spelling, the span of its value, and whether
/// the span belongs to the row being walked. A retained row layout overwrites
/// slots positionally, so an untouched entry must read as absent rather than as
/// the previous row's value.
struct Entry {
    key: MemberKey,
    span: ByteRange,
    present: bool,
}

/// Last-wins object members with a key index, so member lookup is not linear
/// per key.
#[derive(Default)]
pub(super) struct Members {
    entries: Vec<Entry>,
    pub(super) index: Option<BTreeMap<Vec<u8>, usize>>,
}

impl Members {
    /// Reset for one object. Inlined for the common empty-map case.
    #[inline]
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        if let Some(index) = &mut self.index {
            index.clear();
        }
    }

    /// Mark every slot unwritten. A retained layout writes only the slots its
    /// shape demands; the rest must answer [`Self::get`] with `None`.
    pub(super) fn begin_row(&mut self) {
        for entry in &mut self.entries {
            entry.present = false;
        }
    }

    /// Insert a member, returning its entry slot so a retained row layout can
    /// write later rows' spans without a lookup.
    pub(super) fn insert(&mut self, bytes: &[u8], name: MemberKey, span: ByteRange, indexed: bool) -> usize {
        if let Some(index) = &mut self.index {
            if let Some(&i) = index.get(member_key(bytes, &name)) {
                self.entries[i].span = span;
                self.entries[i].present = true;
                return i;
            }
            index.insert(member_key(bytes, &name).to_vec(), self.entries.len());
            self.entries.push(Entry {
                key: name,
                span,
                present: true,
            });
            return self.entries.len() - 1;
        }
        let key = member_key(bytes, &name);
        if let Some(i) = self.entries.iter().position(|e| member_key(bytes, &e.key) == key) {
            self.entries[i].span = span;
            self.entries[i].present = true;
            return i;
        }
        self.entries.push(Entry {
            key: name,
            span,
            present: true,
        });
        let slot = self.entries.len() - 1;
        if indexed && self.entries.len() > INDEX_THRESHOLD {
            self.build_index(bytes);
        }
        slot
    }

    fn build_index(&mut self, bytes: &[u8]) {
        let mut index = BTreeMap::new();
        for (i, entry) in self.entries.iter().enumerate() {
            index.insert(member_key(bytes, &entry.key).to_vec(), i);
        }
        self.index = Some(index);
    }

    pub(super) fn get(&self, bytes: &[u8], field: &[u8]) -> Option<ByteRange> {
        match &self.index {
            Some(index) => index
                .get(field)
                .and_then(|&i| self.entries[i].present.then_some(self.entries[i].span)),
            None => self
                .entries
                .iter()
                .rev()
                .find(|entry| entry.present && member_key(bytes, &entry.key) == field)
                .map(|entry| entry.span),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn keys<'a>(&'a self, bytes: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + 'a {
        self.entries.iter().map(move |e| member_key(bytes, &e.key))
    }
}

/// Bound on the content-keyed head index. Past it new heads are not retained,
/// so an object with all-unique keys cannot grow the index per member; the
/// positional path is unaffected.
const HEAD_INDEX_CAP: usize = 64;

/// One retained member head: the head's source span (key, colon and trailing
/// trivia), the [`Members`] slot its value writes (or `None`), the head length,
/// and its first (up to) 16 bytes as two words for a masked compare.
#[derive(Clone, Copy)]
pub(super) struct Head {
    span: ByteRange,
    slot: Option<usize>,
    len: usize,
    w0: u64,
    w1: u64,
}

impl Head {
    pub(super) fn new(bytes: &[u8], start: usize, val_start: usize, slot: Option<usize>) -> Self {
        let len = val_start - start;
        let (w0, w1) = head_prefixes(bytes, start, len);
        Self {
            span: ByteRange::try_new(start, val_start).expect("ordered"),
            slot,
            len,
            w0,
            w1,
        }
    }
}

/// Retained shape of an object array. Each member's *head* — its quoted key,
/// the colon, and any trivia after it — is kept as a source span.
///
/// * `positional` is the previous row's sequence: while a row repeats those
///   heads byte-for-byte at the same position the walk skips key tokenizing and
///   demand matching (the homogeneous fast path). A mismatch falls the row back
///   to the general loop.
/// * `index` is the distinct heads keyed by content, bounded so an object with
///   all-unique keys cannot grow it per member; a rotated or partial row probes
///   it without re-tokenizing.
pub(super) struct RowLayout {
    valid: bool,
    positional: Vec<Head>,
    index: Vec<Head>,
    /// Head indices per key first content byte, as a 64-bit bucket mask.
    buckets: [u64; 256],
}

impl Default for RowLayout {
    fn default() -> Self {
        Self {
            valid: false,
            positional: Vec::new(),
            index: Vec::new(),
            buckets: [0; 256],
        }
    }
}

impl RowLayout {
    /// Drop the retained shape: head slots are written under one demanded key
    /// set, so a new array/run with a different set must not reuse them.
    pub(super) fn reset(&mut self) {
        self.valid = false;
        self.positional.clear();
        self.index.clear();
        self.buckets.fill(0);
    }

    /// Retain a head in the content index. Equal heads collapse, so a repeated
    /// key (last-wins) keeps one entry and its slot.
    fn learn(&mut self, bytes: &[u8], head: Head) {
        let start = head.span.start();
        let Some(&first) = bytes.get(start + 1) else {
            return;
        };
        let mut mask = self.buckets[first as usize];
        while mask != 0 {
            let bit = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let known = &self.index[bit];
            if known.len == head.len && head_eq(bytes, start, known.span) {
                return;
            }
        }
        if self.index.len() >= HEAD_INDEX_CAP {
            return;
        }
        let bit = self.index.len();
        self.index.push(head);
        self.buckets[first as usize] |= 1u64 << bit;
    }

    /// Match a retained head at `member_start` by content. Returns the head's
    /// value slot (or `None` for an un-demanded key) and the head's length. A
    /// head only matches when its end is a value byte: otherwise it is a strict
    /// prefix of a longer head (extra trivia) and must not advance the walk.
    fn head_at<S: Scan>(&self, bytes: &[u8], member_start: usize) -> Option<(Head, Option<usize>)> {
        if bytes.get(member_start) != Some(&b'"') {
            return None;
        }
        let first = *bytes.get(member_start + 1)?;
        let mut mask = self.buckets[first as usize];
        while mask != 0 {
            let bit = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let head = self.index[bit];
            if head_matches::<S>(&head, bytes, member_start) {
                return Some((head, head.slot));
            }
        }
        None
    }
}

/// Whether the byte at `at + len` is a value start rather than trivia. A head
/// ends at the first value byte, so a trivia byte there means the matched head
/// is a strict prefix of a longer member head.
#[inline(always)]
fn ends_value<S: Scan>(bytes: &[u8], at: usize, len: usize) -> bool {
    bytes.get(at + len).is_some_and(|&byte| !S::trivia_starts(byte))
}

/// First (up to) 16 bytes of a head as two words, zero-padded past its length.
#[inline]
fn head_prefixes(bytes: &[u8], at: usize, len: usize) -> (u64, u64) {
    let w0 = prefix8(bytes, at, len);
    let w1 = if len > 8 { prefix8(bytes, at + 8, len - 8) } else { 0 };
    (w0, w1)
}

#[inline]
fn prefix8(bytes: &[u8], at: usize, len: usize) -> u64 {
    let n = len.min(8);
    // A full word is a masked load; only the last bytes before EOF need the
    // zero-padded copy, so the common head costs one load and no memcpy.
    if let Some(word) = load8(bytes, at) {
        return word & mask8(n);
    }
    let mut buf = [0u8; 8];
    if let Some(src) = bytes.get(at..at + n) {
        buf[..n].copy_from_slice(src);
    }
    u64::from_ne_bytes(buf)
}

#[inline(always)]
const fn mask8(n: usize) -> u64 {
    if n >= 8 { u64::MAX } else { (1u64 << (8 * n)) - 1 }
}

#[inline(always)]
fn load8(bytes: &[u8], at: usize) -> Option<u64> {
    let chunk = bytes.get(at..at.checked_add(8)?)?;
    Some(u64::from_ne_bytes(chunk.try_into().expect("8 bytes")))
}

/// Match a head's stored prefix words against the bytes at `at`, and require
/// the byte after the head to be a value start (not trivia). A head ends at the
/// first value byte, so a trivia byte there means the head is a strict prefix
/// of a longer member head and must not advance the walk.
#[inline(always)]
fn head_matches<S: Scan>(head: &Head, bytes: &[u8], at: usize) -> bool {
    ends_value::<S>(bytes, at, head.len) && head_prefix_eq(head, bytes, at)
}

#[inline(always)]
pub(super) fn head_prefix_eq(head: &Head, bytes: &[u8], at: usize) -> bool {
    let len = head.len;
    let Some(cur0) = load8(bytes, at) else {
        return head_prefix_eq_eof(head, bytes, at);
    };
    if (cur0 ^ head.w0) & mask8(len) != 0 {
        return false;
    }
    if len > 8 {
        let Some(cur1) = load8(bytes, at + 8) else {
            return head_prefix_eq_eof(head, bytes, at);
        };
        if (cur1 ^ head.w1) & mask8(len - 8) != 0 {
            return false;
        }
        if len > 16 && !head_eq(bytes, at, head.span) {
            return false;
        }
    }
    true
}

/// [`head_prefix_eq`] within 8 bytes of EOF, where a full word load is not
/// readable. The stored prefix is exact and `ends_value` proved `at + len` is a
/// real byte, so a masked compare over the partial, zero-padded word is still
/// exact. Out of line: the hot path never pays for the tail case.
#[cold]
#[inline(never)]
fn head_prefix_eq_eof(head: &Head, bytes: &[u8], at: usize) -> bool {
    let len = head.len;
    if (prefix8(bytes, at, len) ^ head.w0) & mask8(len) != 0 {
        return false;
    }
    if len > 8 {
        if (prefix8(bytes, at + 8, len - 8) ^ head.w1) & mask8(len - 8) != 0 {
            return false;
        }
        if len > 16 && !head_eq(bytes, at, head.span) {
            return false;
        }
    }
    true
}

/// Compare the head at `span` with the bytes at `at`, both `span.len()` long.
/// Used for the amortized learn-dedup and for heads longer than 16 bytes.
#[inline]
fn head_eq(bytes: &[u8], at: usize, span: ByteRange) -> bool {
    let len = span.len();
    let (Some(cur), Some(src)) = (bytes.get(at..at + len), bytes.get(span.start()..span.end())) else {
        return false;
    };
    cur == src
}

/// The member names a row demand touches, gathered once per element loop as a
/// per-member reject filter (`first`) plus an exact test. It holds the one
/// wanted name inline; several names resolve against the hits it was gathered
/// from, so neither case allocates.
pub(super) struct Wanted<'a> {
    one: Option<&'a [u8]>,
    /// More than one name wanted: `contains` resolves against the hits.
    more: bool,
    first: [u64; 4],
    /// The resolved predicate of each flat `Filter` hit, indexed by mark index
    /// (sparse). Built once per row loop so the ordering operands are parsed
    /// once, not once per row.
    resolved: Vec<Option<ResolvedPredicate<'a>>>,
}

impl<'a> Wanted<'a> {
    /// The empty demand: no key is wanted. A `Path`-spine object owns no flat
    /// field set, so it shares this instead of rebuilding a filter per level.
    pub(super) const NONE: Wanted<'static> = Wanted {
        one: None,
        more: false,
        first: [0; 4],
        resolved: Vec::new(),
    };

    #[inline]
    pub(super) fn gather(hits: &[Hit<'a>]) -> Self {
        let mut wanted = Self {
            one: None,
            more: false,
            first: [0; 4],
            resolved: Vec::new(),
        };
        for h in hits {
            match h.view {
                View::Project { path: [], fields } => {
                    for field in fields {
                        wanted.add(field.as_bytes());
                    }
                }
                View::Filter {
                    path: [],
                    predicate,
                    project,
                } => {
                    for field in project {
                        wanted.add(field.as_bytes());
                    }
                    wanted.add_predicate(predicate);
                    if wanted.resolved.len() <= h.idx {
                        wanted.resolved.resize_with(h.idx + 1, || None);
                    }
                    wanted.resolved[h.idx] = Some(ResolvedPredicate::resolve(predicate));
                }
                _ => {}
            }
        }
        wanted
    }

    /// The resolved predicate of the flat `Filter` hit for `idx`, when this
    /// demand set gathered one.
    pub(super) fn resolved(&self, idx: usize) -> Option<&ResolvedPredicate<'a>> {
        self.resolved.get(idx)?.as_ref()
    }

    fn add(&mut self, key: &'a [u8]) {
        if let Some(&byte) = key.first() {
            self.first[byte as usize >> 6] |= 1u64 << (byte & 63);
        }
        match self.one {
            None => self.one = Some(key),
            Some(existing) if existing == key => {}
            Some(_) => self.more = true,
        }
    }

    fn add_predicate(&mut self, predicate: &'a Predicate) {
        match predicate {
            Predicate::And(a, b) | Predicate::Or(a, b) => {
                self.add_predicate(a);
                self.add_predicate(b);
            }
            Predicate::Not(p) => self.add_predicate(p),
            Predicate::Eq { field, .. }
            | Predicate::Ne { field, .. }
            | Predicate::Gt { field, .. }
            | Predicate::Lt { field, .. }
            | Predicate::Ge { field, .. }
            | Predicate::Le { field, .. } => self.add(field.as_bytes()),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.one.is_none()
    }

    #[inline]
    pub(super) fn contains(&self, key: &[u8], hits: &[Hit<'_>]) -> bool {
        if let Some(&head) = key.first()
            && self.first[head as usize >> 6] & (1u64 << (head & 63)) == 0
        {
            return false;
        }
        if self.more {
            return hits.iter().any(|h| view_has_key(h.view, key));
        }
        self.one.is_some_and(|k| k.len() == key.len() && k == key)
    }
}

/// Whether `view` names `key`: the same names [`Wanted::gather`] collects, so the
/// multi-name fallback agrees with the inline set.
fn view_has_key(view: View<'_>, key: &[u8]) -> bool {
    match view {
        View::Project { path: [], fields } => fields.iter().any(|field| field.as_bytes() == key),
        View::Filter {
            path: [],
            predicate,
            project,
        } => project.iter().any(|field| field.as_bytes() == key) || predicate_has_key(predicate, key),
        _ => false,
    }
}

fn predicate_has_key(predicate: &Predicate, key: &[u8]) -> bool {
    match predicate {
        Predicate::And(a, b) | Predicate::Or(a, b) => predicate_has_key(a, key) || predicate_has_key(b, key),
        Predicate::Not(p) => predicate_has_key(p, key),
        Predicate::Eq { field, .. }
        | Predicate::Ne { field, .. }
        | Predicate::Gt { field, .. }
        | Predicate::Lt { field, .. }
        | Predicate::Ge { field, .. }
        | Predicate::Le { field, .. } => field.as_bytes() == key,
    }
}

pub(super) fn pred_on_members(
    bytes: &[u8],
    members: &Members,
    predicate: &ResolvedPredicate<'_>,
    dialect: Dialect,
    scratch: &mut String,
) -> bool {
    match predicate {
        ResolvedPredicate::And(a, b) => {
            pred_on_members(bytes, members, a, dialect, scratch) && pred_on_members(bytes, members, b, dialect, scratch)
        }
        ResolvedPredicate::Or(a, b) => {
            pred_on_members(bytes, members, a, dialect, scratch) || pred_on_members(bytes, members, b, dialect, scratch)
        }
        ResolvedPredicate::Not(p) => !pred_on_members(bytes, members, p, dialect, scratch),
        ResolvedPredicate::Eq { field, value } => {
            member_bytes(bytes, members, field).is_some_and(|g| span_eq_value(g, value, dialect, scratch))
        }
        ResolvedPredicate::Ne { field, value } => {
            member_bytes(bytes, members, field).is_none_or(|g| !span_eq_value(g, value, dialect, scratch))
        }
        ResolvedPredicate::Order { field, cmp, want } => num_cmp(bytes, members, field, *cmp, want, dialect, scratch),
    }
}

/// Read one object key at `start`; returns the decoded spelling when the key is
/// not a plain double-quoted run, the source span otherwise, and the end.
///
/// Only the plain-key head is forced into the member loop; a key that needs a
/// skip and a possible decode is the rare arm and stays out of line (same shape
/// as [`head_prefix_eq_eof`]).
#[allow(clippy::inline_always)] // hot scan path: the plain-key head must fold in
#[inline(always)]
pub(super) fn read_key<S: Scan>(
    bytes: &[u8],
    start: usize,
    dialect: Dialect,
) -> Result<(Option<String>, Option<ByteRange>, usize), SmallErr> {
    // Fast path: a plain double-quoted key needs one scan and no decode.
    // Without this, `skip_key` + `key_plain_inner` scan it twice.
    if let Some((span, end)) = S::plain_key(bytes, start) {
        return Ok((None, Some(span), end));
    }
    decode_key::<S>(bytes, start, dialect)
}

/// The rare key arm: escaped / single-quoted / bare spellings, plus the decode
/// buffer. Kept `#[cold]` and out of line so its `String` machinery does not
/// bloat every member-loop instantiation.
#[cold]
#[inline(never)]
fn decode_key<S: Scan>(
    bytes: &[u8],
    start: usize,
    dialect: Dialect,
) -> Result<(Option<String>, Option<ByteRange>, usize), SmallErr> {
    let end = S::skip_key(bytes, start, Check::Values, dialect)?;
    if let Some(span) = S::key_plain_inner(bytes, start, end, dialect) {
        return Ok((None, Some(span), end));
    }
    let mut key = String::new();
    S::parse_string_into(bytes, start, &mut key, dialect)?;
    Ok((Some(key), None, end))
}

pub(super) fn member_name(inner: Option<ByteRange>, key_bytes: &[u8]) -> MemberKey {
    if let Some(span) = inner {
        MemberKey::Plain(span)
    } else {
        MemberKey::Decoded(String::from_utf8_lossy(key_bytes).into_owned())
    }
}

fn member_key<'a>(bytes: &'a [u8], key: &'a MemberKey) -> &'a [u8] {
    match key {
        MemberKey::Plain(span) => bytes.get(span.start()..span.end()).unwrap_or(&[]),
        MemberKey::Decoded(s) => s.as_bytes(),
    }
}

pub(super) fn member_bytes<'a>(bytes: &'a [u8], members: &Members, field: &str) -> Option<&'a [u8]> {
    members
        .get(bytes, field.as_bytes())
        .and_then(|rng| bytes.get(rng.start()..rng.end()))
}
