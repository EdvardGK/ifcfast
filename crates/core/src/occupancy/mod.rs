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
/// keep-out); zero is **go** (routable). `clearance2_vox[idx]` is the
/// **squared** distance (in voxel units) from each voxel centre to the
/// nearest nogo voxel centre — the configuration-space clearance field.
/// One field serves every object diameter: a route centreline that needs
/// radius `r` is admissible exactly where `clearance(v) > r`
/// ([`Occupancy::is_clear`]), so the same build routes a thin pipe and a
/// fat duct, each at its own radius (GH #63).
#[derive(Clone, Debug)]
pub struct Occupancy {
    pub grid: VoxelGrid,
    /// Squared distance-to-nearest-nogo in voxel² (Felzenszwalb–Huttenlocher
    /// exact EDT). Nogo voxels are `0.0`; an empty grid is uniformly the
    /// `big` sentinel (clear at any radius). Index with `grid.idx`.
    pub clearance2_vox: Vec<f32>,
}

impl Occupancy {
    /// True when voxel `(i, j, k)` is in-bounds and routable (go), ignoring
    /// object size. Equivalent to `is_clear(.., 0.0)`.
    #[inline]
    pub fn is_free(&self, i: usize, j: usize, k: usize) -> bool {
        self.grid.in_bounds(i, j, k) && !self.grid.is_occupied(i, j, k)
    }

    /// True when an object whose centreline needs `clearance_m` metres of
    /// room can occupy voxel `(i, j, k)` — i.e. the nearest nogo voxel is
    /// strictly more than `clearance_m` away. `clearance_m <= 0` reduces to
    /// [`is_free`](Self::is_free) (any non-occupied voxel). This is the
    /// C-space inflation, evaluated per-radius against the prebuilt field
    /// so no obstacle dilation or rebuild is needed.
    #[inline]
    pub fn is_clear(&self, i: usize, j: usize, k: usize, clearance_m: f32) -> bool {
        if !self.grid.in_bounds(i, j, k) {
            return false;
        }
        let d2 = self.clearance2_vox[self.grid.idx(i, j, k)];
        if clearance_m <= 0.0 {
            return d2 > 0.0; // not the obstacle voxel itself
        }
        let cell = self.grid.cell;
        d2 * cell * cell > clearance_m * clearance_m
    }

    /// World-metre clearance at voxel `(i, j, k)` — distance from its centre
    /// to the nearest nogo. `f32::INFINITY`-large for an obstacle-free grid;
    /// `0` for a nogo voxel. The "width of the free channel" at that point.
    #[inline]
    pub fn clearance_m(&self, i: usize, j: usize, k: usize) -> f32 {
        if !self.grid.in_bounds(i, j, k) {
            return 0.0;
        }
        self.clearance2_vox[self.grid.idx(i, j, k)].sqrt() * self.grid.cell
    }

    /// Count of routable (go) voxels, ignoring object size.
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

    let clearance2_vox = edt::squared_distance_field(&grid);
    Some(Occupancy { grid, clearance2_vox })
}

/// Exact squared Euclidean distance transform (Felzenszwalb & Huttenlocher,
/// "Distance Transforms of Sampled Functions", 2012). Separable: a 1D
/// lower-envelope-of-parabolas pass along X, then Y, then Z, each O(n) per
/// line, gives the exact squared distance (voxel²) from every voxel to the
/// nearest source. Sources here are the nogo voxels, so the result is the
/// clearance field consumed by [`Occupancy::is_clear`].
mod edt {
    use crate::mesh::voxel::VoxelGrid;

    /// 1D squared distance transform of seed costs `f` into `out`. `v`/`z`
    /// are caller scratch (`v` len ≥ n, `z` len ≥ n+1) so the 3D driver
    /// allocates once, not per line.
    fn dt_1d(f: &[f32], out: &mut [f32], v: &mut [usize], z: &mut [f32]) {
        let n = f.len();
        if n == 0 {
            return;
        }
        // Intersection abscissa of the parabolas seeded at q and p.
        let para = |q: usize, p: usize| -> f32 {
            let fq = f[q] + (q * q) as f32;
            let fp = f[p] + (p * p) as f32;
            (fq - fp) / (2.0 * q as f32 - 2.0 * p as f32)
        };
        let mut k = 0usize; // index of the rightmost parabola in the lower envelope
        v[0] = 0;
        z[0] = f32::NEG_INFINITY;
        z[1] = f32::INFINITY;
        for q in 1..n {
            let mut s = para(q, v[k]);
            // z[0] = -inf guards the decrement: a finite s is never <= -inf,
            // so k never underflows past 0.
            while s <= z[k] {
                k -= 1;
                s = para(q, v[k]);
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
        }
        let mut k = 0usize;
        for (q, slot) in out.iter_mut().enumerate() {
            while z[k + 1] < q as f32 {
                k += 1;
            }
            let d = q as f32 - v[k] as f32;
            *slot = d * d + f[v[k]];
        }
    }

    /// Squared distance (voxel²) from every voxel to the nearest occupied
    /// (nogo) voxel. Obstacle-free grids come back uniformly at a finite
    /// `big` sentinel (larger than any achievable squared distance).
    pub fn squared_distance_field(grid: &VoxelGrid) -> Vec<f32> {
        let [nx, ny, nz] = grid.dims;
        let n = nx * ny * nz;
        if n == 0 {
            return Vec::new();
        }
        // Sentinel strictly exceeds the largest real squared distance and
        // stays exact in f32 (integers ≤ 2^24).
        let big = (nx * nx + ny * ny + nz * nz) as f32 + 1.0;
        let mut f: Vec<f32> = grid
            .occ
            .iter()
            .map(|&o| if o != 0 { 0.0 } else { big })
            .collect();

        let lin = |i: usize, j: usize, k: usize| (k * ny + j) * nx + i;
        let maxdim = nx.max(ny).max(nz);
        let mut line = vec![0f32; maxdim];
        let mut out = vec![0f32; maxdim];
        let mut v = vec![0usize; maxdim];
        let mut z = vec![0f32; maxdim + 1];

        // Pass 1: along X.
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    line[i] = f[lin(i, j, k)];
                }
                dt_1d(&line[..nx], &mut out[..nx], &mut v[..nx], &mut z[..nx + 1]);
                for i in 0..nx {
                    f[lin(i, j, k)] = out[i];
                }
            }
        }
        // Pass 2: along Y.
        for k in 0..nz {
            for i in 0..nx {
                for j in 0..ny {
                    line[j] = f[lin(i, j, k)];
                }
                dt_1d(&line[..ny], &mut out[..ny], &mut v[..ny], &mut z[..ny + 1]);
                for j in 0..ny {
                    f[lin(i, j, k)] = out[j];
                }
            }
        }
        // Pass 3: along Z.
        for j in 0..ny {
            for i in 0..nx {
                for k in 0..nz {
                    line[k] = f[lin(i, j, k)];
                }
                dt_1d(&line[..nz], &mut out[..nz], &mut v[..nz], &mut z[..nz + 1]);
                for k in 0..nz {
                    f[lin(i, j, k)] = out[k];
                }
            }
        }
        f
    }
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
