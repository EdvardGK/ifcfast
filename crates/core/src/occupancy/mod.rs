//! Occupancy — the **go/nogo** free-space field for constraint-aware MEP
//! rerouting (GH #63). Voxelize the disciplines' solids and the per-space
//! keep-out volumes into one dense grid; a voxel is **nogo** if it is
//! inside any obstacle or any keep-out, **go** (routable) otherwise. A
//! classic pathfinder ([`crate::routing`]) then searches the go voxels.
//!
//! This is the layer the user's mental model names directly: *"voxelize
//! the spaces, define each voxel go/nogo, find paths."* The distinctive
//! semantics over a raw solid raster:
//!
//! * **Keep-out, not bounds.** A space contributes an *occupiable* prism
//!   — its footprint × `[floor, floor + keepout_height]` (default 2.4 m
//!   from top-of-floor) — marked nogo so routes stay out of the
//!   human-usable volume. It does NOT bound the routable region.
//! * **Plenum is free by construction.** Because keep-out stops at
//!   `keepout_height` (not at the slab), the gap between it and the
//!   structure above is left `go` — exactly the technical-ceiling plenum
//!   where MEP actually routes, with no explicit "fill the gap" step.
//!
//! Free-space = grid − obstacle solids − keep-out prisms. Everything is
//! world metres (the caller bakes placement + `unit_scale`; see the GH
//! #63 design synthesis). Obstacle + keep-out geometry is consumed as
//! closed triangle meshes via [`crate::mesh::voxel::rasterize_solid_3d`];
//! keep-out prisms are built from a footprint with
//! [`crate::mesh::extrusion::extrude_polygon`], so concave footprints and
//! holes are handled for free.

use glam::{Mat4, Vec3};

use crate::mesh::extrusion::extrude_polygon;
use crate::mesh::profile::Polygon2D;
use crate::mesh::voxel::{rasterize_solid_3d, VoxelGrid};

/// Tunables for [`build`].
#[derive(Clone, Copy, Debug)]
pub struct OccupancyParams {
    /// Voxel edge length in metres. Drives resolution and memory; with
    /// C-space inflation (route the centreline as a point) this is
    /// decoupled from the rerouted profile size — pick ~50–100 mm.
    pub cell_m: f32,
    /// Default keep-out height above a space's floor, in metres, when a
    /// [`KeepoutSpace`] does not override it. 2.4 m (the usual
    /// top-of-furniture / door head) per the GH #63 default.
    pub default_keepout_m: f32,
}

impl Default for OccupancyParams {
    fn default() -> Self {
        Self { cell_m: 0.1, default_keepout_m: 2.4 }
    }
}

/// A space's occupiable keep-out: its footprint (world-metre XY polygon)
/// swept up from the floor level by a keep-out height.
#[derive(Clone, Debug)]
pub struct KeepoutSpace {
    /// Footprint in world-metre XY (outer ring + optional holes).
    pub footprint: Polygon2D,
    /// Floor level (world-metre Z = top-of-floor).
    pub floor_z_m: f32,
    /// Keep-out height above the floor in metres; `None` → params default.
    pub height_m: Option<f32>,
}

/// The go/nogo voxel field. `grid.occ[idx] != 0` is **nogo** (obstacle or
/// keep-out); zero is **go** (routable).
#[derive(Clone, Debug)]
pub struct Occupancy {
    pub grid: VoxelGrid,
}

impl Occupancy {
    /// True when voxel `(i, j, k)` is in-bounds and routable (go).
    #[inline]
    pub fn is_free(&self, i: usize, j: usize, k: usize) -> bool {
        self.grid.in_bounds(i, j, k) && !self.grid.is_occupied(i, j, k)
    }

    /// Count of routable (go) voxels.
    pub fn free_count(&self) -> usize {
        self.grid.occ.iter().filter(|&&v| v == 0).count()
    }
}

/// Build the go/nogo field over `[min, max]` (world metres). `obstacles`
/// are closed discipline solids (envelope walls, slabs, columns, beams)
/// as `(vertices, indices)` in world metres; `keepouts` are per-space
/// occupiable prisms. Returns `None` if the grid cannot be allocated
/// (non-finite bounds / non-positive cell / overflow).
pub fn build(
    min: Vec3,
    max: Vec3,
    params: OccupancyParams,
    obstacles: &[(&[f32], &[u32])],
    keepouts: &[KeepoutSpace],
) -> Option<Occupancy> {
    let mut grid = VoxelGrid::from_bounds(min, max, params.cell_m)?;

    // Hard obstacles: stamp each solid's interior nogo.
    for (vertices, indices) in obstacles {
        rasterize_solid_3d(&mut grid, vertices, indices);
    }

    // Keep-out: sweep each space footprint into a prism and stamp it nogo.
    for space in keepouts {
        if space.footprint.outer.len() < 3 {
            continue;
        }
        let height = space.height_m.unwrap_or(params.default_keepout_m);
        if !height.is_finite() || height <= 0.0 {
            continue;
        }
        let prism = extrude_polygon(
            &space.footprint,
            Vec3::Z,
            height,
            Mat4::from_translation(Vec3::new(0.0, 0.0, space.floor_z_m)),
        );
        rasterize_solid_3d(&mut grid, &prism.vertices, &prism.indices);
    }

    Some(Occupancy { grid })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;

    /// Axis-aligned box `[min, max]` as a closed 12-triangle mesh.
    fn box_mesh(min: Vec3, max: Vec3) -> (Vec<f32>, Vec<u32>) {
        let v = [
            [min.x, min.y, min.z],
            [max.x, min.y, min.z],
            [max.x, max.y, min.z],
            [min.x, max.y, min.z],
            [min.x, min.y, max.z],
            [max.x, min.y, max.z],
            [max.x, max.y, max.z],
            [min.x, max.y, max.z],
        ];
        let verts: Vec<f32> = v.iter().flat_map(|p| p.iter().copied()).collect();
        let idx: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        (verts, idx)
    }

    fn rect_footprint(min: Vec2, max: Vec2) -> Polygon2D {
        Polygon2D {
            outer: vec![
                Vec2::new(min.x, min.y),
                Vec2::new(max.x, min.y),
                Vec2::new(max.x, max.y),
                Vec2::new(min.x, max.y),
            ],
            holes: Vec::new(),
        }
    }

    #[test]
    fn keepout_blocks_below_but_plenum_above_stays_free() {
        // A 4×4 m room, floor z=0, with a 0.3 m structural slab at the top
        // (z=3.0..3.3). Keep-out 2.4 m. The routable plenum is the
        // z=2.4..3.0 gap; below 2.4 is nogo, the slab is nogo.
        let min = Vec3::new(0.0, 0.0, 0.0);
        let max = Vec3::new(4.0, 4.0, 3.3);
        let slab = box_mesh(Vec3::new(0.0, 0.0, 3.0), Vec3::new(4.0, 4.0, 3.3));
        let keep = KeepoutSpace {
            footprint: rect_footprint(Vec2::ZERO, Vec2::new(4.0, 4.0)),
            floor_z_m: 0.0,
            height_m: None, // default 2.4
        };
        let occ = build(
            min,
            max,
            OccupancyParams::default(),
            &[(&slab.0, &slab.1)],
            &[keep],
        )
        .unwrap();

        let g = &occ.grid;
        let mid = g.world_to_voxel(Vec3::new(2.0, 2.0, 1.0)).unwrap();
        assert!(!occ.is_free(mid[0], mid[1], mid[2]), "1.0 m is in keep-out");
        let plenum = g.world_to_voxel(Vec3::new(2.0, 2.0, 2.7)).unwrap();
        assert!(occ.is_free(plenum[0], plenum[1], plenum[2]), "2.7 m plenum is go");
        let inslab = g.world_to_voxel(Vec3::new(2.0, 2.0, 3.15)).unwrap();
        assert!(!occ.is_free(inslab[0], inslab[1], inslab[2]), "slab is nogo");
    }

    #[test]
    fn obstacle_only_field() {
        // A single 1 m cube obstacle in a 3 m box; everything else go.
        let (v, i) = box_mesh(Vec3::new(1.0, 1.0, 1.0), Vec3::new(2.0, 2.0, 2.0));
        let occ = build(
            Vec3::ZERO,
            Vec3::splat(3.0),
            OccupancyParams { cell_m: 0.1, default_keepout_m: 2.4 },
            &[(&v, &i)],
            &[],
        )
        .unwrap();
        let inside = occ.grid.world_to_voxel(Vec3::splat(1.5)).unwrap();
        assert!(!occ.is_free(inside[0], inside[1], inside[2]));
        let outside = occ.grid.world_to_voxel(Vec3::splat(0.5)).unwrap();
        assert!(occ.is_free(outside[0], outside[1], outside[2]));
    }
}
