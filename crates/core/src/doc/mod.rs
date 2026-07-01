//! Owned, round-trippable STEP document — the foundation for surgical
//! IFC subset + mesh hotswap (GH #124).
//!
//! Today the parser is one-way: `ifcfast.open()` streams the buffer,
//! extracts derived rows, and discards the STEP graph. A *writer* needs
//! the opposite — an owned, resident document it can re-emit faithfully
//! and mutate surgically. This module is that document.
//!
//! ## Design: offsets, not borrows (no `unsafe`)
//!
//! [`crate::entity_table::EntityRefs`] already stores byte *offsets*, not
//! borrowed slices, so an owned document that holds `buf: Vec<u8>`
//! alongside an offset table has no self-referential-lifetime problem:
//! accessors take `&self.buf` and slice on demand. We reuse that idea
//! here with a parallel `order` / `starts` layout so re-emission walks
//! records in source order.
//!
//! ## Byte-identity contract (Phase 1 gate)
//!
//! `emit(doc, None)` (keep everything) MUST reproduce the source bytes
//! exactly. The construction guarantees it: each record's emit span runs
//! from its `#` to the *next* record's `#` (the last to `ENDSEC`), so the
//! spans tile `[first_record .. ENDSEC)` with no gaps or overlaps;
//! prepend the header prefix and append the trailer and the result is the
//! original buffer, byte for byte. See `doc/emit.rs`.

mod emit;
mod refs;
mod rel_rules;
mod subset;

pub use emit::{emit, emit_subset, EmitStats};
pub use refs::{forward_refs, reachable_closure};
pub use rel_rules::{field_refs, field_span, parse_rel, rule_for, RelField, RelRule, REL_RULES};
pub use subset::{subset, SubsetStats};

use std::collections::HashMap;
use std::path::Path;

use crate::lexer::{data_section_start, endsec_position, for_each_record_span};

/// An owned STEP document: source bytes plus a record-span index that
/// supports verbatim, filtered re-emission.
pub struct Doc {
    /// The full source bytes (header + DATA + trailer), owned. For an
    /// `.ifczip` input these are the *decompressed* STEP bytes.
    buf: Vec<u8>,
    /// step_ids in source order (parallel to `starts`).
    order: Vec<u64>,
    /// `starts[i]` is the byte offset of `order[i]`'s leading `#`.
    starts: Vec<usize>,
    /// Byte offset of `ENDSEC;` that closes the DATA section — the upper
    /// bound for the last record's emit span and the start of the trailer.
    endsec: usize,
    /// step_id → index into `order`/`starts`. First occurrence wins
    /// (mirrors `EntityTable`'s dedup of malformed duplicate ids).
    index: HashMap<u64, usize>,
    /// Largest step_id present — the base for a new-id allocator
    /// (hotswap allocates `max_id + 1, +2, …`).
    max_id: u64,
}

impl Doc {
    /// Open an IFC file (transparently decompressing `.ifczip`) into an
    /// owned, editable document.
    pub fn open_editable(path: &Path) -> std::io::Result<Doc> {
        let source = crate::source::open(path)?;
        Ok(Doc::from_bytes(source.as_bytes().to_vec()))
    }

    /// Build a [`Doc`] from owned STEP bytes already in memory.
    pub fn from_bytes(buf: Vec<u8>) -> Doc {
        let data_start = data_section_start(&buf).unwrap_or(0);
        let endsec = endsec_position(&buf, data_start);

        let cap = (endsec.saturating_sub(data_start) / 110).max(64);
        let mut order: Vec<u64> = Vec::with_capacity(cap);
        let mut starts: Vec<usize> = Vec::with_capacity(cap);
        let mut index: HashMap<u64, usize> = HashMap::with_capacity(cap);
        let mut max_id = 0u64;

        for_each_record_span(&buf, data_start, endsec, |id, start, _end| {
            // First occurrence wins; a duplicate id (malformed file) is
            // not re-indexed and its later span is dropped from `order`
            // so re-emission can't double-count it.
            if let std::collections::hash_map::Entry::Vacant(slot) = index.entry(id) {
                slot.insert(order.len());
                order.push(id);
                starts.push(start);
                if id > max_id {
                    max_id = id;
                }
            }
        });

        Doc { buf, order, starts, endsec, index, max_id }
    }

    /// Number of DATA-section records.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Largest step_id in the document (0 if empty).
    pub fn max_id(&self) -> u64 {
        self.max_id
    }

    /// Every step_id in source order.
    pub fn ids(&self) -> &[u64] {
        &self.order
    }

    /// Whether `id` is present.
    pub fn contains(&self, id: u64) -> bool {
        self.index.contains_key(&id)
    }

    /// The full record span bytes (`#id = TYPE(...)` plus its trailing
    /// separator) for `id`, or `None` if absent. Used by the reference
    /// scanner to extract outbound `#ref` tokens, and by the rel subset
    /// pass (via [`rel_rules::parse_rel`]) to read positional fields.
    pub fn record_bytes(&self, id: u64) -> Option<&[u8]> {
        let i = *self.index.get(&id)?;
        Some(&self.buf[self.record_span(i)])
    }

    /// The raw bytes of the buffer (header + DATA + trailer).
    pub(crate) fn buf(&self) -> &[u8] {
        &self.buf
    }

    pub(crate) fn endsec(&self) -> usize {
        self.endsec
    }

    /// Byte offset where the header/prefix ends and the first record
    /// begins (or `endsec` if the document has no records).
    pub(crate) fn prefix_end(&self) -> usize {
        self.starts.first().copied().unwrap_or(self.endsec)
    }

    /// The verbatim emit span `[start, end)` for the record at position
    /// `i`: from its `#` to the next record's `#` (or `endsec` for the
    /// last). Trailing separators travel with the record so the spans
    /// tile the DATA section exactly.
    pub(crate) fn record_span(&self, i: usize) -> std::ops::Range<usize> {
        let start = self.starts[i];
        let end = self.starts.get(i + 1).copied().unwrap_or(self.endsec);
        start..end
    }

    /// Iterate `(id, position)` in source order.
    pub(crate) fn records(&self) -> impl Iterator<Item = (u64, usize)> + '_ {
        self.order.iter().copied().enumerate().map(|(i, id)| (id, i))
    }
}
