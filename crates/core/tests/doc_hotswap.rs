//! Mesh hotswap mechanism + orphan-GC correctness (GH #124 Phase 3).
//!
//! `hotswap_body.ifc` has two walls whose bodies are *mapped* to a single
//! shared `IfcRepresentationMap`. Swapping one wall's body must:
//!   - repoint its `IfcShapeRepresentation` at a new `IfcTriangulatedFaceSet`
//!     and flip its type to `Tessellation`,
//!   - reclaim the geometry that wall uniquely owned (its mapped item + the
//!     item's mapping target),
//!   - leave the *shared* map (and everything under it) alone, because the
//!     other wall still references it,
//!   - re-open with zero dangling references.

use _core::doc::{forward_refs, hotswap, Doc, HotswapError};
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

const WALL1: &str = "7XvctVUKr0kugbFTf53O9L";

/// A unit cube (8 verts, 12 tris) — a plausible decimated body.
fn unit_cube() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
    let v = vec![
        [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
    ];
    let t = vec![
        [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
        [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
        [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7],
    ];
    (v, t)
}

fn field_str(doc: &Doc, id: u64, index: usize) -> Option<String> {
    let span = doc.record_bytes(id)?;
    let (_id, _ty, args) = _core::lexer::parse_record_span(span)?;
    let split = _core::lexer::split_top_level_args(args);
    _core::lexer::decode_string(split.get(index)?)
}

#[test]
fn swaps_body_gcs_unique_items_keeps_shared_map() {
    let doc = Doc::open_editable(&fixtures_dir().join("hotswap_body.ifc")).expect("open");
    let max_id = doc.max_id();
    let (verts, tris) = unit_cube();

    let (bytes, stats) = hotswap(&doc, WALL1, &verts, &tris).expect("hotswap ok");

    // New ids allocated above the source max (IFC4 fixture → faceset).
    assert_eq!(stats.product, 30);
    assert_eq!(stats.shape_rep, 41);
    assert_eq!(stats.new_geometry, max_id + 2, "faceset above point list");
    assert_eq!(stats.new_records, 2, "point list + faceset");
    // Wall1 uniquely owned #42 (mapped item) + #44 (its mapping target).
    assert_eq!(stats.records_gc, 2, "should reclaim exactly the unique items");

    let re = Doc::from_bytes(bytes);

    // The body rep now points at the new faceset and is a Tessellation.
    assert_eq!(field_str(&re, 41, 2).as_deref(), Some("Tessellation"));
    let items = forward_refs(&re, 41);
    assert_eq!(items, vec![10, stats.new_geometry], "context + new faceset only");

    // New geometry present; the faceset backs onto the new point list (max+1).
    assert!(re.contains(stats.new_geometry));
    assert!(re.contains(max_id + 1));
    assert_eq!(forward_refs(&re, stats.new_geometry), vec![max_id + 1]);

    // Unique old items gone.
    assert!(!re.contains(42), "wall1 mapped item should be GC'd");
    assert!(!re.contains(44), "wall1 mapping target should be GC'd");

    // Shared map + its geometry survive (wall2 still references them).
    for kept in [63, 64, 65, 67, 68, 69, 10] {
        assert!(re.contains(kept), "#{kept} must survive (shared/live)");
    }
    // Wall2 is entirely untouched.
    for w2 in [50, 60, 61, 62, 66] {
        assert!(re.contains(w2), "#{w2} (wall2) must be untouched");
    }

    // Zero dangling references in the whole reopened document.
    for &id in re.ids() {
        for r in forward_refs(&re, id) {
            assert!(re.contains(r), "dangling #{r} referenced by #{id}");
        }
    }
}

#[test]
fn ifc2x3_emits_a_shell_based_surface_model() {
    // Same graph, IFC2x3 schema: the compact faceset doesn't exist there,
    // so the body must become an IfcShellBasedSurfaceModel over an open
    // shell of faces, and the rep type 'SurfaceModel'.
    let doc = Doc::open_editable(&fixtures_dir().join("hotswap_body_2x3.ifc")).expect("open");
    let (verts, tris) = unit_cube(); // 8 verts, 12 tris

    let (bytes, stats) = hotswap(&doc, WALL1, &verts, &tris).expect("hotswap ok");
    // 8 points + 12*(loop+bound+face) + open shell + sbsm = 8+36+2 = 46.
    assert_eq!(stats.new_records, 46);

    let re = Doc::from_bytes(bytes);
    assert_eq!(field_str(&re, 41, 2).as_deref(), Some("SurfaceModel"));
    // The body rep points at the new sbsm root.
    assert_eq!(forward_refs(&re, 41), vec![10, stats.new_geometry]);
    // Root is a shell-based surface model.
    let root = re.record_bytes(stats.new_geometry).unwrap();
    assert!(
        _core::lexer::parse_record_span(root).unwrap().1 == b"IFCSHELLBASEDSURFACEMODEL",
        "root should be a shell-based surface model"
    );

    // Zero dangling references end to end.
    for &id in re.ids() {
        for r in forward_refs(&re, id) {
            assert!(re.contains(r), "dangling #{r} referenced by #{id}");
        }
    }
}

#[test]
fn keep_everything_but_the_swap_is_byte_stable_elsewhere() {
    // Records other than the swapped rep + appended geometry are emitted
    // verbatim: wall2's shape rep must be byte-identical pre/post swap.
    let doc = Doc::open_editable(&fixtures_dir().join("hotswap_body.ifc")).expect("open");
    let before = doc.record_bytes(61).unwrap().to_vec();
    let (verts, tris) = unit_cube();
    let (bytes, _) = hotswap(&doc, WALL1, &verts, &tris).expect("ok");
    let re = Doc::from_bytes(bytes);
    assert_eq!(re.record_bytes(61).unwrap(), before.as_slice());
}

#[test]
fn unknown_guid_is_loud() {
    let doc = Doc::open_editable(&fixtures_dir().join("hotswap_body.ifc")).expect("open");
    let (verts, tris) = unit_cube();
    match hotswap(&doc, "THISGUIDDOESNOTEXIST00", &verts, &tris) {
        Err(HotswapError::UnknownGuid(_)) => {}
        other => panic!("expected UnknownGuid, got {other:?}"),
    }
}

#[test]
fn product_without_representation_is_loud() {
    // minimal.ifc's wall has Representation = $.
    let doc = Doc::open_editable(&fixtures_dir().join("minimal.ifc")).expect("open");
    let (verts, tris) = unit_cube();
    match hotswap(&doc, "7XvctVUKr0kugbFTf53O9L", &verts, &tris) {
        Err(HotswapError::NoRepresentation) => {}
        other => panic!("expected NoRepresentation, got {other:?}"),
    }
}

#[test]
fn bad_mesh_is_loud() {
    let doc = Doc::open_editable(&fixtures_dir().join("hotswap_body.ifc")).expect("open");
    // Empty mesh.
    assert!(matches!(
        hotswap(&doc, WALL1, &[], &[]),
        Err(HotswapError::BadMesh(_))
    ));
    // Triangle indexes a vertex out of range.
    let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let tris = vec![[0u32, 1, 9]];
    assert!(matches!(
        hotswap(&doc, WALL1, &verts, &tris),
        Err(HotswapError::BadMesh(_))
    ));
}
