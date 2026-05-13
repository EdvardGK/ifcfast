//! Generic `step_id → entity byte range` lookup for the whole DATA section.
//!
//! The tier-1 indexer (`indexer::index`) only captures attributes for entities
//! it knows the schema position of (IfcProduct subtypes + storeys + sites +
//! buildings + rels). The mesh emitter needs to *follow refs* through the
//! representation graph — IfcExtrudedAreaSolid → SweptArea (IfcProfileDef) →
//! Position (IfcAxis2Placement2D), etc. That requires looking up any entity by
//! step_id without a schema-aware extractor for it.
//!
//! This module builds a flat lookup table during the same lexer pass. Memory
//! is `n_entities * ~40 bytes` — about 70 MB for a 1.8 M-entity (192 MB) IFC.
//! Each entry stores byte offsets into the source buffer; argument-slice
//! parsing happens lazily per-lookup.

use std::collections::HashMap;

use crate::lexer::{data_section_start, endsec_position, for_each_record};

/// Byte ranges into the source buffer for one STEP record.
#[derive(Debug, Clone, Copy)]
pub struct EntityRefs {
    pub type_start: usize,
    pub type_len: u32,
    pub args_start: usize,
    pub args_len: u32,
}

/// Lookup table for every entity in the IFC's DATA section. Constructed
/// once and queried many times.
pub struct EntityTable<'a> {
    buf: &'a [u8],
    entries: HashMap<u64, EntityRefs>,
}

impl<'a> EntityTable<'a> {
    pub fn build(buf: &'a [u8]) -> Self {
        let data_start = data_section_start(buf).unwrap_or(0);
        let data_end = endsec_position(buf, data_start);

        // Capacity hint based on observation: roughly 1 entity per ~110 bytes
        // of DATA section for typical IFCs (smaller for header-heavy files).
        let cap_hint = ((data_end.saturating_sub(data_start)) / 110).max(1024);
        let mut entries: HashMap<u64, EntityRefs> = HashMap::with_capacity(cap_hint);

        for_each_record(buf, data_start, data_end, |rec| {
            // SAFETY: rec.type_name and rec.args are sub-slices of `buf` from
            // the same `for_each_record` walk, so their bytes are addressable
            // via offset arithmetic from `buf.as_ptr()`.
            let type_start = rec.type_name.as_ptr() as usize - buf.as_ptr() as usize;
            let args_start = rec.args.as_ptr() as usize - buf.as_ptr() as usize;
            entries.insert(
                rec.id,
                EntityRefs {
                    type_start,
                    type_len: rec.type_name.len() as u32,
                    args_start,
                    args_len: rec.args.len() as u32,
                },
            );
        });

        Self { buf, entries }
    }

    /// Look up an entity by STEP id. Returns `(type_name, args)` byte slices
    /// or None if not present.
    #[inline]
    pub fn get(&self, id: u64) -> Option<(&[u8], &[u8])> {
        let e = self.entries.get(&id)?;
        let type_end = e.type_start + e.type_len as usize;
        let args_end = e.args_start + e.args_len as usize;
        Some((
            &self.buf[e.type_start..type_end],
            &self.buf[e.args_start..args_end],
        ))
    }

    /// Just the type name. Useful when you only need to dispatch.
    #[inline]
    pub fn type_of(&self, id: u64) -> Option<&[u8]> {
        let e = self.entries.get(&id)?;
        let end = e.type_start + e.type_len as usize;
        Some(&self.buf[e.type_start..end])
    }

    /// Iterate over (id, type, args) for every entity. Useful for diagnostics.
    pub fn iter(&self) -> impl Iterator<Item = (u64, &[u8], &[u8])> + '_ {
        self.entries.iter().map(|(id, e)| {
            let type_end = e.type_start + e.type_len as usize;
            let args_end = e.args_start + e.args_len as usize;
            (
                *id,
                &self.buf[e.type_start..type_end],
                &self.buf[e.args_start..args_end],
            )
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
