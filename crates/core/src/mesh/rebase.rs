//! Far-origin rebase: turning [`BakeFrame::Local`] vertices into
//! shifted world metres without ever casting an absolute coordinate to
//! `f32` (GH #117, GH #188).
//!
//! # Why this module exists
//!
//! [`BakeFrame::World`] bakes `effective(f32) * p + anchor(f32)`. The
//! cast to `f32` happens at the *absolute* magnitude, so on a Norwegian
//! NTM site placement authored in millimetres (`x ~ 9.2e7`,
//! `y ~ 1.25e9`) the vertex lattice is the `f32` ulp there: **8 mm in X,
//! 128 mm in Y**. A Ø400 duct wall is 2 mm thick; at that lattice a
//! circular duct wobbles ±4 mm and one face in ten collapses to zero
//! area. Subtracting a shift *after* the cast recovers nothing — the
//! residual carries the lattice.
//!
//! [`BakeFrame::Local`] keeps the placement's rotation, drops the large
//! translation, and preserves each fragment's small intra-product
//! offset, so the vertices are near-origin and `f32`-exact. The anchor
//! that puts them back in the world is [`ProductMesh::mesh_anchor`], an
//! `f64`. The encode is therefore:
//!
//! ```text
//! out = ((local + (mesh_anchor - shift)) * unit_scale) as f32
//! ```
//!
//! — all of it in `f64`, with the `f32` cast last, on a number that is
//! small because `shift` was chosen near the model. Both terms are
//! small, so the sum is `f32`-safe even for a georeferenced model.
//!
//! Every batch consumer that hands out world coordinates goes through
//! here: `_core.extract_meshes`, the glTF writer's sink
//! (`m.to_gltf()`), and the wasm `streamMeshes` / `toGlb` passes. One
//! implementation, one shift rule, one place to change it.
//!
//! [`BakeFrame::World`]: crate::mesh::BakeFrame::World
//! [`BakeFrame::Local`]: crate::mesh::BakeFrame::Local

use crate::mesh::ProductMesh;

/// Below this many metres from the origin, `f32` already resolves a
/// coordinate finely enough (~1 mm quantum at 10 km) that no shift is
/// warranted — so near-origin models keep absolute world coordinates and
/// a `[0, 0, 0]` shift. Output there is unchanged up to the f32
/// unit-factor correction: metre files are bit-identical, millimetre
/// files move by at most one f32 ulp (`0.001f32 != 0.001f64`).
const THRESHOLD_M: f64 = 1.0e4;

/// Decide the model-wide global shift from the first geometry product's
/// `f64` anchor. Returns the rounded anchor **in model units** (so
/// far-from-origin geometry is repositioned near the `f32`-precise
/// origin) when the anchor is genuinely large, otherwise `[0, 0, 0]`.
///
/// This is the CloudCompare "global shift" contract: the caller reports
/// the value (scaled to metres) alongside the vertices, and
/// `vertex + global_shift` is the absolute world coordinate.
///
/// The same rule serves `_core.extract_meshes`' `global_shift`,
/// `sample_point_cloud`, `m.to_gltf()`'s `global_shift` stat and the
/// wasm `streamShiftJson()` — one definition, so the four agree by
/// construction rather than by three ports staying in sync.
pub fn global_shift_for(world_origin: &[f64; 3], unit_scale: f64) -> [f64; 3] {
    let max_m = world_origin
        .iter()
        .map(|c| (c * unit_scale).abs())
        .fold(0.0_f64, f64::max);
    if max_m > THRESHOLD_M {
        [
            world_origin[0].round(),
            world_origin[1].round(),
            world_origin[2].round(),
        ]
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// Offset from the model-wide `shift` to this product's precise anchor,
/// in model units. Small for any product inside a sane model extent —
/// computed in `f64` so it never collapses.
#[inline]
pub fn anchor_offset(mesh_anchor: &[f64; 3], shift: &[f64; 3]) -> [f64; 3] {
    [
        mesh_anchor[0] - shift[0],
        mesh_anchor[1] - shift[1],
        mesh_anchor[2] - shift[2],
    ]
}

/// Append `mesh`'s [`BakeFrame::Local`] vertices to `out` as shifted
/// world **metres**, `f32`.
///
/// `shift` is in model units (what [`global_shift_for`] returns);
/// `unit_scale` is metres per model unit. The reposition and the metre
/// scale happen in `f64`; the `f32` cast is last and lands on a
/// near-origin number.
///
/// [`BakeFrame::Local`]: crate::mesh::BakeFrame::Local
pub fn shifted_world_positions(
    mesh: &ProductMesh,
    shift: &[f64; 3],
    unit_scale: f64,
    out: &mut Vec<f32>,
) {
    let off = anchor_offset(&mesh.mesh_anchor, shift);
    out.reserve(mesh.vertices.len());
    for chunk in mesh.vertices.as_chunks::<3>().0 {
        out.push(((chunk[0] as f64 + off[0]) * unit_scale) as f32);
        out.push(((chunk[1] as f64 + off[1]) * unit_scale) as f32);
        out.push(((chunk[2] as f64 + off[2]) * unit_scale) as f32);
    }
}

/// [`shifted_world_positions`] in place: rewrite `mesh.vertices` from
/// the Local frame into shifted world metres. For sinks that retain the
/// [`ProductMesh`] (the glTF writer's) rather than copying its positions
/// out.
///
/// Note what is **not** rewritten: `world_transform`, `mesh_anchor` and
/// `parts[].local_vertices` / `instance_transform` stay in model units,
/// because they describe the representation, not the baked output. A
/// consumer of `parts` (glTF instancing, the substrate) has to apply
/// `unit_scale` and the shift itself — see
/// [`crate::mesh::gltf::WriteOptions`].
pub fn shift_world_in_place(mesh: &mut ProductMesh, shift: &[f64; 3], unit_scale: f64) {
    let off = anchor_offset(&mesh.mesh_anchor, shift);
    for chunk in mesh.vertices.as_chunks_mut::<3>().0 {
        chunk[0] = ((chunk[0] as f64 + off[0]) * unit_scale) as f32;
        chunk[1] = ((chunk[1] as f64 + off[1]) * unit_scale) as f32;
        chunk[2] = ((chunk[2] as f64 + off[2]) * unit_scale) as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_origin_models_get_no_shift() {
        assert_eq!(global_shift_for(&[1.0, 2.0, 3.0], 1.0), [0.0, 0.0, 0.0]);
        // 9 km in metre units — still under the threshold.
        assert_eq!(global_shift_for(&[9000.0, 0.0, 0.0], 1.0), [0.0, 0.0, 0.0]);
        // The same 9 km authored in millimetres.
        assert_eq!(
            global_shift_for(&[9.0e6, 0.0, 0.0], 0.001),
            [0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn far_origin_models_round_the_anchor_in_model_units() {
        // NTM at millimetre scale — the GH #188 numbers.
        assert_eq!(
            global_shift_for(&[92_519_899.3, 1_247_315_537.2, 127_250.0], 0.001),
            [92_519_899.0, 1_247_315_537.0, 127_250.0]
        );
    }
}
