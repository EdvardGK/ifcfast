//! `BakeFrame::ItemLocal` — the representation-item frame (GH #127).
//!
//! The contract under test: for every product,
//!
//!   `world_vertex ≈ placement · item_local_vertex`
//!
//! where `placement` is the product's resolved `ObjectPlacement` chain
//! (`ProductMesh.world_transform` rotation + f64 `world_origin`
//! translation) and `item_local_vertex` comes from an `ItemLocal` bake.
//! That identity is exactly what makes `ItemLocal` the hotswap input
//! frame: `doc::hotswap` writes the vertices verbatim as `Body` item
//! coordinates, and any consumer applies the (untouched) placement on
//! top.
//!
//! Fixture: `hotswap_roundtrip.ifc` — WallA is a direct extrusion under
//! a translated + 90°-rotated placement; WallB is a mapped item with a
//! `LocalOrigin` offset (the mapping composition is part of the item
//! frame and must survive; only the placement is dropped).

#![cfg(feature = "mesh")]

use _core::mesh::{mesh_ifc_streaming_framed, BakeFrame, ProductMesh, VecSink};
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn mesh_framed(buf: &[u8], frame: BakeFrame) -> Vec<ProductMesh> {
    let mut sink = VecSink::default();
    mesh_ifc_streaming_framed(buf, &mut sink, frame);
    sink.products
}

/// Apply the product's world placement (f32 rotation columns, f64
/// translation) to an item-local point, in f64.
fn apply_placement(world: &[f32; 16], origin: &[f64; 3], p: [f32; 3]) -> [f64; 3] {
    // `world_transform` is col-major: element (r, c) = world[c*4 + r].
    let (x, y, z) = (p[0] as f64, p[1] as f64, p[2] as f64);
    [
        world[0] as f64 * x + world[4] as f64 * y + world[8] as f64 * z + origin[0],
        world[1] as f64 * x + world[5] as f64 * y + world[9] as f64 * z + origin[1],
        world[2] as f64 * x + world[6] as f64 * y + world[10] as f64 * z + origin[2],
    ]
}

#[test]
fn item_local_composes_to_world_through_placement() {
    let buf = std::fs::read(fixtures_dir().join("hotswap_roundtrip.ifc")).unwrap();
    let world = mesh_framed(&buf, BakeFrame::World);
    let local = mesh_framed(&buf, BakeFrame::ItemLocal);
    assert_eq!(world.len(), 2, "fixture has two walls");
    assert_eq!(local.len(), world.len());

    for (w, l) in world.iter().zip(local.iter()) {
        assert_eq!(w.guid, l.guid);
        assert_eq!(w.vertices.len(), l.vertices.len());
        assert_eq!(w.indices, l.indices, "frames must not reorder topology");

        let mut worst = 0.0_f64;
        for (wc, lc) in w.vertices.chunks_exact(3).zip(l.vertices.chunks_exact(3)) {
            let mapped =
                apply_placement(&w.world_transform, &w.world_origin, [lc[0], lc[1], lc[2]]);
            for k in 0..3 {
                worst = worst.max((mapped[k] - wc[k] as f64).abs());
            }
        }
        // mm-unit fixture: 0.05 mm covers the f32 bake quantum at the
        // fixture's ~10^4 mm coordinate magnitudes.
        assert!(
            worst < 5e-2,
            "{}: world != placement * item_local (worst {worst} mm)",
            w.guid
        );
    }
}

#[test]
fn item_local_drops_placement_but_keeps_mapping() {
    let buf = std::fs::read(fixtures_dir().join("hotswap_roundtrip.ifc")).unwrap();
    let world = mesh_framed(&buf, BakeFrame::World);
    let local = mesh_framed(&buf, BakeFrame::ItemLocal);

    // WallA sits at (10000, 5000, 2000): world and item-local must be
    // loudly apart — this is the double-placement trap GH #127 closes.
    let wa_w = world.iter().find(|m| m.guid == "0LocalFrameWallA000001").unwrap();
    let wa_l = local.iter().find(|m| m.guid == "0LocalFrameWallA000001").unwrap();
    let max_delta = wa_w
        .vertices
        .iter()
        .zip(wa_l.vertices.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        max_delta > 1000.0,
        "placement did not separate frames (max delta {max_delta} mm)"
    );

    // WallB's mapping LocalOrigin is (2000, 0, 0): the item-local mesh
    // must keep it (x extent reaches past 1500 mm), because the mapped
    // item IS the Body item — only the ObjectPlacement is dropped.
    let wb_l = local.iter().find(|m| m.guid == "0LocalFrameWallB000001").unwrap();
    let max_x = wb_l
        .vertices
        .chunks_exact(3)
        .map(|c| c[0])
        .fold(f32::MIN, f32::max);
    assert!(
        max_x > 1500.0,
        "mapping composition lost from the item-local frame (max x {max_x} mm)"
    );
}
