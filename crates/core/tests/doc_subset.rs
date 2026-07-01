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
