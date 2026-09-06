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
use std::sync::OnceLock;

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
///
/// Two-storage layout:
/// - `entries` (HashMap) — O(1) `get(step_id)` for the ref-walking code
///   paths (psets, materials, mesh dispatch, etc.).
/// - `order` (Vec) — step_ids in the order they appeared in the source
///   file. `iter()` walks this Vec so the iteration order is
///   deterministic across calls. Without this, std `HashMap`'s
///   per-instance random hash seeding shuffles iteration order between
///   `EntityTable::build` invocations on the same buffer — invisible
///   for most workflows (the substrate doesn't care about row order)
///   but fatal for the point-cloud sampler, which needs bit-identical
///   output across runs for the same `(file, per_m2, seed)`.
pub struct EntityTable<'a> {
    buf: &'a [u8],
    entries: HashMap<u64, EntityRefs>,
    order: Vec<u64>,
    /// Memoized id of the first `IfcUnitAssignment` (or `None` if absent).
    /// Finding it is an O(n) `iter()` scan and the assignment can sit at the
    /// very end of the DATA section (G55_ARK: penultimate entity of ~2.8M),
    /// so callers that need it per-item (e.g. profile conic-trim unit
    /// resolution) would otherwise re-scan the whole table repeatedly. Filled
    /// on first request via `unit_assignment_id`, tied to this table's
    /// lifetime so it can never go stale across models.
    unit_assignment: OnceLock<Option<u64>>,
    /// Non-`None` when the DATA walk did NOT end legitimately — a stray
    /// byte mid-stream, an unterminated final record, or a missing
    /// `DATA;` marker (GH #148). The table then describes a PARTIAL
    /// model. Read it via [`EntityTable::scan_error`]; it is also
    /// printed to stderr at build time so the loss is never silent even
    /// for callers that don't check.
    scan_error: Option<String>,
    warnings: Vec<String>,
}

impl<'a> EntityTable<'a> {
    pub fn build(buf: &'a [u8]) -> Self {
        // A missing `DATA;` marker is not "start at zero and hope" — it
        // means we could not locate the entity stream at all. Scanning
        // from 0 keeps bare record-list fixtures working, but the
        // condition is recorded rather than swallowed (GH #148).
        let (data_start, mut scan_error) = match data_section_start(buf) {
            Some(s) => (s, None),
            None => (
                0,
                Some(
                    "no `DATA;` section marker found — scanning from byte 0. \
                     If this is an IFC file it is malformed or truncated in \
                     its header; entity coverage is not trustworthy."
                        .to_string(),
                ),
            ),
        };
        let data_end = endsec_position(buf, data_start);

        // Capacity hint based on observation: roughly 1 entity per ~110 bytes
        // of DATA section for typical IFCs (smaller for header-heavy files).
        let cap_hint = ((data_end.saturating_sub(data_start)) / 110).max(1024);
        let mut entries: HashMap<u64, EntityRefs> = HashMap::with_capacity(cap_hint);
        let mut order: Vec<u64> = Vec::with_capacity(cap_hint);

        // Duplicate STEP ids are illegal per ISO-10303-21 but do occur
        // in hand-merged files. Policy (GH #159): `entries` keeps the
        // LAST record for the id, `order` keeps the id exactly once at
        // its FIRST appearance, so `iter()` visits every id once and
        // agrees with `get()`. The shadowed record is real data loss, so
        // it is counted and warned about instead of vanishing.
        let mut duplicate_ids: Vec<u64> = Vec::new();

        let stop = for_each_record(buf, data_start, data_end, |rec| {
            // SAFETY: rec.type_name and rec.args are sub-slices of `buf` from
            // the same `for_each_record` walk, so their bytes are addressable
            // via offset arithmetic from `buf.as_ptr()`.
            let type_start = rec.type_name.as_ptr() as usize - buf.as_ptr() as usize;
            let args_start = rec.args.as_ptr() as usize - buf.as_ptr() as usize;
            // Only push to `order` on first insertion. STEP ids should be
            // unique by spec, but a malformed file with duplicates would
            // otherwise inflate `order` and cause `iter()` to revisit
            // entries with the (overwritten) latest value.
            if entries
                .insert(
                    rec.id,
                    EntityRefs {
                        type_start,
                        type_len: rec.type_name.len() as u32,
                        args_start,
                        args_len: rec.args.len() as u32,
                    },
                )
                .is_none()
            {
                order.push(rec.id);
            } else if duplicate_ids.len() < 16 {
                duplicate_ids.push(rec.id);
            }
        });

        if let Some(msg) = stop.describe() {
            // A garbage byte / unterminated record beats a missing
            // `DATA;` as the headline problem — it is the one that
            // silently truncated the model.
            scan_error = Some(msg);
        }
        let mut warnings: Vec<String> = Vec::new();
        if !duplicate_ids.is_empty() {
            warnings.push(format!(
                "[ifcfast] WARNING: duplicate STEP ids in the DATA section \
                 (first {}: {:?}). The LAST record wins for each id; the \
                 earlier one(s) are not readable. Duplicate ids are illegal \
                 per ISO-10303-21.",
                duplicate_ids.len(),
                duplicate_ids
            ));
        }

        Self {
            buf,
            entries,
            order,
            unit_assignment: OnceLock::new(),
            scan_error,
            warnings,
        }
    }

    /// Why the DATA walk did not end legitimately, if it didn't. `Some`
    /// means this table describes a PARTIAL model — callers building a
    /// substrate / QTO / clash result from it are publishing a truncated
    /// answer and should refuse (GH #148).
    pub fn scan_error(&self) -> Option<&str> {
        self.scan_error.as_deref()
    }

    /// Non-fatal anomalies seen while building the table (today:
    /// duplicate STEP ids). Collected, never printed — surfaced through
    /// the extractor result dicts and `Bundle.warnings`.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Id of the first `IfcUnitAssignment` in the DATA section, memoized so
    /// the O(n) scan runs at most once per table. Returns `None` when the
    /// file declares no unit assignment. Used by profile conic-trim unit
    /// resolution, which would otherwise re-scan the whole table per arc.
    pub fn unit_assignment_id(&self) -> Option<u64> {
        *self.unit_assignment.get_or_init(|| {
            self.iter()
                .find(|(_, t, _)| t.eq_ignore_ascii_case(b"IFCUNITASSIGNMENT"))
                .map(|(id, _, _)| id)
        })
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

    /// The step_ids of every entity, in the order they appeared in the
    /// source file's DATA section. Determinism contract — two
    /// `EntityTable::build` calls on the same buffer yield the same
    /// slice. Exposed so consumers can shard the walk in parallel
    /// (rayon `par_iter` over the slice + `table.get(id)` per shard).
    #[inline]
    pub fn order(&self) -> &[u64] {
        &self.order
    }

    /// Iterate over `(id, type, args)` for every entity, in the order
    /// entries appeared in the source file's DATA section. Determinism
    /// is contract: two `EntityTable::build` calls on the same buffer
    /// yield the same iteration sequence.
    pub fn iter(&self) -> impl Iterator<Item = (u64, &[u8], &[u8])> + '_ {
        self.order.iter().filter_map(|id| {
            let e = self.entries.get(id)?;
            let type_end = e.type_start + e.type_len as usize;
            let args_end = e.args_start + e.args_len as usize;
            Some((
                *id,
                &self.buf[e.type_start..type_end],
                &self.buf[e.args_start..args_end],
            ))
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
