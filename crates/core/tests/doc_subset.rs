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

use _core::doc::{emit, forward_refs, reachable_closure, Doc};

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
