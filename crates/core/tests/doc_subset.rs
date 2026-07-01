//! Phase 2 (GH #124): forward-reachability closure engine for subset.
//!
//! These guard the dependency-closure primitive that subset is built on.
//! The load-bearing invariant: the closure of any seed set is
//! *forward-closed* — emitting exactly it leaves no dangling `#ref`. The
//! relationship pass (spatial path + IfcRel* pruning) lands next; this
//! covers the reachability core it composes with.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use _core::doc::{emit, forward_refs, parse_rel, reachable_closure, subset, Doc};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn ifc_fixtures() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(fixtures_dir())
        .expect("fixtures dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("ifc"))
        .collect();
    out.sort();
    out
}

#[test]
fn closure_is_forward_closed_for_every_single_seed() {
    // For each record as a lone seed, the closure must contain all of
    // every reachable record's forward references — no dangling ref.
    for path in ifc_fixtures() {
        let doc = Doc::open_editable(&path).expect("open");
        for &seed in doc.ids() {
            let keep = reachable_closure(&doc, &[seed]);
            assert!(keep.contains(&seed));
            for &id in &keep {
                for r in forward_refs(&doc, id) {
                    // Only entities that EXIST but were dropped count as
                    // dangling; a ref to a never-present id (already
                    // broken source, e.g. broken_conversion_unit.ifc)
                    // can't be kept and isn't the closure's fault.
                    if doc.contains(r) {
                        assert!(
                            keep.contains(&r),
                            "dangling ref #{} from #{} (seed #{}) in {:?}",
                            r, id, seed, path
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn closure_of_all_ids_is_everything() {
    for path in ifc_fixtures() {
        let doc = Doc::open_editable(&path).expect("open");
        let all: Vec<u64> = doc.ids().to_vec();
        let keep = reachable_closure(&doc, &all);
        assert_eq!(keep.len(), all.len(), "closure(all) must equal all in {:?}", path);
    }
}

#[test]
#[ignore = "requires real corpus files via IFCFAST_CORPUS (colon-separated paths)"]
fn corpus_roundtrip_and_closure_across_diverse_files() {
    // Guards against designing to one file's quirks: runs both writer
    // invariants over a DIVERSE set (IFC2x3/4/4x3, Revit/IfcOpenShell/
    // MagiCAD, 60KB..284MB, ARK/RIV/MEP). Set:
    //   IFCFAST_CORPUS="/a.ifc:/b.ifc:..." cargo test -p ifcfast-core \
    //     --no-default-features --test doc_subset -- --ignored --nocapture
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
        let original = fs::read(&path).expect("read corpus file");
        let doc = Doc::open_editable(&path).expect("open_editable");

        // (1) byte-identity round-trip.
        let (out, stats) = emit(&doc, None);
        assert_eq!(stats.records_in, stats.records_out);
        assert!(
            out == original,
            "round-trip NOT byte-identical: {:?} ({} records)",
            path, stats.records_in
        );

        // (2) forward-closed closure invariant, on ~300 seeds spread
        // across the file (every single-seed test is O(n^2) — infeasible
        // at millions of records, so sample).
        let ids = doc.ids();
        let step = (ids.len() / 300).max(1);
        let mut checked = 0usize;
        for &seed in ids.iter().step_by(step) {
            let keep = reachable_closure(&doc, &[seed]);
            for &id in &keep {
                for r in forward_refs(&doc, id) {
                    if doc.contains(r) {
                        assert!(
                            keep.contains(&r),
                            "dangling ref #{} from #{} (seed #{}) in {:?}",
                            r, id, seed, path
                        );
                    }
                }
            }
            checked += 1;
        }

        // (3) a combined subset of the sampled seeds must emit and
        // reopen forward-closed (dropped in-graph deps == defect).
        let seeds: Vec<u64> = ids.iter().copied().step_by(step).collect();
        let keep = reachable_closure(&doc, &seeds);
        let (bytes, _) = emit(&doc, Some(&keep));
        let re = Doc::from_bytes(bytes);
        let re_ids: HashSet<u64> = re.ids().iter().copied().collect();
        for &id in &re_ids {
            for r in forward_refs(&re, id) {
                if !re.contains(r) {
                    assert!(
                        !doc.contains(r),
                        "reopened subset dropped in-graph dep #{} (from #{}) in {:?}",
                        r, id, path
                    );
                }
            }
        }

        eprintln!(
            "OK {:?}: {} records, {} bytes, {} seeds checked, subset={} records",
            path.file_name().unwrap(), stats.records_in, out.len(), checked, keep.len()
        );
    }
}

/// Assert `re` has no forward ref to an entity that was present in `orig`
/// but got dropped — i.e. the subset is self-contained (refs to
/// never-present ids in an already-broken source don't count).
fn assert_no_dropped_deps(re: &Doc, orig: &Doc) {
    let present: HashSet<u64> = re.ids().iter().copied().collect();
    for &id in &present {
        for r in forward_refs(re, id) {
            if !re.contains(r) {
                assert!(
                    !orig.contains(r),
                    "subset dropped in-graph dependency #{} (from #{})",
                    r, id
                );
            }
        }
    }
}

#[test]
fn subset_climbs_spine_pulls_defs_and_prunes_shared_rels() {
    // Seed one of two walls that share a storey, pset and material. The
    // subset must climb Storey→Building→Site→Project, pull the pset/material,
    // and rewrite every shared rel's RelatedObjects SET from (#30,#31) to
    // (#30) — dropping wall B entirely.
    let doc = Doc::open_editable(&fixtures_dir().join("subset_prune.ifc")).expect("open");
    let (bytes, stats) = subset(&doc, &[30]);
    assert_eq!(stats.seeds_present, 1);

    let re = Doc::from_bytes(bytes);

    // Wall A survives; wall B is gone.
    assert!(re.contains(30), "seeded wall A must survive");
    assert!(!re.contains(31), "unseeded wall B must be dropped");

    // Spatial spine to IfcProject.
    for s in [1u64, 10, 11, 12] {
        assert!(re.contains(s), "spatial ancestor #{} missing", s);
    }
    // Pulled definitions (pset + its props, material).
    for d in [40u64, 41, 42, 50] {
        assert!(re.contains(d), "pulled definition #{} missing", d);
    }
    // Retained relationships (spine aggregates + attachments + containment).
    for r in [20u64, 21, 22, 43, 51, 60] {
        assert!(re.contains(r), "relationship #{} missing", r);
    }

    // Self-contained: nothing dangling.
    assert_no_dropped_deps(&re, &doc);

    // The three shared rels had their anchor SET rewritten to just #30.
    for rel in [43u64, 51, 60] {
        let span = re.record_bytes(rel).expect("rel present");
        let (_rule, anchor, _pull) = parse_rel(span).expect("known rel");
        assert_eq!(anchor, vec![30], "rel #{} anchor not pruned to [30]", rel);
    }
    assert_eq!(stats.rels_pruned, 3, "exactly the 3 shared rels prune");
}

#[test]
fn subset_of_all_ids_is_byte_identical_to_source() {
    // The whole-document degenerate case must reproduce the source: closure
    // of all ids is everything, every rel keeps its full anchor, no override.
    for name in ["minimal.ifc", "geom_box.ifc", "materials.ifc", "quantities.ifc"] {
        let doc = Doc::open_editable(&fixtures_dir().join(name)).expect("open");
        let all: Vec<u64> = doc.ids().to_vec();
        let (sub_bytes, stats) = subset(&doc, &all);
        let (full_bytes, _) = emit(&doc, None);
        assert_eq!(stats.rels_pruned, 0, "no pruning when keeping all ({})", name);
        assert!(sub_bytes == full_bytes, "subset(all) != source for {}", name);
    }
}

#[test]
fn subset_single_seed_reopens_self_contained() {
    // Every single-product seed across the fixtures yields a self-contained
    // subset — no dangling ref, seed present.
    for path in ifc_fixtures() {
        let doc = Doc::open_editable(&path).expect("open");
        for &seed in doc.ids() {
            let (bytes, _) = subset(&doc, &[seed]);
            let re = Doc::from_bytes(bytes);
            assert!(re.contains(seed) || !doc.contains(seed), "seed #{} lost in {:?}", seed, path);
            assert_no_dropped_deps(&re, &doc);
        }
    }
}

#[test]
#[ignore = "requires real corpus files via IFCFAST_CORPUS (colon-separated paths)"]
fn subset_across_corpus_is_self_contained() {
    // Runs the full subset pass (forward closure + rel activation + anchor
    // prune) over diverse real files. For each, seeds a spread of ~200
    // records and asserts the emitted subset re-opens with no dangling ref
    // to a dropped in-graph entity. If IFCFAST_SUBSET_DIR is set, writes
    // each subset to <dir>/<name>.subset.ifc for external ifcopenshell
    // validation (the spatial-tree + zero-dangling acceptance gate).
    //   IFCFAST_CORPUS="/a.ifc:/b.ifc" IFCFAST_SUBSET_DIR=/tmp/subs \
    //     cargo test -p ifcfast-core --no-default-features --test doc_subset \
    //     -- --ignored subset_across_corpus --nocapture
    let raw = match std::env::var("IFCFAST_CORPUS") {
        Ok(s) => s,
        Err(_) => {
            eprintln!("IFCFAST_CORPUS unset — skipping");
            return;
        }
    };
    let out_dir = std::env::var("IFCFAST_SUBSET_DIR").ok();
    let paths: Vec<&str> = raw.split(':').filter(|s| !s.is_empty()).collect();
    assert!(!paths.is_empty());

    for p in paths {
        let path = PathBuf::from(p);
        let doc = Doc::open_editable(&path).expect("open_editable");
        let ids = doc.ids();
        let step = (ids.len() / 200).max(1);
        let seeds: Vec<u64> = ids.iter().copied().step_by(step).collect();

        let (bytes, stats) = subset(&doc, &seeds);
        let re = Doc::from_bytes(bytes.clone());
        assert_no_dropped_deps(&re, &doc);
        assert!(
            re.len() <= doc.len(),
            "subset larger than source in {:?}",
            path
        );

        if let Some(dir) = &out_dir {
            let name = path.file_stem().unwrap().to_string_lossy();
            let out = PathBuf::from(dir).join(format!("{}.subset.ifc", name));
            fs::write(&out, &bytes).expect("write subset");
            eprintln!("wrote {:?}", out);
        }
        eprintln!(
            "OK {:?}: {} seeds → {} records ({} rels, {} pruned) of {}",
            path.file_name().unwrap(),
            seeds.len(),
            stats.records_out,
            stats.rels_kept,
            stats.rels_pruned,
            doc.len()
        );
    }
}

#[test]
fn closure_subset_emits_and_reopens_forward_closed() {
    // Emitting a proper (non-full) closure yields bytes that re-open as
    // a Doc whose every record is still forward-closed — i.e. a valid,
    // self-contained STEP fragment (relationships aside, which the rel
    // pass handles next).
    let path = fixtures_dir().join("geom_box.ifc");
    let doc = Doc::open_editable(&path).expect("open");
    // Seed from a mid-graph record so the closure is a strict subset.
    let seed = *doc.ids().last().expect("has records");
    let keep = reachable_closure(&doc, &[seed]);
    assert!(keep.len() <= doc.len());

    let (bytes, stats) = emit(&doc, Some(&keep));
    assert_eq!(stats.records_out, keep.len());

    let reopened = Doc::from_bytes(bytes);
    let re_ids: HashSet<u64> = reopened.ids().iter().copied().collect();
    for &id in &re_ids {
        for r in forward_refs(&reopened, id) {
            if reopened.contains(r) {
                continue;
            }
            // A ref absent from the reopened doc is only a defect if it
            // was present in the ORIGINAL (i.e. we dropped a dependency).
            assert!(
                !doc.contains(r),
                "reopened subset dropped an in-graph dependency #{} (from #{})",
                r, id
            );
        }
    }
}
