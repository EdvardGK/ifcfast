//! Reference graph over a [`Doc`] — the reachability engine for subset
//! (GH #124 Phase 2).
//!
//! Forward references are extracted syntactically: scan a record's bytes
//! for `#id` tokens (schema-free, via [`crate::lexer::scan_ref_tokens`]).
//! The transitive forward closure of a seed set is every entity those
//! seeds *depend on* — placements, representations, materials, units,
//! contexts — i.e. everything that must be kept for the seeds to remain
//! valid.
//!
//! What forward closure does NOT reach: relationship objects
//! (`IfcRel*`), because in STEP a rel points *at* its participants, not
//! vice versa — a product never references the rel that contains it. So
//! spatial containment and property/material *attachment* are handled by
//! a separate rel pass (see `doc/subset.rs`, next), not here. This module
//! is deliberately just the dependency-closure primitive.

use std::collections::{HashSet, VecDeque};

use crate::lexer::scan_ref_tokens;

use super::Doc;

/// Outbound references of the record `id`: every `#ref` in its bytes
/// except the record's own id (the leading token). Empty if `id` is
/// absent or the record has no references.
pub fn forward_refs(doc: &Doc, id: u64) -> Vec<u64> {
    match doc.record_bytes(id) {
        Some(bytes) => {
            let mut toks = scan_ref_tokens(bytes);
            if !toks.is_empty() {
                toks.remove(0); // drop the record's own id
            }
            toks
        }
        None => Vec::new(),
    }
}

/// Transitive forward-reachability closure from `seeds`: every entity
/// reachable by following outbound references, plus the seeds
/// themselves (those present in the document). The result is
/// *forward-closed* — for every id in it, all of that id's forward
/// references are also in it — so emitting exactly this set leaves no
/// dangling forward reference.
pub fn reachable_closure(doc: &Doc, seeds: &[u64]) -> HashSet<u64> {
    let mut keep: HashSet<u64> = HashSet::new();
    let mut work: VecDeque<u64> = VecDeque::new();
    for &s in seeds {
        if doc.contains(s) && keep.insert(s) {
            work.push_back(s);
        }
    }
    while let Some(id) = work.pop_front() {
        for r in forward_refs(doc, id) {
            if doc.contains(r) && keep.insert(r) {
                work.push_back(r);
            }
        }
    }
    keep
}
