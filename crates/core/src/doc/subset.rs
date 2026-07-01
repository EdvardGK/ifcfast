//! Surgical subset builder (GH #124 Phase 2b).
//!
//! Given a set of seed step-ids — the products a caller wants to keep — this
//! produces STEP bytes for a self-contained document that holds exactly the
//! seeds plus everything needed to keep them valid:
//!
//! 1. **Forward dependencies** (`doc/refs.rs`): placements, representations,
//!    profiles, materials, units, contexts — everything a seed *points at*.
//! 2. **Relationship attachment** (`doc/rel_rules.rs`): the `IfcRel*` objects
//!    that point *at* the seeds. Forward closure never reaches these, so a
//!    second pass activates each rel whose anchor side still has a survivor,
//!    pulls in its upstream dependency (the spatial parent, the pset, the
//!    material, the type…), and forward-closes that. Iterated to a fixpoint,
//!    this climbs the spatial spine to `IfcProject` and drags in every
//!    attached definition.
//!
//! ## Why rels are tracked apart from the keep set
//!
//! A relationship's forward references *include its participants*. If a rel
//! were placed in the forward-closure keep set, closing it would re-admit
//! every dropped participant (all the other products in a shared containment
//! or pset rel), defeating the subset. So rels are never forward-closed:
//! only their *pull* ref is added to keep. The rel record is emitted
//! separately, with its anchor SET rewritten to the survivors — see
//! [`super::rel_rules`] and [`super::emit::emit_subset`].
//!
//! ## Guarantees
//!
//! The emitted bytes re-open as a [`Doc`] with no dangling reference to an
//! entity that was present in the source: forward closure leaves no dangling
//! forward ref, and the anchor rewrite leaves no rel pointing at a dropped
//! participant. `subset(doc, all_ids)` reproduces the source byte-for-byte.

use std::collections::HashMap;

use super::emit::{emit_subset, EmitStats};
use super::rel_rules::{field_span, parse_rel, rule_for};
use super::refs::reachable_closure;
use super::Doc;

/// One relationship's static reference sets, parsed once up front so the
/// activation fixpoint is pure set-membership (no re-parsing per iteration).
struct RelInfo {
    id: u64,
    anchor: Vec<u64>,
    pull: Vec<u64>,
}

/// Summary of a subset build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubsetStats {
    /// Seeds that were actually present in the source.
    pub seeds_present: usize,
    /// Total records emitted (kept entities + retained rels).
    pub records_out: usize,
    /// Relationship records retained.
    pub rels_kept: usize,
    /// Retained rels whose anchor SET was rewritten (a proper subset of the
    /// original participants survived).
    pub rels_pruned: usize,
}

/// Build a self-contained subset containing `seeds`. Returns the STEP bytes
/// and a [`SubsetStats`]. See the module docs for the closure it computes.
pub fn subset(doc: &Doc, seeds: &[u64]) -> (Vec<u8>, SubsetStats) {
    let seeds_present = seeds.iter().filter(|s| doc.contains(**s)).count();

    // Pre-scan every relationship the pass understands, once.
    let rels: Vec<RelInfo> = doc
        .ids()
        .iter()
        .filter_map(|&id| {
            let bytes = doc.record_bytes(id)?;
            let (_rule, anchor, pull) = parse_rel(bytes)?;
            Some(RelInfo { id, anchor, pull })
        })
        .collect();

    // Phase 1: forward-dependency closure of the seeds.
    let mut keep = reachable_closure(doc, seeds);

    // Phase 2: activate rels and pull upstream deps to a fixpoint. Each
    // round collects the pull refs of every activated rel not yet kept,
    // forward-closes them, and merges — growing `keep` monotonically until a
    // round adds nothing. Bounded by spatial depth (typically <6 rounds).
    loop {
        let mut new_pulls: Vec<u64> = Vec::new();
        for rel in &rels {
            let activated = rel.anchor.iter().any(|a| keep.contains(a));
            if !activated {
                continue;
            }
            for &p in &rel.pull {
                if doc.contains(p) && !keep.contains(&p) {
                    new_pulls.push(p);
                }
            }
        }
        if new_pulls.is_empty() {
            break;
        }
        let before = keep.len();
        keep.extend(reachable_closure(doc, &new_pulls));
        if keep.len() == before {
            break; // defensive: no growth despite pending pulls
        }
    }

    // Phase 3: decide which rels to emit and rewrite pruned anchor SETs.
    let mut emit_ids = keep.clone();
    let mut overrides: HashMap<u64, Vec<u8>> = HashMap::new();
    let mut rels_kept = 0usize;
    let mut rels_pruned = 0usize;

    for rel in &rels {
        let survivors: Vec<u64> = rel
            .anchor
            .iter()
            .copied()
            .filter(|a| keep.contains(a))
            .collect();
        if survivors.is_empty() {
            continue; // not activated → dropped
        }
        rels_kept += 1;
        emit_ids.insert(rel.id);
        // Rewrite only when some participants were dropped. A single-ref
        // anchor (voids/fills) always survives whole, so it never rewrites.
        if survivors.len() < rel.anchor.len() {
            if let Some(bytes) = splice_anchor(doc, rel.id, &survivors) {
                overrides.insert(rel.id, bytes);
                rels_pruned += 1;
            }
        }
    }

    let (out, emit_stats): (Vec<u8>, EmitStats) = emit_subset(doc, &emit_ids, &overrides);
    (
        out,
        SubsetStats {
            seeds_present,
            records_out: emit_stats.records_out,
            rels_kept,
            rels_pruned,
        },
    )
}

/// Rebuild rel record `id`'s bytes with its anchor SET rewritten to
/// `survivors` (source order preserved), leaving every other byte — guid,
/// pull ref, trailing separator — verbatim. `None` if the record isn't a
/// known rel or its anchor field can't be located.
fn splice_anchor(doc: &Doc, id: u64, survivors: &[u64]) -> Option<Vec<u8>> {
    let span = doc.record_bytes(id)?;
    let (_id, type_name, _args) = crate::lexer::parse_record_span(span)?;
    let rule = rule_for(type_name)?;
    let range = field_span(span, rule.anchor)?;

    let mut field = Vec::with_capacity(survivors.len() * 8 + 2);
    field.push(b'(');
    for (i, s) in survivors.iter().enumerate() {
        if i > 0 {
            field.push(b',');
        }
        field.push(b'#');
        field.extend_from_slice(s.to_string().as_bytes());
    }
    field.push(b')');

    let mut out = Vec::with_capacity(span.len() + field.len());
    out.extend_from_slice(&span[..range.start]);
    out.extend_from_slice(&field);
    out.extend_from_slice(&span[range.end..]);
    Some(out)
}
