//! Field-index pinning for the `IfcRel*` subset rules (GH #124 Phase 2b).
//!
//! The subset pass keys its keep/prune decisions on positional argument
//! indices of relationship records (anchor side vs. pull side). An
//! off-by-one there silently corrupts a subset — the classic trap is
//! `IfcRelAssignsToGroup`, whose `RelatedObjectsType` enum at index 5
//! pushes `RelatingGroup` to index 6. These tests lock every index down:
//!
//! 1. `rel_field_pinning.ifc` — one synthetic record per rule with anchor
//!    refs in the `#50xx` band and pull refs in the `#40xx` band, so we can
//!    assert *exact* extraction. Pins field POSITIONS structurally.
//! 2. Local real fixtures — the 5 rel types present in `minimal.ifc` are
//!    checked against a real graph (every extracted ref must resolve).
//! 3. Corpus gate (`IFCFAST_CORPUS`, ignored by default) — the same
//!    resolve-every-ref invariant over diverse real files, which is what
//!    catches a layout that happens to match a hand-written fixture but not
//!    what exporters actually emit.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use _core::doc::{forward_refs, parse_rel, Doc};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// `(anchor, pull)` refs extracted from record `id`, or panics if the
/// record is absent or not a known rel.
fn extract(doc: &Doc, id: u64) -> (Vec<u64>, Vec<u64>) {
    let bytes = doc.record_bytes(id).expect("record present");
    let (_rule, anchor, pull) = parse_rel(bytes).expect("known rel type");
    (anchor, pull)
}

#[test]
fn pins_every_rule_field_index_exactly() {
    let doc = Doc::open_editable(&fixtures_dir().join("rel_field_pinning.ifc")).expect("open");

    // (rel id, expected anchor set, expected pull set). Anchors are #50xx,
    // pulls #40xx — see the fixture header.
    let expected: &[(u64, &[u64], &[u64])] = &[
        (101, &[5001, 5002], &[4001]), // IfcRelAggregates
        (102, &[5003, 5004], &[4002]), // IfcRelContainedInSpatialStructure
        (103, &[5005, 5006], &[4003]), // IfcRelDefinesByProperties
        (104, &[5007, 5008], &[4004]), // IfcRelDefinesByType
        (105, &[5009, 5010], &[4005]), // IfcRelAssociatesMaterial
        (106, &[5011, 5012], &[4006]), // IfcRelAssociatesClassification
        (107, &[5013, 5014], &[4007]), // IfcRelNests
        (108, &[5015, 5016], &[4008]), // IfcRelAssignsToGroup (group @ idx 6)
        (109, &[5017, 5018], &[4009]), // IfcRelServicesBuildings
        (110, &[5019, 5020], &[4010]), // IfcRelDeclares
        (111, &[5021], &[4011]),       // IfcRelVoidsElement
        (112, &[5022], &[4012]),       // IfcRelFillsElement
    ];

    for &(id, want_anchor, want_pull) in expected {
        let (anchor, pull) = extract(&doc, id);
        assert_eq!(anchor, want_anchor, "anchor mismatch on #{}", id);
        assert_eq!(pull, want_pull, "pull mismatch on #{}", id);
    }

    // Every anchor ref lives in the #50xx band and every pull ref in #40xx
    // — a coarse guard that no field bleeds into a neighbour of the wrong
    // role (e.g. anchor accidentally reading a pull ref).
    for &(id, _, _) in expected {
        let (anchor, pull) = extract(&doc, id);
        for r in anchor {
            assert!((5000..6000).contains(&r), "anchor #{} out of band on #{}", r, id);
        }
        for r in pull {
            assert!((4000..5000).contains(&r), "pull #{} out of band on #{}", r, id);
        }
    }
}

#[test]
fn extracted_refs_resolve_in_a_real_graph() {
    // minimal.ifc carries 5 rel types with a valid graph. Every ref the
    // rules extract must point at a record that actually exists — a wrong
    // index would read a `$`/string/number (→ empty) or a stray id.
    let doc = Doc::open_editable(&fixtures_dir().join("minimal.ifc")).expect("open");
    let mut seen_types: HashSet<String> = HashSet::new();

    for &id in doc.ids() {
        let bytes = doc.record_bytes(id).expect("present");
        let (rule, anchor, pull) = match parse_rel(bytes) {
            Some(t) => t,
            None => continue,
        };
        seen_types.insert(String::from_utf8_lossy(rule.type_name).into_owned());

        assert!(!anchor.is_empty(), "empty anchor on #{} ({:?})", id, rule.type_name);
        assert!(!pull.is_empty(), "empty pull on #{} ({:?})", id, rule.type_name);
        for r in anchor.iter().chain(pull.iter()) {
            assert!(doc.contains(*r), "unresolved ref #{} on #{}", r, id);
        }
        // Anchor ∪ pull must be a subset of the record's forward refs
        // (nothing invented).
        let fwd: HashSet<u64> = forward_refs(&doc, id).into_iter().collect();
        for r in anchor.iter().chain(pull.iter()) {
            assert!(fwd.contains(r), "extracted #{} not a forward ref of #{}", r, id);
        }
    }

    for t in ["IFCRELAGGREGATES", "IFCRELCONTAINEDINSPATIALSTRUCTURE",
              "IFCRELDEFINESBYPROPERTIES", "IFCRELASSOCIATESMATERIAL",
              "IFCRELASSOCIATESCLASSIFICATION"] {
        assert!(seen_types.contains(t), "fixture drifted: no {} record", t);
    }
}

#[test]
#[ignore = "requires real corpus files via IFCFAST_CORPUS (colon-separated paths)"]
fn rel_indices_resolve_across_diverse_corpus() {
    // The real-data pin: for every known rel in every corpus file, both
    // anchor and pull must be non-empty and resolve to existing records.
    // This is what a synthetic fixture cannot prove — that the pinned
    // indices match what Revit/IfcOpenShell/MagiCAD actually write.
    //   IFCFAST_CORPUS="/a.ifc:/b.ifc:..." cargo test -p ifcfast-core \
    //     --no-default-features --test doc_rel_rules -- --ignored --nocapture
    let raw = match std::env::var("IFCFAST_CORPUS") {
        Ok(s) => s,
        Err(_) => {
            eprintln!("IFCFAST_CORPUS unset — skipping");
            return;
        }
    };
    let paths: Vec<&str> = raw.split(':').filter(|s| !s.is_empty()).collect();
    assert!(!paths.is_empty());

    for p in paths {
        let path = PathBuf::from(p);
        let doc = Doc::open_editable(&path).expect("open_editable");

        let mut counts: std::collections::HashMap<String, usize> = Default::default();
        for &id in doc.ids() {
            let bytes = doc.record_bytes(id).expect("present");
            let (rule, anchor, pull) = match parse_rel(bytes) {
                Some(t) => t,
                None => continue,
            };
            let tname = String::from_utf8_lossy(rule.type_name).into_owned();
            *counts.entry(tname.clone()).or_default() += 1;

            assert!(!anchor.is_empty(), "empty anchor on #{} ({}) in {:?}", id, tname, path);
            assert!(!pull.is_empty(), "empty pull on #{} ({}) in {:?}", id, tname, path);
            for r in anchor.iter().chain(pull.iter()) {
                assert!(
                    doc.contains(*r),
                    "unresolved rel ref #{} on #{} ({}) in {:?}",
                    r, id, tname, path
                );
            }
        }
        eprintln!("OK {:?}: rel counts {:?}", path.file_name().unwrap(), counts);
    }
}
