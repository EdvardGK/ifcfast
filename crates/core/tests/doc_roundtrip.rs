//! Phase 1 gate for the IFC writer (GH #124): open → emit → byte-identical.
//!
//! The whole subset/hotswap edifice rests on faithful re-serialisation.
//! This test proves it the only way that admits no ambiguity: for every
//! committed fixture, `Doc::open_editable(f)` then `emit(.., None)` (keep
//! everything) must reproduce the source file byte for byte. If this
//! holds, a subset is just a filtered emit and a hotswap is an append +
//! pointer splice — both built on bytes we know we can round-trip.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use _core::doc::{emit, Doc};

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
fn roundtrip_is_byte_identical_keeping_everything() {
    let fixtures = ifc_fixtures();
    assert!(!fixtures.is_empty(), "no .ifc fixtures found");

    for path in fixtures {
        let original = fs::read(&path).expect("read fixture");
        let doc = Doc::open_editable(&path).expect("open_editable");
        let (out, stats) = emit(&doc, None);

        assert_eq!(
            out, original,
            "round-trip not byte-identical for {:?} (in={} out={} bytes={})",
            path, stats.records_in, stats.records_out, stats.bytes_out
        );
        assert_eq!(
            stats.records_in, stats.records_out,
            "keep-all must emit every record for {:?}",
            path
        );
    }
}

#[test]
#[ignore = "requires a real corpus file via IFCFAST_ROUNDTRIP_FILE (not committed — RAM/licence)"]
fn roundtrip_is_byte_identical_on_corpus_file() {
    // Synthetic fixtures don't exercise multi-line records, inter-record
    // comments, far-origin coordinates, or exporter whitespace quirks.
    // Point this at a real model to prove byte-identity at scale:
    //   IFCFAST_ROUNDTRIP_FILE=/path/to/G55_ARK.ifc \
    //     cargo test -p ifcfast-core --no-default-features \
    //     --test doc_roundtrip -- --ignored --nocapture
    let path = match std::env::var("IFCFAST_ROUNDTRIP_FILE") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!("IFCFAST_ROUNDTRIP_FILE unset — skipping");
            return;
        }
    };
    let original = fs::read(&path).expect("read corpus file");
    let doc = Doc::open_editable(&path).expect("open_editable");
    let (out, stats) = emit(&doc, None);
    eprintln!(
        "corpus {:?}: {} records, {} bytes",
        path, stats.records_in, stats.bytes_out
    );
    assert_eq!(stats.records_in, stats.records_out);
    assert_eq!(
        out.len(),
        original.len(),
        "length mismatch on {:?}",
        path
    );
    assert!(out == original, "round-trip not byte-identical on {:?}", path);
}

#[test]
fn subset_keeping_all_ids_equals_keep_none() {
    // An explicit keep-set containing every id must match the None
    // (keep-everything) path exactly — guards the two emit branches
    // against divergence.
    for path in ifc_fixtures() {
        let doc = Doc::open_editable(&path).expect("open_editable");
        let all: HashSet<u64> = doc.ids().iter().copied().collect();
        let (none_out, _) = emit(&doc, None);
        let (all_out, _) = emit(&doc, Some(&all));
        assert_eq!(none_out, all_out, "keep-all set != keep-none for {:?}", path);
    }
}

#[test]
fn subset_drops_only_requested_records() {
    // Dropping a single leaf record must shrink the output and remove
    // exactly that record's bytes, leaving a still-parseable document
    // (the survivors retain their verbatim spans + terminators).
    let path = fixtures_dir().join("minimal.ifc");
    let doc = Doc::open_editable(&path).expect("open_editable");
    let ids = doc.ids().to_vec();
    assert!(ids.len() >= 2, "need a couple of records to test a drop");

    // Keep all but the last id.
    let drop = *ids.last().unwrap();
    let keep: HashSet<u64> = ids.iter().copied().filter(|&x| x != drop).collect();

    let (full, _) = emit(&doc, None);
    let (subset, stats) = emit(&doc, Some(&keep));

    assert!(subset.len() < full.len(), "dropping a record must shrink output");
    assert_eq!(stats.records_out, ids.len() - 1);
    // The dropped record's `#<id>=` token must be gone from the output.
    let needle = format!("#{}=", drop);
    let needle_spaced = format!("#{} =", drop);
    let hay = String::from_utf8_lossy(&subset);
    assert!(
        !hay.contains(&needle) && !hay.contains(&needle_spaced),
        "dropped record #{} still present in subset output",
        drop
    );
}
