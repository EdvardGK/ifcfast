//! Per-product geometric QTO — one O(triangles) pass over the
//! world-coord mesh that emits everything a quantity take-off needs:
//!
//!   * total volume (m³) via the signed-tetrahedra divergence theorem
//!   * total surface area (m²)
//!   * area subdivided by face orientation: `top` (normal ≈ +Z), `bottom`
//!     (≈ -Z), `side` (normal in the horizontal plane), `inclined` (the
//!     rest — sloped roofs, ramps, chamfers)
//!   * the list of distinct planar surfaces with `(area, normal_xyz)`,
//!     sorted by area, so a SQL UNNEST surfaces "biggest surface on
//!     type X" in milliseconds
//!
//! The "distinct planar surface" aggregation is normal-bucket based:
//! triangles whose face normal rounds to the same 0.1-precision unit
//! vector (~5.7° angular granularity) collapse into one surface. This
//! is exact for box-like building elements (walls, slabs, doors,
//! windows, beams) and bucketed-coarsely for curved geometry (pipes,
//! cylinders) — a 12-segment cylinder side reads as 12 wedge surfaces,
//! which is the right model for QTO ("the largest planar approximation
//! of the surface"), not a single "fake" curved surface that doesn't
//! exist in the tessellation.
//!
//! Why world-coord and not local-coord: orientation buckets (top /
//! bottom / side / inclined) are properties of the placed product, not
//! the authoring frame. A wall tilted 30° from the architect's
//! intended vertical reads as "inclined", which is the QTO truth.
//! [`crate::mesh::ProductMesh::vertices`] is already world-baked so we
//! consume it directly.

use std::collections::HashMap;

/// One distinct planar surface on a product. The normal is the
/// quantized direction (rounded to 0.1 per component, ~5.7° bin) so
/// triangles that are coplanar within tolerance aggregate into one
/// row. `area_m2` is the *summed* triangle area in this bucket.
#[derive(Debug, Clone, Copy)]
pub struct PlanarSurface {
    pub area_m2: f32,
    pub nx: f32,
    pub ny: f32,
    pub nz: f32,
}

/// All the QTO numbers a `ProductMesh` resolves to. Designed as a flat
/// struct so the bundle sink can hand each field straight to the
/// parquet builders without re-walking anything.
#[derive(Debug, Clone, Default)]
pub struct MeshQto {
    /// Signed volume in m³ (divergence theorem). Consumers typically
    /// take `.abs()` — sign indicates winding for closed meshes,
    /// undefined for open shells.
    pub volume_m3: f32,
    /// Axis-aligned bounding-box volume in m³. Always ≥ |volume_m3|
    /// for closed meshes; the ratio is a "compactness" proxy
    /// (1.0 = box, ~0.5 = cylinder, small = thin / sparse).
    pub aabb_volume_m3: f32,
    /// Total triangle-area sum in m².
    pub surface_area_m2: f32,
    /// Sum of triangle areas whose face normal points within 20° of
    /// +Z (world up). For floors / slabs this is the walkable top.
    pub area_top_m2: f32,
    /// Same with normal within 20° of -Z (world down). Soffits,
    /// ceiling bottoms.
    pub area_bottom_m2: f32,
    /// Triangles whose normal is within 20° of the horizontal plane —
    /// the "vertical" faces (wall sides, door faces, window panes).
    pub area_side_m2: f32,
    /// Everything else (sloped surfaces). For a pyramid roof this is
    /// almost the whole surface; for a vertical wall it's near zero.
    pub area_inclined_m2: f32,
    /// Largest single planar surface in m² (the biggest entry in
    /// `surfaces`, lifted to the scalar so the common "biggest face"
    /// query needs no UNNEST).
    pub largest_surface_m2: f32,
    /// Smallest planar surface above the noise floor (1e-6 m² ≈
    /// 1 mm²). 0 if every surface is sub-noise — only possible on
    /// degenerate meshes.
    pub smallest_surface_m2: f32,
    /// Number of distinct planar surfaces (the length of `surfaces`).
    pub surface_count: u32,
    /// Every distinct planar surface, sorted by area descending.
    /// Stored as a Parquet list so DuckDB UNNEST gives one row per
    /// face for "show me every surface on this product" queries.
    pub surfaces: Vec<PlanarSurface>,
}

// 20° threshold against ±Z: `cos(20°) ≈ 0.940`.
// "Side" = normal within 20° of horizontal plane: `|nz| <= sin(20°) ≈ 0.342`.
const COS_TOP_THRESHOLD: f32 = 0.940;
const SIN_SIDE_THRESHOLD: f32 = 0.342;

// Normal quantization grid: 0.1 per component. Coarse enough to merge
// floating-point noise on flat surfaces (Revit emits ±1e-5 jitter on
// nominally-coplanar walls) but fine enough that a pipe's 12-segment
// tessellation reads as 12 distinct wedges, not one big amorphous
// surface. Encode the quantized normal as a packed i32 key for a
// cheap hash on triples of small ints.
const NORMAL_QUANT_SCALE: f32 = 10.0;

#[inline]
fn quantize_normal(nx: f32, ny: f32, nz: f32) -> (i32, i32, i32) {
    (
        (nx * NORMAL_QUANT_SCALE).round() as i32,
        (ny * NORMAL_QUANT_SCALE).round() as i32,
        (nz * NORMAL_QUANT_SCALE).round() as i32,
    )
}

/// Compute QTO for one product. `unit_scale` is the IFC project's
/// linear-unit-to-metres factor (typically 0.001 for Revit/Tekla
/// millimetre files, 1.0 for files already authored in metres). The
/// function multiplies area by `unit_scale²` and volume by
/// `unit_scale³` so the output is *always* in m² / m³ regardless of
/// the source file's unit choice.
pub fn compute(vertices: &[f32], indices: &[u32], unit_scale: f32) -> MeshQto {
    // Early-out for degenerate inputs — emits a zero record so the
    // downstream parquet row still gets written (reveal-all: even an
    // unmeshed product carries identity + zeroed QTO, never a NULL we
    // can't distinguish from "missing").
    if indices.len() < 3 || vertices.len() < 9 {
        return MeshQto::default();
    }

    let area_scale = unit_scale * unit_scale; // m² scaling
    let volume_scale = unit_scale * unit_scale * unit_scale; // m³ scaling

    let mut surface_area_raw: f32 = 0.0;
    let mut volume_x6_raw: f32 = 0.0;
    let mut area_top_raw: f32 = 0.0;
    let mut area_bottom_raw: f32 = 0.0;
    let mut area_side_raw: f32 = 0.0;
    let mut area_inclined_raw: f32 = 0.0;

    // AABB tracked inline so the volume_box stays free of a second pass.
    let mut xmin = f32::INFINITY;
    let mut ymin = f32::INFINITY;
    let mut zmin = f32::INFINITY;
    let mut xmax = f32::NEG_INFINITY;
    let mut ymax = f32::NEG_INFINITY;
    let mut zmax = f32::NEG_INFINITY;

    // Planar-surface aggregation. Small linear-probe map keyed by the
    // 3-int quantized normal — typical building elements have 6-20
    // distinct surfaces, so a flat Vec beats a HashMap on cache
    // behavior. Falls back to a HashMap once the Vec grows past a
    // threshold (curved geometry with many tessellation wedges).
    let mut small_buckets: Vec<((i32, i32, i32), f32)> = Vec::with_capacity(16);
    let mut overflow_buckets: HashMap<(i32, i32, i32), f32> = HashMap::new();

    for tri in indices.chunks_exact(3) {
        let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        // Bounds check on the way in — the mesher writes these
        // ourselves but a corrupt iter would explode silently if we
        // skipped this.
        let off_a = a * 3;
        let off_b = b * 3;
        let off_c = c * 3;
        if off_c + 2 >= vertices.len() {
            continue;
        }

        let ax = vertices[off_a];
        let ay = vertices[off_a + 1];
        let az = vertices[off_a + 2];
        let bx = vertices[off_b];
        let by = vertices[off_b + 1];
        let bz = vertices[off_b + 2];
        let cx = vertices[off_c];
        let cy = vertices[off_c + 1];
        let cz = vertices[off_c + 2];

        // AABB update — cheaper to do here than to rewalk later.
        if ax < xmin { xmin = ax; } if ax > xmax { xmax = ax; }
        if ay < ymin { ymin = ay; } if ay > ymax { ymax = ay; }
        if az < zmin { zmin = az; } if az > zmax { zmax = az; }
        if bx < xmin { xmin = bx; } if bx > xmax { xmax = bx; }
        if by < ymin { ymin = by; } if by > ymax { ymax = by; }
        if bz < zmin { zmin = bz; } if bz > zmax { zmax = bz; }
        if cx < xmin { xmin = cx; } if cx > xmax { xmax = cx; }
        if cy < ymin { ymin = cy; } if cy > ymax { ymax = cy; }
        if cz < zmin { zmin = cz; } if cz > zmax { zmax = cz; }

        // Triangle area + normal direction via the cross product.
        let ux = bx - ax; let uy = by - ay; let uz = bz - az;
        let vx = cx - ax; let vy = cy - ay; let vz = cz - az;
        let nrx = uy * vz - uz * vy;
        let nry = uz * vx - ux * vz;
        let nrz = ux * vy - uy * vx;
        let mag = (nrx * nrx + nry * nry + nrz * nrz).sqrt();
        if mag < 1e-12 {
            continue; // degenerate triangle, skip silently
        }
        let area_raw = 0.5 * mag;
        surface_area_raw += area_raw;

        // Signed-tetrahedra divergence accumulator.
        volume_x6_raw += ax * (by * cz - bz * cy)
                      + ay * (bz * cx - bx * cz)
                      + az * (bx * cy - by * cx);

        // Unit normal — used for orientation bucket + quantized key.
        let inv = 1.0 / mag;
        let nx = nrx * inv;
        let ny = nry * inv;
        let nz = nrz * inv;

        // Orientation bucket. Disjoint thresholds: a triangle either
        // is "top" (within 20° of +Z), "bottom" (within 20° of -Z),
        // "side" (within 20° of horizontal plane), or "inclined".
        if nz > COS_TOP_THRESHOLD {
            area_top_raw += area_raw;
        } else if nz < -COS_TOP_THRESHOLD {
            area_bottom_raw += area_raw;
        } else if nz.abs() < SIN_SIDE_THRESHOLD {
            area_side_raw += area_raw;
        } else {
            area_inclined_raw += area_raw;
        }

        // Planar-surface bucket — quantize normal direction, accumulate area.
        let key = quantize_normal(nx, ny, nz);
        let mut placed = false;
        for entry in small_buckets.iter_mut() {
            if entry.0 == key {
                entry.1 += area_raw;
                placed = true;
                break;
            }
        }
        if !placed {
            if small_buckets.len() < 64 {
                small_buckets.push((key, area_raw));
            } else {
                // Migrate to HashMap once the linear scan gets expensive.
                if overflow_buckets.is_empty() {
                    for (k, v) in small_buckets.drain(..) {
                        overflow_buckets.insert(k, v);
                    }
                }
                *overflow_buckets.entry(key).or_insert(0.0) += area_raw;
            }
        }
    }

    let volume_raw = volume_x6_raw / 6.0;
    let aabb_volume_raw = if xmin.is_finite() {
        (xmax - xmin).max(0.0) * (ymax - ymin).max(0.0) * (zmax - zmin).max(0.0)
    } else {
        0.0
    };

    // Assemble the per-surface list (largest first). Either source —
    // small_buckets or overflow_buckets — depending on which we used.
    let mut surfaces: Vec<PlanarSurface> = if !overflow_buckets.is_empty() {
        overflow_buckets
            .into_iter()
            .map(|(k, v)| PlanarSurface {
                area_m2: v * area_scale,
                nx: k.0 as f32 / NORMAL_QUANT_SCALE,
                ny: k.1 as f32 / NORMAL_QUANT_SCALE,
                nz: k.2 as f32 / NORMAL_QUANT_SCALE,
            })
            .collect()
    } else {
        small_buckets
            .into_iter()
            .map(|(k, v)| PlanarSurface {
                area_m2: v * area_scale,
                nx: k.0 as f32 / NORMAL_QUANT_SCALE,
                ny: k.1 as f32 / NORMAL_QUANT_SCALE,
                nz: k.2 as f32 / NORMAL_QUANT_SCALE,
            })
            .collect()
    };
    surfaces.sort_by(|a, b| b.area_m2.partial_cmp(&a.area_m2).unwrap_or(std::cmp::Ordering::Equal));

    // Noise floor for "smallest surface": 1 mm² = 1e-6 m². Smaller
    // entries get dropped from the scalar so the value isn't a stray
    // sub-pixel artifact.
    const NOISE_FLOOR_M2: f32 = 1e-6;
    let largest = surfaces.first().map(|s| s.area_m2).unwrap_or(0.0);
    let smallest = surfaces
        .iter()
        .map(|s| s.area_m2)
        .filter(|a| *a > NOISE_FLOOR_M2)
        .fold(f32::INFINITY, f32::min);
    let smallest = if smallest.is_finite() { smallest } else { 0.0 };
    let surface_count = surfaces.len() as u32;

    MeshQto {
        volume_m3: volume_raw * volume_scale,
        aabb_volume_m3: aabb_volume_raw * volume_scale,
        surface_area_m2: surface_area_raw * area_scale,
        area_top_m2: area_top_raw * area_scale,
        area_bottom_m2: area_bottom_raw * area_scale,
        area_side_m2: area_side_raw * area_scale,
        area_inclined_m2: area_inclined_raw * area_scale,
        largest_surface_m2: largest,
        smallest_surface_m2: smallest,
        surface_count,
        surfaces,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1m unit cube centered at origin, axis-aligned. Six faces, each
    // 1 m². Total surface area 6 m², volume 1 m³.
    fn unit_cube_world() -> (Vec<f32>, Vec<u32>) {
        let v = vec![
            -0.5, -0.5, -0.5,
             0.5, -0.5, -0.5,
             0.5,  0.5, -0.5,
            -0.5,  0.5, -0.5,
            -0.5, -0.5,  0.5,
             0.5, -0.5,  0.5,
             0.5,  0.5,  0.5,
            -0.5,  0.5,  0.5,
        ];
        // 12 triangles, two per face, wound CCW from outside.
        let i = vec![
            // bottom (z = -0.5, normal = -Z)
            0, 2, 1,  0, 3, 2,
            // top (z = +0.5, normal = +Z)
            4, 5, 6,  4, 6, 7,
            // -Y face
            0, 1, 5,  0, 5, 4,
            // +Y face
            3, 7, 6,  3, 6, 2,
            // -X face
            0, 4, 7,  0, 7, 3,
            // +X face
            1, 2, 6,  1, 6, 5,
        ];
        (v, i)
    }

    #[test]
    fn unit_cube_volume_and_area() {
        let (v, i) = unit_cube_world();
        let q = compute(&v, &i, 1.0);
        assert!((q.volume_m3.abs() - 1.0).abs() < 1e-5, "got {}", q.volume_m3);
        assert!((q.surface_area_m2 - 6.0).abs() < 1e-5, "got {}", q.surface_area_m2);
        assert!((q.aabb_volume_m3 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn unit_cube_orientation_buckets() {
        let (v, i) = unit_cube_world();
        let q = compute(&v, &i, 1.0);
        // Each face is 1 m². Top + bottom each have one face;
        // sides have 4 faces of 1 m² = 4 m². Inclined is empty.
        assert!((q.area_top_m2 - 1.0).abs() < 1e-5);
        assert!((q.area_bottom_m2 - 1.0).abs() < 1e-5);
        assert!((q.area_side_m2 - 4.0).abs() < 1e-5);
        assert!(q.area_inclined_m2.abs() < 1e-5);
    }

    #[test]
    fn unit_cube_six_planar_surfaces() {
        let (v, i) = unit_cube_world();
        let q = compute(&v, &i, 1.0);
        // Six distinct face normals → six planar surfaces, each 1 m².
        assert_eq!(q.surface_count, 6);
        assert!((q.largest_surface_m2 - 1.0).abs() < 1e-5);
        assert!((q.smallest_surface_m2 - 1.0).abs() < 1e-5);
        for s in &q.surfaces {
            assert!((s.area_m2 - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn unit_cube_unit_scale_mm_to_m() {
        // Same cube but expressed in mm: 1000 mm side, file authored
        // in mm so unit_scale = 0.001. Output must still be 1 m³.
        let (mut v, i) = unit_cube_world();
        for x in v.iter_mut() { *x *= 1000.0; }
        let q = compute(&v, &i, 0.001);
        assert!((q.volume_m3.abs() - 1.0).abs() < 1e-4, "got {}", q.volume_m3);
        assert!((q.surface_area_m2 - 6.0).abs() < 1e-4, "got {}", q.surface_area_m2);
    }

    #[test]
    fn degenerate_inputs_zero() {
        let q = compute(&[], &[], 1.0);
        assert_eq!(q.volume_m3, 0.0);
        assert_eq!(q.surface_count, 0);
    }
}
