//! GH #210 — a half-space clip keeps its plane in f64.
//!
//! The wall is an `IfcFacetedBrep` box (5.0 × 0.2 × 3.0 m) whose vertices
//! are baked at (6 500 000, 1 200 000, 50) m; the brep kernel rebases it by
//! its bbox-min, so the host is exact in its local frame. The clip is an
//! `IfcBooleanResult(.DIFFERENCE.)` whose half-space plane is *also* baked:
//! `BaseSurface.Position` at x = 6 500 003.37 m. The `f32` ulp there is
//! 0.5 m, so before the fix the plane landed at 6 500 003.5 and the wall
//! read 2.1000 m³ instead of ifcopenshell's 2.0220 (+3.9 %).
//!
//! Oracle: ifcopenshell 0.8.5, `disable-opening-subtractions` True,
//! `use-world-coords` True; pre-fix = ifcfast v0.6.2 (`c03d113`), no-cut.
//!
//! | fixture                          | pre-fix  | ifcopenshell |
//! |----------------------------------|----------|--------------|
//! | far_origin_clip_plane_210.ifc    | 2.100000 | 2.022000     |
//! | far_origin_clip_pbhs_210.ifc     | 2.707500 | 2.682150     |
//! | origin_clip_plane_210.ifc        | 2.022000 | 2.022000     |
//! | origin_clip_pbhs_210.ifc         | 2.682150 | 2.682150     |
//!
//! The `origin_*` twins are the same geometry at the origin; their output
//! is bit-identical before and after the fix (the f64 path is gated at
//! 10 km, like the global shift).

#![cfg(feature = "mesh")]

use std::path::{Path, PathBuf};

use _core::mesh::{mesh_ifc_framed, BakeFrame, ProductMesh};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn wall(name: &str) -> (ProductMesh, _core::mesh::MeshStats) {
    let buf = std::fs::read(fixture(name)).expect("fixture readable");
    let (meshes, stats) = mesh_ifc_framed(&buf, BakeFrame::Local);
    let w = meshes
        .into_iter()
        .find(|m| m.guid == "2W7lCZ0vP0Bw2rZa8K6h10")
        .expect("the clipped wall is meshed");
    (w, stats)
}

/// Closed-mesh volume by the divergence theorem, in f64, taken about the
/// vertex centroid so the result does not depend on the frame's offset.
fn volume(m: &ProductMesh) -> f64 {
    let v: Vec<[f64; 3]> = m
        .vertices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| [c[0] as f64, c[1] as f64, c[2] as f64])
        .collect();
    let n = v.len() as f64;
    let c = v.iter().fold([0.0; 3], |a, p| {
        [a[0] + p[0] / n, a[1] + p[1] / n, a[2] + p[2] / n]
    });
    let sub = |p: [f64; 3]| [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
    m.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let (a, b, d) = (
                sub(v[t[0] as usize]),
                sub(v[t[1] as usize]),
                sub(v[t[2] as usize]),
            );
            (a[0] * (b[1] * d[2] - b[2] * d[1]) - a[1] * (b[0] * d[2] - b[2] * d[0])
                + a[2] * (b[0] * d[1] - b[1] * d[0]))
                / 6.0
        })
        .sum::<f64>()
        .abs()
}

fn assert_rel(got: f64, want: f64, what: &str) {
    assert!(
        ((got - want) / want).abs() < 1.0e-4,
        "{what}: got {got} m³, ifcopenshell (openings disabled) {want} m³"
    );
}

#[test]
fn far_origin_plane_clip_matches_ifcopenshell() {
    let (w, stats) = wall("far_origin_clip_plane_210.ifc");
    // 3.37 × 0.2 × 3.0 — pre-fix 2.1000 (plane snapped to x = …3.5).
    assert_rel(volume(&w), 2.022, "IfcHalfSpaceSolid, far origin");
    assert_eq!(stats.halfspace_clip_unapplied, 0);
}

#[test]
fn far_origin_bounded_clip_matches_ifcopenshell() {
    let (w, stats) = wall("far_origin_clip_pbhs_210.ifc");
    // 3.0 − 1.63 × 0.13 × 1.5 — the boundary edge at y = 1 200 000.13
    // needs the f64 boundary frame as much as the plane needs its origin.
    assert_rel(
        volume(&w),
        2.68215,
        "IfcPolygonalBoundedHalfSpace, far origin",
    );
    assert_eq!(stats.halfspace_clip_unapplied, 0);
    assert_eq!(stats.halfspace_clip_manifold, 0);
}

#[test]
fn near_origin_twins_match_ifcopenshell() {
    let (w, _) = wall("origin_clip_plane_210.ifc");
    assert_rel(volume(&w), 2.022, "IfcHalfSpaceSolid, origin");
    let (w, _) = wall("origin_clip_pbhs_210.ifc");
    assert_rel(volume(&w), 2.68215, "IfcPolygonalBoundedHalfSpace, origin");
}

/// The far and near fixtures are the same solid; after the fix they agree
/// to f32 resolution of the local frame, where before they differed by
/// 3.9 % (plane) and 0.9 % (bounded).
#[test]
fn far_origin_equals_near_origin_twin() {
    for (far, near) in [
        ("far_origin_clip_plane_210.ifc", "origin_clip_plane_210.ifc"),
        ("far_origin_clip_pbhs_210.ifc", "origin_clip_pbhs_210.ifc"),
    ] {
        let (a, _) = wall(far);
        let (b, _) = wall(near);
        assert_eq!(a.indices.len(), b.indices.len(), "{far}: triangle count");
        let (va, vb) = (volume(&a), volume(&b));
        assert!(((va - vb) / vb).abs() < 1e-6, "{far}: {va} vs {near}: {vb}");
    }
}
