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
    /// Validity classifier for `volume_m3`:
    ///
    /// - `"closed"`:    `|volume_m3| <= aabb_volume_m3` (with a small
    ///                  numerical tolerance). The divergence-theorem
    ///                  computation produced a physically possible
    ///                  value; consumers can trust it for sum queries.
    /// - `"open_shell"`: `|volume_m3| > aabb_volume_m3`. The mesh is
    ///                  not a closed manifold (open boundary, hole,
    ///                  inverted normal). Divergence-theorem volumes
    ///                  are mathematically undefined on open shells;
    ///                  consumers should fall back to `aabb_volume_m3`
    ///                  or filter the row out of volume sums.
    /// - `"degenerate"`: `aabb_volume_m3 <= 0`. No real geometry —
    ///                  empty mesh, 2D annotation, or a product whose
    ///                  representation collapsed to a line / point.
    ///                  Every quantity column is suspect.
    ///
    /// 9.4% of products on the audited Duplex file land in
    /// `"open_shell"`; without this flag any downstream `SUM(volume_m3)`
    /// silently sums garbage with valid figures.
    pub mesh_quality: &'static str,
    /// Routing flag: `true` when `volume_best_m3` is the mesh-measured
    /// volume and is trustworthy — either a closed manifold, or a
    /// non-manifold whose volume is still within its tight prism bound
    /// (the edge-pairing classifier over-flags dedup-imperfect meshes
    /// whose volumes are nonetheless accurate). `false` means the mesh
    /// volume exceeded its prism bound (provably too big) or the rep is
    /// degenerate, so `volume_best_m3` is the prism fallback / 0 instead.
    /// Pipelines escalate the `false` rows to an authoritative kernel and
    /// keep ifcfast's speed on the rest (~2-3 % flagged on a real model).
    pub volume_reliable: bool,
    /// Which definition `volume_best_m3` carries (always agrees with
    /// `volume_reliable`):
    /// - `"mesh"`: the signed-tetra mesh volume (reliable rows).
    /// - `"prism_fallback"`: `footprint × z_extent` — substituted when
    ///   the mesh volume is provably too big or the rep is degenerate.
    ///   A tighter bound than the AABB; reproduces the QTO-convention
    ///   prism value tools like Solibri report for open slabs.
    pub volume_method: &'static str,
    /// The best single volume estimate: the mesh volume when reliable,
    /// else the prism fallback (`volume_prism_bound_m3`). This is what
    /// the `volume_m3` substrate column / `mesh_qto()` output carries —
    /// `SUM(volume_m3)` no longer mixes open-shell garbage into totals.
    pub volume_best_m3: f32,
    /// Tight prism upper-bound on volume, `footprint × z_extent` in m³,
    /// where `footprint` is the raster-estimated union of the mesh's
    /// triangles projected onto the XY plane. Computed for every
    /// non-closed row (it is both the tripwire and the fallback value);
    /// `f32::NAN` on closed rows, where it is neither needed nor computed
    /// (keeps the watertight hot path raster-free). v1 uses the Z-axis
    /// prism only (tight for slabs); a min over the three axis
    /// projections (tight for beams/columns) is a documented follow-up.
    pub volume_prism_bound_m3: f32,
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
        return MeshQto {
            mesh_quality: "degenerate",
            volume_reliable: false,
            volume_method: "prism_fallback",
            volume_best_m3: 0.0,
            volume_prism_bound_m3: 0.0,
            ..MeshQto::default()
        };
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
        // Once we've migrated to overflow_buckets, it is the only source of
        // truth — small_buckets stays empty thereafter.
        let key = quantize_normal(nx, ny, nz);
        if !overflow_buckets.is_empty() {
            *overflow_buckets.entry(key).or_insert(0.0) += area_raw;
        } else {
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
                    for (k, v) in small_buckets.drain(..) {
                        overflow_buckets.insert(k, v);
                    }
                    *overflow_buckets.entry(key).or_insert(0.0) += area_raw;
                }
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

    let volume_m3 = volume_raw * volume_scale;
    let aabb_volume_m3 = aabb_volume_raw * volume_scale;

    // Mesh validity classifier — see field docs on `MeshQto.mesh_quality`.
    //
    // Two-tier classifier:
    //   1. Cheap upper-bound check — `|volume| > aabb * 1.001` is
    //      mathematically impossible for a closed manifold and catches
    //      ~29% of Duplex products (windows, cabinets, IfcSpaces). The
    //      1.001 multiplier absorbs ~0.1% f32 noise on the divergence
    //      sum.
    //   2. Edge-pairing manifold check — closed iff every undirected
    //      edge is shared by exactly 2 triangles with opposite wind.
    //      Catches the cases the cheap check misses: open shells whose
    //      divergence-theorem volume happens to land inside the AABB
    //      (e.g. a cube at origin with one face removed gives ~5/6 the
    //      cube volume — wrong but bounded).
    let mesh_quality: &'static str = if aabb_volume_m3 <= 0.0 {
        "degenerate"
    } else if volume_m3.abs() > aabb_volume_m3 * 1.001 {
        "open_shell"
    } else if !is_closed_manifold(indices) {
        "open_shell"
    } else {
        "closed"
    };

    // Volume-reliability + prism fallback (GH #60). A closed manifold's
    // signed-tetra volume is trustworthy as-is. For anything else we
    // compute the tight prism upper bound (union-XY footprint × z-extent)
    // and use it two ways:
    //   * as a *tripwire* — a mesh volume that exceeds its own prism
    //     bound is provably too big (open shell / inverted winding), so
    //     it gets replaced by the prism estimate (matches the QTO-prism
    //     value tools like Solibri report for open slabs);
    //   * but an open shell whose volume is still *within* the prism
    //     bound keeps its mesh value. The edge-pairing classifier
    //     over-flags watertight-enough meshes (coincident-vertex
    //     dedup false negatives), and on real models those mesh volumes
    //     are accurate to ~0.2 % — replacing them with the looser prism
    //     would regress them (validated on G55: 0 regressions, 18 fixes).
    // The raster runs only on the non-closed minority, so the closed hot
    // path is untouched.
    let volume_mesh_m3 = volume_m3.abs();
    // Raster footprint error can slightly under-count, pulling the prism
    // bound a hair below a correct mesh volume; this margin keeps such
    // rows from being falsely tripped. Real violations exceed the bound
    // by integer multiples — far outside the margin (G55: tripwire fires
    // identically for any margin from 1.02 to 1.20).
    const PRISM_TRIPWIRE_MARGIN: f32 = 1.05;
    let (volume_best_m3, volume_method, volume_reliable, volume_prism_bound_m3) =
        if mesh_quality == "closed" {
            // Footprint deliberately NOT computed — NaN signals "not
            // applicable / not computed" so consumers don't read it as 0.
            (volume_mesh_m3, "mesh", true, f32::NAN)
        } else {
            let z_extent_raw = if zmin.is_finite() {
                (zmax - zmin).max(0.0)
            } else {
                0.0
            };
            // Vertical / planar meshes have zero z-extent → the z-prism
            // is zero regardless of footprint, so skip the raster.
            let prism = if z_extent_raw > 0.0 && xmax > xmin && ymax > ymin {
                let footprint_raw = footprint_xy_raw(vertices, indices, xmin, xmax, ymin, ymax);
                footprint_raw * z_extent_raw * volume_scale
            } else {
                0.0
            };
            if prism > 0.0 && volume_mesh_m3 <= prism * PRISM_TRIPWIRE_MARGIN {
                // Mesh volume sits within its tight upper bound — trust it
                // even though the shell isn't a perfect manifold.
                (volume_mesh_m3, "mesh", true, prism)
            } else {
                // Provably too big (or degenerate / no usable footprint)
                // → fall back to the prism estimate.
                (prism, "prism_fallback", false, prism)
            }
        };

    MeshQto {
        volume_m3,
        aabb_volume_m3,
        surface_area_m2: surface_area_raw * area_scale,
        area_top_m2: area_top_raw * area_scale,
        area_bottom_m2: area_bottom_raw * area_scale,
        area_side_m2: area_side_raw * area_scale,
        area_inclined_m2: area_inclined_raw * area_scale,
        largest_surface_m2: largest,
        smallest_surface_m2: smallest,
        surface_count,
        mesh_quality,
        volume_reliable,
        volume_method,
        volume_best_m3,
        volume_prism_bound_m3,
        surfaces,
    }
}

/// Union of the mesh's triangles projected onto the XY plane, in raw
/// (pre-`unit_scale`) units², estimated by rasterizing onto a fixed-cell
/// grid that spans the XY bounding box. Overlapping triangles re-mark the
/// same cells, so the result is the true *union* footprint, not the sum
/// of per-triangle areas. Vertical faces project to a line (zero 2D area)
/// and contribute nothing, which is exactly right — only the horizontal
/// extent makes up a footprint.
///
/// Cost is `O(triangles + covered_cells)` and this is called ONLY on the
/// flagged minority (`!volume_reliable`, ~0.3 % of products), so it never
/// touches the reliable hot path. The grid is square-celled (no axis
/// bias) and capped at `MAX_CELLS` per side, giving ~0.2 % relative area
/// error on a slab — fine for a fallback/tripwire value.
///
/// This is an *estimate*: a coarse grid can slip thin triangles between
/// cell centres (slight under-count). An exact 2D boolean union (via
/// `i_overlay`) is the documented accuracy upgrade if telemetry needs it.
fn footprint_xy_raw(
    vertices: &[f32],
    indices: &[u32],
    xmin: f32,
    xmax: f32,
    ymin: f32,
    ymax: f32,
) -> f32 {
    // 256 cells/side ≈ 0.4 % per-axis quantization (well under 1 % on
    // footprint area) while keeping the per-triangle cell-bbox scan cheap
    // enough to run on every non-closed row. On G55 this reproduced the
    // Solibri QTO-prism value to within 0.01 m³.
    const MAX_CELLS: usize = 256;
    let w = (xmax - xmin).max(0.0);
    let h = (ymax - ymin).max(0.0);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let cell = w.max(h) / MAX_CELLS as f32;
    if cell <= 0.0 {
        return 0.0;
    }
    let nx = ((w / cell).ceil() as usize).clamp(1, MAX_CELLS);
    let ny = ((h / cell).ceil() as usize).clamp(1, MAX_CELLS);
    let cell_w = w / nx as f32;
    let cell_h = h / ny as f32;
    let mut covered = vec![false; nx * ny];

    for tri in indices.chunks_exact(3) {
        let ia = tri[0] as usize * 3;
        let ib = tri[1] as usize * 3;
        let ic = tri[2] as usize * 3;
        if ia + 1 >= vertices.len() || ib + 1 >= vertices.len() || ic + 1 >= vertices.len() {
            continue;
        }
        let ax = vertices[ia];
        let ay = vertices[ia + 1];
        let bx = vertices[ib];
        let by = vertices[ib + 1];
        let cx = vertices[ic];
        let cy = vertices[ic + 1];

        // Signed 2D area — skip near-zero (vertical faces project to a
        // line and add no footprint).
        let twice_area = (bx - ax) * (cy - ay) - (cx - ax) * (by - ay);
        if twice_area.abs() < 1e-12 {
            continue;
        }

        // Triangle's cell-index bbox, clamped into the grid.
        let gx0 = (((ax.min(bx).min(cx) - xmin) / cell_w).floor() as isize)
            .clamp(0, nx as isize - 1) as usize;
        let gx1 = (((ax.max(bx).max(cx) - xmin) / cell_w).floor() as isize)
            .clamp(0, nx as isize - 1) as usize;
        let gy0 = (((ay.min(by).min(cy) - ymin) / cell_h).floor() as isize)
            .clamp(0, ny as isize - 1) as usize;
        let gy1 = (((ay.max(by).max(cy) - ymin) / cell_h).floor() as isize)
            .clamp(0, ny as isize - 1) as usize;

        for gy in gy0..=gy1 {
            let py = ymin + (gy as f32 + 0.5) * cell_h;
            let row = gy * nx;
            for gx in gx0..=gx1 {
                let px = xmin + (gx as f32 + 0.5) * cell_w;
                if point_in_triangle(px, py, ax, ay, bx, by, cx, cy) {
                    covered[row + gx] = true;
                }
            }
        }
    }

    let count = covered.iter().filter(|&&c| c).count();
    count as f32 * cell_w * cell_h
}

/// 2D point-in-triangle via the three edge half-plane signs. Winding-
/// agnostic (a point inside has all three signs agree); edge-inclusive,
/// which only matters at shared edges where the cell is marked once
/// anyway.
#[inline]
fn point_in_triangle(
    px: f32,
    py: f32,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    cx: f32,
    cy: f32,
) -> bool {
    let d1 = (px - bx) * (ay - by) - (ax - bx) * (py - by);
    let d2 = (px - cx) * (by - cy) - (bx - cx) * (py - cy);
    let d3 = (px - ax) * (cy - ay) - (cx - ax) * (py - ay);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// Closed-manifold check via directed-edge pairing.
///
/// A mesh is a closed manifold iff every undirected edge is shared by
/// exactly two triangles whose winding agrees that the edge is
/// traversed in opposite directions. Equivalently:
///
/// - Each undirected edge `{u, v}` appears in exactly 2 triangles
///   (`unsigned_count == 2`).
/// - The two triangles contribute opposite directed edges along it:
///   one gives `(u, v)`, the other gives `(v, u)`. Summing `+1` for
///   `u < v` direction and `-1` otherwise gives `signed_count == 0`.
///
/// Open boundaries (only 1 triangle on an edge), T-junctions
/// (3+ triangles), and consistently-inverted normals (2 triangles
/// winding the same way) all violate one or the other condition and
/// classify the mesh as non-manifold.
///
/// Vertex deduplication caveat: this uses the raw vertex indices the
/// mesher emitted. If two geometrically-coincident vertices live at
/// different indices (no dedup on the mesher's side), the check will
/// report "open" even on a visually-closed mesh. The reverse case —
/// false-positive "closed" on a truly open mesh — is impossible by
/// construction. So this is a conservative classifier: false negatives
/// (extra "open_shell" labels) are possible; false positives are not.
pub(crate) fn is_closed_manifold(indices: &[u32]) -> bool {
    if indices.len() < 9 || indices.len() % 3 != 0 {
        return false;
    }
    // Capacity hint: ~3 directed edges per triangle, but with the
    // undirected-key merge that's roughly 1.5 unique entries per
    // triangle on a closed manifold (Euler: V - E + F = 2 → E ≈ 1.5F
    // for a triangulated 2-manifold).
    let mut edges: std::collections::HashMap<(u32, u32), (u32, i32)> =
        std::collections::HashMap::with_capacity(indices.len() / 2);
    for tri in indices.chunks_exact(3) {
        let a = tri[0];
        let b = tri[1];
        let c = tri[2];
        for &(u, v) in &[(a, b), (b, c), (c, a)] {
            if u == v {
                // Degenerate triangle (edge of zero length). Same
                // treatment as `mesh::qto::compute` — silently skip;
                // a single degenerate edge shouldn't condemn the
                // whole mesh.
                continue;
            }
            let (key, sign) = if u < v { ((u, v), 1) } else { ((v, u), -1) };
            let entry = edges.entry(key).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += sign;
        }
    }
    edges.values().all(|&(unsigned, signed)| unsigned == 2 && signed == 0)
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

    /// 70 triangles, each with a unique face normal (third vertex rotated
    /// slightly in the XY plane). Each triangle has the same area so the
    /// bucket totals are all equal. Tests that surface_count is correct
    /// and that per-surface areas sum to total surface_area_m2.
    ///
    /// If the overflow-bucket post-drain bug is present, keys pushed back
    /// into small_buckets after the drain are silently dropped from the
    /// assembled surface list, so surface_count will be < 70 and the
    /// area sums will not match.
    #[test]
    fn many_unique_normals_overflow_buckets() {
        // Build triangles whose face normals are spread across a 3D grid so
        // that the quantizer (NORMAL_QUANT_SCALE=10) yields >64 distinct
        // bucket keys. Iterate over a coarse (nx, ny, nz) lattice on the
        // unit sphere; for each, synthesize a triangle whose normal equals
        // that direction. Keep going until we have collected enough unique
        // quantized keys to force overflow_buckets migration.
        let mut directions: Vec<(f32, f32, f32)> = Vec::new();
        let mut seen: std::collections::HashSet<(i32, i32, i32)> =
            std::collections::HashSet::new();
        // 0.15 step on each axis gives a grid coarse enough that every cell
        // quantizes distinctly under the 0.1 scale, but fine enough to fill
        // a hemisphere with >64 entries.
        let step: f32 = 0.15;
        let mut x = -1.0_f32;
        while x <= 1.0 {
            let mut y = -1.0_f32;
            while y <= 1.0 {
                let r2 = x * x + y * y;
                if r2 <= 0.95 {
                    let z = (1.0 - r2).sqrt();
                    let inv = 1.0 / (x * x + y * y + z * z).sqrt();
                    let n = (x * inv, y * inv, z * inv);
                    let key = (
                        (n.0 * NORMAL_QUANT_SCALE).round() as i32,
                        (n.1 * NORMAL_QUANT_SCALE).round() as i32,
                        (n.2 * NORMAL_QUANT_SCALE).round() as i32,
                    );
                    if seen.insert(key) {
                        directions.push(n);
                    }
                }
                y += step;
            }
            x += step;
        }
        // We need strictly more than 64 distinct quantized normals to force
        // the migration. The grid above yields ~120 in practice.
        assert!(
            directions.len() > 64,
            "test setup error: only {} distinct quantized normals; need >64 \
             to trigger overflow_buckets migration",
            directions.len()
        );

        // Build a triangle for each normal direction. Pick two orthogonal
        // in-plane axes (u, v) for each n; the triangle (0, u, v) has
        // face normal n.
        let mut vertices: Vec<f32> = Vec::with_capacity(directions.len() * 9);
        let mut indices: Vec<u32> = Vec::with_capacity(directions.len() * 3);
        for (i, &(nx, ny, nz)) in directions.iter().enumerate() {
            // Choose any axis not parallel to n, cross to get u, then v = n × u.
            let helper = if nx.abs() < 0.9 {
                (1.0, 0.0, 0.0)
            } else {
                (0.0, 1.0, 0.0)
            };
            let ux = ny * helper.2 - nz * helper.1;
            let uy = nz * helper.0 - nx * helper.2;
            let uz = nx * helper.1 - ny * helper.0;
            let um = (ux * ux + uy * uy + uz * uz).sqrt();
            let (ux, uy, uz) = (ux / um, uy / um, uz / um);
            let vx = ny * uz - nz * uy;
            let vy = nz * ux - nx * uz;
            let vz = nx * uy - ny * ux;

            let base = (i * 3) as u32;
            vertices.extend_from_slice(&[0.0, 0.0, 0.0]);
            vertices.extend_from_slice(&[ux, uy, uz]);
            vertices.extend_from_slice(&[vx, vy, vz]);
            indices.extend_from_slice(&[base, base + 1, base + 2]);
        }

        let q = compute(&vertices, &indices, 1.0);

        // Every triangle's normal quantizes to a distinct key by
        // construction, so surface_count must equal the input count.
        assert_eq!(
            q.surface_count, directions.len() as u32,
            "surface_count: expected {}, got {}",
            directions.len(),
            q.surface_count
        );

        // The per-surface areas must sum to total surface_area_m2.
        let surface_sum: f32 = q.surfaces.iter().map(|s| s.area_m2).sum();
        assert!(
            (surface_sum - q.surface_area_m2).abs() < 1e-3,
            "surface area sum mismatch: surfaces sum to {surface_sum:.6}, \
             total is {:.6}",
            q.surface_area_m2
        );
    }

    #[test]
    fn mesh_quality_closed_for_closed_manifold() {
        // The unit cube is a closed orientable manifold — divergence
        // volume equals AABB volume to within float precision.
        let (v, i) = unit_cube_world();
        let q = compute(&v, &i, 1.0);
        assert_eq!(q.mesh_quality, "closed");
    }

    #[test]
    fn mesh_quality_degenerate_for_no_triangles() {
        // Empty input → early-out → degenerate.
        let q = compute(&[], &[], 1.0);
        assert_eq!(q.mesh_quality, "degenerate");
        assert_eq!(q.aabb_volume_m3, 0.0);
    }

    #[test]
    fn mesh_quality_degenerate_for_planar_mesh() {
        // Two triangles forming a unit square at z=0. AABB has zero
        // depth so aabb_volume_m3 == 0 → degenerate (also true for
        // single annotations, drawn polylines, etc.).
        let vertices: Vec<f32> = vec![
            0.0, 0.0, 0.0,
            1.0, 0.0, 0.0,
            1.0, 1.0, 0.0,
            0.0, 1.0, 0.0,
        ];
        let indices: Vec<u32> = vec![0, 1, 2,  0, 2, 3];
        let q = compute(&vertices, &indices, 1.0);
        assert_eq!(q.mesh_quality, "degenerate");
        assert!(q.aabb_volume_m3 <= f32::EPSILON);
        // Unsigned triangle areas sum to 1 m² (the unit square).
        assert!((q.surface_area_m2 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn mesh_quality_open_shell_for_unclosed_box() {
        // Unit cube translated far from origin, missing its bottom
        // face. The divergence theorem implicitly closes the surface
        // by joining the open boundary to the origin — when the mesh
        // is far from origin the resulting "phantom" tetrahedra dwarf
        // the AABB. This is the classifier's intended target: it
        // catches *unambiguously* invalid volumes, not every open
        // shell. (A cube centered at origin with a face removed gives
        // |volume| ≈ 5/6 · aabb — wrong but inside the AABB; the
        // classifier won't flag it. That's a known under-detection.)
        let offset = 10.0_f32;
        let v: Vec<f32> = vec![
            offset + -0.5, offset + -0.5, offset + -0.5,
            offset +  0.5, offset + -0.5, offset + -0.5,
            offset +  0.5, offset +  0.5, offset + -0.5,
            offset + -0.5, offset +  0.5, offset + -0.5,
            offset + -0.5, offset + -0.5, offset +  0.5,
            offset +  0.5, offset + -0.5, offset +  0.5,
            offset +  0.5, offset +  0.5, offset +  0.5,
            offset + -0.5, offset +  0.5, offset +  0.5,
        ];
        // Same wind order as unit_cube_world, with the BOTTOM face
        // (first two triangles) removed.
        let i: Vec<u32> = vec![
            // top
            4, 5, 6,  4, 6, 7,
            // -Y
            0, 1, 5,  0, 5, 4,
            // +Y
            3, 7, 6,  3, 6, 2,
            // -X
            0, 4, 7,  0, 7, 3,
            // +X
            1, 2, 6,  1, 6, 5,
        ];
        let q = compute(&v, &i, 1.0);
        // AABB still 1.0 m³ — the missing face's vertices are still
        // referenced by adjacent faces.
        assert!((q.aabb_volume_m3 - 1.0).abs() < 1e-5);
        let exceeds = q.volume_m3.abs() > q.aabb_volume_m3 * 1.001;
        assert!(
            exceeds,
            "open-shell translated cube: |volume_m3|={:.6} should \
             exceed aabb_volume_m3={:.6}",
            q.volume_m3, q.aabb_volume_m3
        );
        assert_eq!(q.mesh_quality, "open_shell");
    }

    #[test]
    fn edge_pairing_catches_open_shell_with_volume_inside_aabb() {
        // The case the cheap `|volume| > aabb` heuristic misses:
        // a unit cube centered at origin with one face removed. The
        // divergence-theorem volume comes out to ~5/6 the cube
        // volume — wrong but bounded inside the AABB. Without the
        // edge-pairing check this would label "closed".
        let (v, mut i) = unit_cube_world();
        // Remove the top face (indices 6..12 — the +Z face).
        i.drain(6..12);
        let q = compute(&v, &i, 1.0);
        // Sanity: AABB still 1 m³ (the removed face's vertices stay
        // referenced by adjacent faces, so the AABB doesn't shrink).
        assert!((q.aabb_volume_m3 - 1.0).abs() < 1e-5);
        // The classifier's old cheap check would not flag this — the
        // divergence volume lands inside the AABB. The edge-pairing
        // tier catches it.
        assert!(
            q.volume_m3.abs() < q.aabb_volume_m3 * 1.001,
            "test setup bug: case must have |volume|={:.6} <= aabb={:.6} \
             so the cheap heuristic misses it, forcing edge-pairing",
            q.volume_m3.abs(), q.aabb_volume_m3
        );
        assert_eq!(q.mesh_quality, "open_shell");
    }

    #[test]
    fn edge_pairing_helper_returns_true_for_unit_cube() {
        let (_v, i) = unit_cube_world();
        assert!(is_closed_manifold(&i));
    }

    #[test]
    fn edge_pairing_helper_returns_false_for_unit_cube_missing_face() {
        let (_v, mut i) = unit_cube_world();
        i.drain(6..12); // drop the +Z face's two triangles
        assert!(!is_closed_manifold(&i));
    }

    #[test]
    fn edge_pairing_helper_rejects_same_winding_pair() {
        // Two triangles sharing all three vertices, wound the same
        // way — looks like a duplicate, contributes the same directed
        // edges twice. signed_count = ±2 on every edge → not closed.
        let i = vec![0, 1, 2,  0, 1, 2];
        assert!(!is_closed_manifold(&i));
    }

    #[test]
    fn volume_reliable_uses_mesh_value_for_closed_cube() {
        // Closed manifold → reliable; volume_best is the mesh volume,
        // method "mesh", and the prism bound is NaN (not computed on the
        // hot path).
        let (v, i) = unit_cube_world();
        let q = compute(&v, &i, 1.0);
        assert!(q.volume_reliable);
        assert_eq!(q.volume_method, "mesh");
        assert!((q.volume_best_m3 - 1.0).abs() < 1e-5, "got {}", q.volume_best_m3);
        assert!(
            q.volume_prism_bound_m3.is_nan(),
            "reliable rows leave the prism bound uncomputed (NaN), got {}",
            q.volume_prism_bound_m3
        );
    }

    #[test]
    fn prism_fallback_replaces_garbage_volume_on_open_shell() {
        // The far-offset cube missing its bottom face: the signed-tetra
        // volume is enormous garbage (volume > aabb), but the footprint
        // (1 m²) × z_extent (1 m) prism is a clean ~1 m³ estimate. The
        // best estimate must switch to the prism, NOT the mesh garbage.
        let offset = 10.0_f32;
        let v: Vec<f32> = vec![
            offset + -0.5, offset + -0.5, offset + -0.5,
            offset +  0.5, offset + -0.5, offset + -0.5,
            offset +  0.5, offset +  0.5, offset + -0.5,
            offset + -0.5, offset +  0.5, offset + -0.5,
            offset + -0.5, offset + -0.5, offset +  0.5,
            offset +  0.5, offset + -0.5, offset +  0.5,
            offset +  0.5, offset +  0.5, offset +  0.5,
            offset + -0.5, offset +  0.5, offset +  0.5,
        ];
        // Same wind as unit_cube_world with the bottom face removed.
        let i: Vec<u32> = vec![
            4, 5, 6,  4, 6, 7,
            0, 1, 5,  0, 5, 4,
            3, 7, 6,  3, 6, 2,
            0, 4, 7,  0, 7, 3,
            1, 2, 6,  1, 6, 5,
        ];
        let q = compute(&v, &i, 1.0);
        assert_eq!(q.mesh_quality, "open_shell");
        assert!(!q.volume_reliable);
        assert_eq!(q.volume_method, "prism_fallback");
        // Raw mesh garbage is preserved on `volume_m3` (signed) but the
        // best estimate is the prism, ~1 m³ (footprint 1 m² × 1 m).
        assert!(q.volume_m3.abs() > q.aabb_volume_m3, "setup: mesh vol should be garbage");
        assert!(
            (q.volume_best_m3 - 1.0).abs() < 0.02,
            "prism fallback should reconstruct ~1 m³, got {}",
            q.volume_best_m3
        );
        assert!(
            (q.volume_prism_bound_m3 - q.volume_best_m3).abs() < 1e-6,
            "prism column should equal the chosen fallback value"
        );
    }

    #[test]
    fn open_shell_within_prism_bound_keeps_mesh_value() {
        // Unit cube at origin missing its top face: edge-pairing flags it
        // open_shell, but the divergence volume (~5/6 m³) sits well within
        // the prism bound (~1 m³), so we KEEP the mesh value rather than
        // regress it to the looser prism. Mirrors the real-file finding
        // that dedup-imperfect open shells are ~0.2 % accurate.
        let (v, mut i) = unit_cube_world();
        i.drain(6..12); // remove the +Z face's two triangles
        let q = compute(&v, &i, 1.0);
        assert_eq!(q.mesh_quality, "open_shell");
        assert!(q.volume_reliable, "mesh within its prism bound → trusted");
        assert_eq!(q.volume_method, "mesh");
        assert!(
            (q.volume_best_m3 - q.volume_m3.abs()).abs() < 1e-6,
            "reliable open shell keeps the mesh value"
        );
        // The prism bound is computed for non-closed rows and is a valid
        // (within margin) upper bound on the kept mesh value.
        assert!(q.volume_prism_bound_m3.is_finite());
        assert!(q.volume_best_m3 <= q.volume_prism_bound_m3 * 1.05 + 1e-6);
    }

    #[test]
    fn degenerate_row_is_unreliable_with_zero_fallback() {
        // Empty mesh → degenerate, not reliable, zero fallback (nothing
        // to escalate a volume for).
        let q = compute(&[], &[], 1.0);
        assert_eq!(q.mesh_quality, "degenerate");
        assert!(!q.volume_reliable);
        assert_eq!(q.volume_method, "prism_fallback");
        assert_eq!(q.volume_best_m3, 0.0);
        assert_eq!(q.volume_prism_bound_m3, 0.0);
    }

    #[test]
    fn footprint_raster_recovers_unit_square() {
        // Two triangles spanning the unit square at z=0..1; the XY
        // footprint must be ~1 m² to within grid quantization.
        let v: Vec<f32> = vec![
            0.0, 0.0, 0.0,
            1.0, 0.0, 0.0,
            1.0, 1.0, 1.0,
            0.0, 1.0, 1.0,
        ];
        let i: Vec<u32> = vec![0, 1, 2,  0, 2, 3];
        let fp = footprint_xy_raw(&v, &i, 0.0, 1.0, 0.0, 1.0);
        assert!((fp - 1.0).abs() < 1e-3, "footprint should be ~1 m², got {fp}");
    }
}
