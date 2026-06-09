//! 3D solid voxelization — `rasterize_solid_3d`, the foundational
//! occupancy primitive for constraint-aware MEP rerouting (GH #63) and
//! the geometry-hotswap roadmap.
//!
//! This is the 3D generalization of [`crate::mesh::qto::footprint_xy_raw`]:
//! where that marks 2D cells whose centre lies under a triangle mesh (a
//! surface *footprint*), this fills the SOLID interior of a closed mesh
//! into a dense voxel grid via per-column Z-ray even-odd parity. The
//! occupancy field it builds is the input to the reroute free-space
//! search (free-space = grid − obstacle solids, plenum-filled).
//!
//! **Units:** vertices are WORLD METRES — the caller bakes world
//! placement and `unit_scale` before calling, so occupancy is metres at
//! every boundary (the clash engine already normalises at substrate read;
//! mm exists only transiently inside extractors). See the GH #63 design
//! synthesis.

use glam::{Vec2, Vec3};

/// A dense axis-aligned voxel occupancy grid in world metres. Row-major
/// in X, then Y, then Z (`idx = (k*ny + j)*nx + i`), matching the
/// per-storey dense-grid decision in the GH #63 design (a plenum-filled
/// grid is dense, so a hash grid is pure overhead).
#[derive(Clone, Debug)]
pub struct VoxelGrid {
    /// Min corner (world metres) — the centre of voxel (0,0,0) is
    /// `origin + 0.5*cell` on every axis.
    pub origin: Vec3,
    /// Voxel edge length in metres (cubic voxels — no axis bias).
    pub cell: f32,
    /// `[nx, ny, nz]` voxel counts.
    pub dims: [usize; 3],
    /// `nx*ny*nz` occupancy bytes; `1` = occupied, `0` = free.
    pub occ: Vec<u8>,
}

impl VoxelGrid {
    /// Allocate an empty grid covering `[min, max]` at the given cell
    /// size, padded by one cell on the max side so a solid flush with the
    /// bound still lands fully inside. Returns `None` on a non-finite
    /// bound, non-positive cell, or a voxel count that overflows `usize`.
    pub fn from_bounds(min: Vec3, max: Vec3, cell: f32) -> Option<Self> {
        if !cell.is_finite() || cell <= 0.0 || !min.is_finite() || !max.is_finite() {
            return None;
        }
        let span = (max - min).max(Vec3::ZERO);
        let along = |s: f32| ((s / cell).ceil() as usize).saturating_add(1).max(1);
        let dims = [along(span.x), along(span.y), along(span.z)];
        let n = dims[0].checked_mul(dims[1])?.checked_mul(dims[2])?;
        Some(Self { origin: min, cell, dims, occ: vec![0u8; n] })
    }

    #[inline]
    pub fn idx(&self, i: usize, j: usize, k: usize) -> usize {
        (k * self.dims[1] + j) * self.dims[0] + i
    }

    #[inline]
    pub fn is_occupied(&self, i: usize, j: usize, k: usize) -> bool {
        self.occ.get(self.idx(i, j, k)).copied().unwrap_or(0) != 0
    }

    #[inline]
    pub fn in_bounds(&self, i: usize, j: usize, k: usize) -> bool {
        i < self.dims[0] && j < self.dims[1] && k < self.dims[2]
    }

    /// World-metre centre of voxel `(i, j, k)`.
    #[inline]
    pub fn voxel_center(&self, i: usize, j: usize, k: usize) -> Vec3 {
        self.origin
            + Vec3::new(
                (i as f32 + 0.5) * self.cell,
                (j as f32 + 0.5) * self.cell,
                (k as f32 + 0.5) * self.cell,
            )
    }

    /// Voxel containing world-metre point `p`, or `None` if outside the
    /// grid. The inverse of [`Self::voxel_center`] (to the nearest cell).
    #[inline]
    pub fn world_to_voxel(&self, p: Vec3) -> Option<[usize; 3]> {
        let rel = (p - self.origin) / self.cell;
        if !rel.is_finite() || rel.min_element() < 0.0 {
            return None;
        }
        let (i, j, k) = (rel.x as usize, rel.y as usize, rel.z as usize);
        if self.in_bounds(i, j, k) {
            Some([i, j, k])
        } else {
            None
        }
    }

    pub fn count_occupied(&self) -> usize {
        self.occ.iter().filter(|&&v| v != 0).count()
    }

    /// Occupied-volume estimate in m³ (occupied voxel count × cell³). A
    /// quantised proxy for the meshed solid volume — handy as a test
    /// oracle against a known box.
    pub fn occupied_volume_m3(&self) -> f32 {
        self.count_occupied() as f32 * self.cell.powi(3)
    }
}

/// Coverage diagnostics from a [`rasterize_solid_3d`] pass.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RasterStats {
    /// Voxel columns that received at least one triangle crossing.
    pub columns_hit: usize,
    /// Columns whose signed crossings did not net to zero — the
    /// open-shell / inconsistent-winding signal. A watertight,
    /// consistently-wound mesh yields zero.
    pub columns_unbalanced: usize,
    /// Voxels newly set to occupied by this pass.
    pub voxels_filled: usize,
}

const VERTICAL_EPS: f32 = 1.0e-9;

/// Fill the SOLID interior of a closed triangle mesh into `grid` (sets
/// occupied voxels to `1`; existing occupancy is preserved so many solids
/// accumulate into one shared grid). Returns coverage diagnostics.
///
/// Method — per voxel COLUMN `(i, j)` along `+Z`: every triangle whose XY
/// projection contains the column centre contributes the height `z` at
/// which the column-centre ray pierces the triangle's plane, tagged with
/// `sign(n_z)` (whether the surface faces up or down there). Sorting the
/// crossings by `z` and sweeping a **winding number** (`+1` per up-facing,
/// `-1` per down-facing) marks the inside-the-solid intervals as those
/// where the running winding is non-zero. Winding (not even-odd parity) is
/// used deliberately: when a column ray grazes a shared triangle edge the
/// inclusive point-in-triangle test reports two coincident crossings, and
/// because both faces wind the same way they *sum* under winding instead
/// of cancelling to empty as they would under even-odd. IFC extruded /
/// tessellated solids are consistently wound, so this is robust; a
/// non-watertight or inconsistently-wound column nets non-zero and is
/// counted in `columns_unbalanced`. Vertical triangles (`|n_z| < eps`,
/// ray-parallel) contribute nothing — exactly as the XY-degenerate
/// triangles drop out of `footprint_xy_raw`.
///
/// Cost is `O(triangles · covered_columns + crossings·log crossings)`.
/// Vertices are world metres (see module docs).
pub fn rasterize_solid_3d(
    grid: &mut VoxelGrid,
    vertices: &[f32],
    indices: &[u32],
) -> RasterStats {
    let [nx, ny, nz] = grid.dims;
    if nx == 0 || ny == 0 || nz == 0 || indices.len() < 3 {
        return RasterStats::default();
    }
    let cell = grid.cell;
    let (ox, oy, oz) = (grid.origin.x, grid.origin.y, grid.origin.z);

    // Flat list of (column index = j*nx + i, pierce height z, winding
    // dir = sign(n_z)). Sorted once, then swept per column —
    // cache-friendlier than a Vec-per-column.
    let mut crossings: Vec<(u32, f32, i8)> = Vec::new();

    for tri in indices.chunks_exact(3) {
        let ia = tri[0] as usize * 3;
        let ib = tri[1] as usize * 3;
        let ic = tri[2] as usize * 3;
        if ia + 2 >= vertices.len() || ib + 2 >= vertices.len() || ic + 2 >= vertices.len() {
            continue;
        }
        let a = Vec3::new(vertices[ia], vertices[ia + 1], vertices[ia + 2]);
        let b = Vec3::new(vertices[ib], vertices[ib + 1], vertices[ib + 2]);
        let c = Vec3::new(vertices[ic], vertices[ic + 1], vertices[ic + 2]);

        // Plane normal; skip ray-parallel (vertical) triangles.
        let n = (b - a).cross(c - a);
        if n.z.abs() < VERTICAL_EPS {
            continue;
        }
        let plane_d = n.dot(a); // n·x = plane_d on the triangle plane

        // Covered XY column-index bbox, clamped to the grid.
        let minx = a.x.min(b.x).min(c.x);
        let maxx = a.x.max(b.x).max(c.x);
        let miny = a.y.min(b.y).min(c.y);
        let maxy = a.y.max(b.y).max(c.y);
        let gx0 = (((minx - ox) / cell).floor() as isize).clamp(0, nx as isize - 1) as usize;
        let gx1 = (((maxx - ox) / cell).floor() as isize).clamp(0, nx as isize - 1) as usize;
        let gy0 = (((miny - oy) / cell).floor() as isize).clamp(0, ny as isize - 1) as usize;
        let gy1 = (((maxy - oy) / cell).floor() as isize).clamp(0, ny as isize - 1) as usize;

        for gy in gy0..=gy1 {
            let py = oy + (gy as f32 + 0.5) * cell;
            for gx in gx0..=gx1 {
                let px = ox + (gx as f32 + 0.5) * cell;
                if !point_in_triangle(Vec2::new(px, py), a.truncate(), b.truncate(), c.truncate())
                {
                    continue;
                }
                // Height where the (px, py, ·) ray meets the plane:
                //   n·(px, py, z) = plane_d  →  z = (plane_d - n.x*px - n.y*py)/n.z
                let z = (plane_d - n.x * px - n.y * py) / n.z;
                let dir: i8 = if n.z > 0.0 { 1 } else { -1 };
                crossings.push(((gy * nx + gx) as u32, z, dir));
            }
        }
    }

    if crossings.is_empty() {
        return RasterStats::default();
    }
    crossings.sort_unstable_by(|l, r| {
        l.0.cmp(&r.0).then(l.1.partial_cmp(&r.1).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut stats = RasterStats::default();
    let mut s = 0usize;
    while s < crossings.len() {
        let col = crossings[s].0;
        let mut e = s + 1;
        while e < crossings.len() && crossings[e].0 == col {
            e += 1;
        }
        let span = &crossings[s..e];
        stats.columns_hit += 1;
        let i = (col as usize) % nx;
        let j = (col as usize) / nx;

        // Winding sweep: accumulate sign(n_z) through the sorted
        // crossings; the interval AFTER crossing `p` (up to crossing
        // `p+1`) is inside the solid when the running winding is non-zero.
        let mut winding = 0i32;
        for p in 0..span.len() {
            winding += span[p].2 as i32;
            if winding == 0 || p + 1 >= span.len() {
                continue;
            }
            let z_lo = span[p].1;
            let z_hi = span[p + 1].1;
            // Voxel layers whose centre z ∈ [z_lo, z_hi].
            //   centre_z(k) = oz + (k+0.5)*cell  ⇒  k = (z - oz)/cell - 0.5
            let k0 = (((z_lo - oz) / cell - 0.5).ceil() as isize).clamp(0, nz as isize - 1);
            let k1 = (((z_hi - oz) / cell - 0.5).floor() as isize).clamp(0, nz as isize - 1);
            for k in k0..=k1 {
                if k < 0 {
                    continue;
                }
                let idx = (k as usize * ny + j) * nx + i;
                if let Some(v) = grid.occ.get_mut(idx) {
                    if *v == 0 {
                        *v = 1;
                        stats.voxels_filled += 1;
                    }
                }
            }
        }
        // A consistently-wound closed column nets to zero winding.
        if winding != 0 {
            stats.columns_unbalanced += 1;
        }
        s = e;
    }
    stats
}

/// Half-plane point-in-triangle test (inclusive on edges). Local copy of
/// the `qto` helper — kept private here so the voxel primitive has no
/// cross-module visibility coupling for the prototype; hoist to a shared
/// `geom` util when the reuse targets are consolidated (GH #63 note).
#[inline]
fn point_in_triangle(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let d1 = (p.x - b.x) * (a.y - b.y) - (a.x - b.x) * (p.y - b.y);
    let d2 = (p.x - c.x) * (b.y - c.y) - (b.x - c.x) * (p.y - c.y);
    let d3 = (p.x - a.x) * (c.y - a.y) - (c.x - a.x) * (p.y - a.y);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Outward-facing winding (not that even-odd needs it).
        let idx: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, // bottom (-Z)
            4, 5, 6, 4, 6, 7, // top (+Z)
            0, 1, 5, 0, 5, 4, // -Y
            1, 2, 6, 1, 6, 5, // +X
            2, 3, 7, 2, 7, 6, // +Y
            3, 0, 4, 3, 4, 7, // -X
        ];
        (verts, idx)
    }

    #[test]
    fn unit_cube_fills_full_grid() {
        // 1×1×1 m cube, 0.1 m voxels → 10×10×10 = 1000 voxels, ~1 m³.
        let (v, i) = box_mesh(Vec3::ZERO, Vec3::ONE);
        let mut grid = VoxelGrid::from_bounds(Vec3::ZERO, Vec3::ONE, 0.1).unwrap();
        let stats = rasterize_solid_3d(&mut grid, &v, &i);
        assert_eq!(stats.columns_unbalanced, 0, "closed cube → balanced winding");
        assert_eq!(grid.count_occupied(), 1000, "10×10×10 interior voxels");
        assert!(
            (grid.occupied_volume_m3() - 1.0).abs() < 1e-4,
            "voxel volume ≈ 1 m³, got {}",
            grid.occupied_volume_m3()
        );
    }

    #[test]
    fn box_volume_scales() {
        // 2×1×0.5 m box = 1.0 m³, 0.1 m voxels.
        let max = Vec3::new(2.0, 1.0, 0.5);
        let (v, i) = box_mesh(Vec3::ZERO, max);
        let mut grid = VoxelGrid::from_bounds(Vec3::ZERO, max, 0.1).unwrap();
        rasterize_solid_3d(&mut grid, &v, &i);
        // 20×10×5 = 1000 voxels.
        assert_eq!(grid.count_occupied(), 1000);
        assert!((grid.occupied_volume_m3() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn two_disjoint_boxes_accumulate() {
        // Two 1 m cubes separated by a 1 m gap along X, in one grid.
        let lo = Vec3::ZERO;
        let hi = Vec3::new(3.0, 1.0, 1.0);
        let mut grid = VoxelGrid::from_bounds(lo, hi, 0.1).unwrap();
        let (v0, i0) = box_mesh(Vec3::ZERO, Vec3::ONE);
        let (v1, i1) = box_mesh(Vec3::new(2.0, 0.0, 0.0), Vec3::new(3.0, 1.0, 1.0));
        rasterize_solid_3d(&mut grid, &v0, &i0);
        rasterize_solid_3d(&mut grid, &v1, &i1);
        // Two cubes filled, the 1 m gap (10 voxels of X) left free.
        assert_eq!(grid.count_occupied(), 2000);
        // Spot-check a voxel in the gap is free (x≈1.5 m → i=15).
        assert!(!grid.is_occupied(15, 5, 5), "gap column must be free");
    }

    #[test]
    fn hollow_column_z_parity_fills_only_solid() {
        // A box from z=0.3..0.7 sitting in a taller grid: only the middle
        // layers fill, proving the Z-interval parity (not a full column).
        let mut grid =
            VoxelGrid::from_bounds(Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0), 0.1).unwrap();
        let (v, i) = box_mesh(Vec3::new(0.0, 0.0, 0.3), Vec3::new(1.0, 1.0, 0.7));
        rasterize_solid_3d(&mut grid, &v, &i);
        // 10×10 columns × 4 layers (z centres 0.35,0.45,0.55,0.65) = 400.
        assert_eq!(grid.count_occupied(), 400);
        assert!(grid.is_occupied(5, 5, 5), "z≈0.55 inside solid");
        assert!(!grid.is_occupied(5, 5, 0), "z≈0.05 below solid");
        assert!(!grid.is_occupied(5, 5, 9), "z≈0.95 above solid");
    }

    #[test]
    fn degenerate_inputs_are_safe() {
        assert!(VoxelGrid::from_bounds(Vec3::ZERO, Vec3::ONE, 0.0).is_none());
        assert!(VoxelGrid::from_bounds(Vec3::ZERO, Vec3::ONE, -1.0).is_none());
        let mut grid = VoxelGrid::from_bounds(Vec3::ZERO, Vec3::ONE, 0.1).unwrap();
        let s = rasterize_solid_3d(&mut grid, &[], &[]);
        assert_eq!(s, RasterStats::default());
        assert_eq!(grid.count_occupied(), 0);
    }
}
