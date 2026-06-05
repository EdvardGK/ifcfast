//! Plane-clipping primitive: subtract a half-space from a closed
//! triangle mesh, return the closed-manifold result.
//!
//! Why this exists (GH #39): `IfcHalfSpaceSolid` cutters in
//! `IfcBooleanClippingResult` trees can't reliably go through
//! `manifold-csg`. The half-space is mathematically infinite, but
//! Manifold accepts only closed meshes, so we'd have to materialise
//! the half-space as a bounded box. Two failure modes follow:
//!
//! * A box sized to "definitely contain the half-space" (e.g. 20 m
//!   side) stacks across a 13-deep clip tree and consumes the host
//!   entirely — the union of materialised cubes covers the wall.
//! * A box bounded to the host AABB tickles Manifold issue #1714
//!   (the boolean's `tolerance_` inherits the cutter's bbox-derived
//!   value; per-vertex predicates then treat all positions as
//!   coincident and the result collapses to empty).
//!
//! Production IFC engines that get this right (ifcopenshell via
//! OpenCASCADE's `BRepPrimAPI_MakeHalfSpace`, Revit, CGAL via
//! `Polygon_mesh_processing::clip(plane)`) all treat the half-space
//! as a *first-class plane primitive*, never as a bounded mesh. This
//! module is the pure-Rust analogue: signed-distance per-triangle
//! classification, edge splitting on the plane, cap the cut with
//! [`earcutr`] so the output stays closed-manifold for downstream
//! consumers (notably `geom::csg::subtract_many` on the residual
//! cross-product openings).
//!
//! Contract:
//! * Input is a closed triangle mesh with outward-facing winding
//!   (the boolean tree's host operand at fold time).
//! * `plane_normal` is unit-length and points *into* the half-space
//!   we want to remove (i.e. toward the cutter's "agree" side).
//! * Output keeps the negative half-space (`(v - point)·n < 0`) and
//!   caps the cut with an outward-facing polygon on the plane.
//! * Empty result (every vertex on the agree side) is returned as
//!   empty buffers — caller decides whether that's a passthrough or
//!   a real "host fully consumed" event.

use std::collections::HashMap;

use glam::Vec3;

use crate::mesh::profile::Polygon2D;

/// Tolerance for treating a vertex as "on the plane". Picked to be
/// well below building-element tolerance (1 mm) while still firmly
/// above f32 round-off at the world coordinates ifcfast operates on
/// (vertices arrive in the wall's local-rebased frame, so values
/// rarely exceed 1e5 mm).
const ON_PLANE_EPS: f32 = 1.0e-3;

/// Clip a closed triangle mesh by a plane, returning the closed
/// portion on the **negative** side (i.e. the half-space *opposite*
/// the one `plane_normal` points into).
///
/// `vertices` is a flat `[x, y, z, x, y, z, ...]` buffer and
/// `indices` is the triangle list (always a multiple of 3). The
/// resulting mesh is also flat-buffer + triangle-list, with the cap
/// polygon already triangulated and stitched.
///
/// Returns `(Vec::new(), Vec::new())` when no triangle survives (the
/// host lies entirely inside the half-space the cutter represents).
/// Returns the input unchanged when no triangle is removed (the
/// host lies entirely outside the half-space — caller can short-
/// circuit if performance matters).
pub fn clip_by_plane(
    vertices: &[f32],
    indices: &[u32],
    plane_point: Vec3,
    plane_normal: Vec3,
) -> (Vec<f32>, Vec<u32>) {
    if indices.is_empty() || vertices.len() < 9 {
        return (vertices.to_vec(), indices.to_vec());
    }
    let n = plane_normal.normalize_or_zero();
    if n.length_squared() < 0.5 {
        return (vertices.to_vec(), indices.to_vec());
    }

    // ----- Phase 1: per-vertex signed distance & sign --------------
    // Treat |d| < ON_PLANE_EPS as "on plane → outside" (conservative
    // toward removing tiny slivers, which keeps the cap topology
    // crisp at the cost of dropping sub-mm geometry near the plane).
    let nv = vertices.len() / 3;
    let mut dist: Vec<f32> = Vec::with_capacity(nv);
    let mut inside: Vec<bool> = Vec::with_capacity(nv);
    for chunk in vertices.chunks_exact(3) {
        let v = Vec3::new(chunk[0], chunk[1], chunk[2]);
        let d = (v - plane_point).dot(n);
        dist.push(d);
        inside.push(d < -ON_PLANE_EPS);
    }

    // Fast paths.
    let in_count = inside.iter().filter(|&&b| b).count();
    if in_count == 0 {
        return (Vec::new(), Vec::new());
    }
    if in_count == nv {
        // Every vertex strictly inside the keep half-space; the
        // triangles can only be in three states:
        //  * all 3 inside → keep
        //  * any vertex with |d| < eps treated as "outside" → split
        // Since every vertex is inside per `inside[i]==true`, no
        // splits — full passthrough.
        return (vertices.to_vec(), indices.to_vec());
    }

    // ----- Phase 2: clip triangles --------------------------------
    let mut out_v: Vec<f32> = vertices.to_vec();
    let mut out_i: Vec<u32> = Vec::with_capacity(indices.len());
    // Cache edge-plane intersections so two triangles sharing an
    // edge end up at the same new vertex. Key is the sorted endpoint
    // pair in the input vertex space.
    let mut edge_cache: HashMap<(u32, u32), u32> = HashMap::new();
    // Boundary edges (start, end) in the output vertex space,
    // wound so that walking each loop traces the cap CCW when
    // viewed from `+plane_normal` (outside of the new closed mesh).
    let mut boundary: Vec<(u32, u32)> = Vec::new();

    for tri in indices.chunks_exact(3) {
        let a = tri[0];
        let b = tri[1];
        let c = tri[2];
        let ia = inside[a as usize];
        let ib = inside[b as usize];
        let ic = inside[c as usize];
        match (ia, ib, ic) {
            (true, true, true) => {
                out_i.push(a);
                out_i.push(b);
                out_i.push(c);
            }
            (false, false, false) => {}
            _ => {
                // Mixed — split. Identify which vertices are inside.
                let count = (ia as u8) + (ib as u8) + (ic as u8);
                if count == 1 {
                    // One inside, two outside. Walk the triangle so
                    // `k` is the inside vertex and `r1, r2` are the
                    // two outside vertices in CCW order.
                    let (k, r1, r2) = if ia {
                        (a, b, c)
                    } else if ib {
                        (b, c, a)
                    } else {
                        (c, a, b)
                    };
                    let i_k_r1 = intersect(
                        k,
                        r1,
                        &vertices,
                        &dist,
                        &mut out_v,
                        &mut edge_cache,
                    );
                    let i_k_r2 = intersect(
                        k,
                        r2,
                        &vertices,
                        &dist,
                        &mut out_v,
                        &mut edge_cache,
                    );
                    // New triangle (k, i_k_r1, i_k_r2) — CCW from
                    // outside, original winding preserved.
                    out_i.push(k);
                    out_i.push(i_k_r1);
                    out_i.push(i_k_r2);
                    // Boundary edge on the plane for the cap. We
                    // walked the triangle CCW from outside, so the
                    // edge going from i_k_r2 to i_k_r1 closes a
                    // loop wound CCW seen from `+plane_normal`.
                    boundary.push((i_k_r2, i_k_r1));
                } else {
                    // Two inside, one outside. Walk so `r` is the
                    // outside vertex and `k1, k2` are the two
                    // inside vertices in CCW order.
                    let (k1, k2, r) = if !ia {
                        (b, c, a)
                    } else if !ib {
                        (c, a, b)
                    } else {
                        (a, b, c)
                    };
                    let i_k1_r = intersect(
                        k1,
                        r,
                        &vertices,
                        &dist,
                        &mut out_v,
                        &mut edge_cache,
                    );
                    let i_k2_r = intersect(
                        k2,
                        r,
                        &vertices,
                        &dist,
                        &mut out_v,
                        &mut edge_cache,
                    );
                    // Quad k1 → k2 → i_k2_r → i_k1_r → k1, split into
                    // two CCW triangles.
                    out_i.push(k1);
                    out_i.push(k2);
                    out_i.push(i_k2_r);
                    out_i.push(k1);
                    out_i.push(i_k2_r);
                    out_i.push(i_k1_r);
                    // Boundary edge for the cap: from i_k2_r to
                    // i_k1_r.
                    boundary.push((i_k1_r, i_k2_r));
                }
            }
        }
    }

    if boundary.is_empty() {
        // Either every kept triangle is fully inside (no plane
        // intersection) — host is closed without needing a cap — or
        // the boundary degenerated. Either way the output is what
        // we have.
        return (out_v, out_i);
    }

    // ----- Phase 3: stitch boundary edges into loops --------------
    let mut loops = stitch_boundary_loops(&boundary);
    if loops.is_empty() {
        return (out_v, out_i);
    }

    // ----- Phase 4: triangulate each cap loop ---------------------
    // Local 2D basis in the plane: e1 ⟂ n, e2 = n × e1.
    let temp = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let e1 = n.cross(temp).normalize();
    let e2 = n.cross(e1).normalize();
    for loop_indices in loops.drain(..) {
        triangulate_cap_loop(&loop_indices, &out_v, plane_point, e1, e2, &mut out_i);
    }

    // ----- Phase 5: compact (drop unreferenced vertices) ----------
    // Downstream consumers — manifold-csg's `build_manifold`, the
    // wheel's vertex-counter for stats — expect a tight vertex
    // buffer. Phase 2 keeps every original input vertex around (so
    // surviving triangles can reference them by their original
    // index without a remap pass per triangle); now we collapse the
    // buffer to just the referenced subset.
    compact_mesh(&out_v, &out_i)
}

fn compact_mesh(vertices: &[f32], indices: &[u32]) -> (Vec<f32>, Vec<u32>) {
    if indices.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut remap: HashMap<u32, u32> = HashMap::with_capacity(indices.len() / 3);
    let mut packed_v: Vec<f32> = Vec::new();
    let mut packed_i: Vec<u32> = Vec::with_capacity(indices.len());
    for &i in indices {
        let new_i = *remap.entry(i).or_insert_with(|| {
            let pos = (packed_v.len() / 3) as u32;
            let base = (i as usize) * 3;
            packed_v.push(vertices[base]);
            packed_v.push(vertices[base + 1]);
            packed_v.push(vertices[base + 2]);
            pos
        });
        packed_i.push(new_i);
    }
    (packed_v, packed_i)
}

/// Compute the intersection of edge `(a, b)` with the plane and
/// return the index of the resulting vertex in `out_v`. Cached on
/// the sorted endpoint pair so two triangles sharing the edge end
/// up at the same vertex (cap-loop stitching depends on this).
fn intersect(
    a: u32,
    b: u32,
    vertices: &[f32],
    dist: &[f32],
    out_v: &mut Vec<f32>,
    edge_cache: &mut HashMap<(u32, u32), u32>,
) -> u32 {
    let key = if a < b { (a, b) } else { (b, a) };
    if let Some(&idx) = edge_cache.get(&key) {
        return idx;
    }
    let ai = a as usize;
    let bi = b as usize;
    let va = Vec3::new(vertices[ai * 3], vertices[ai * 3 + 1], vertices[ai * 3 + 2]);
    let vb = Vec3::new(vertices[bi * 3], vertices[bi * 3 + 1], vertices[bi * 3 + 2]);
    let da = dist[ai];
    let db = dist[bi];
    // Linear interpolation between signed distances. Guarded against
    // pathological da == db (parallel triangle, should never fire on
    // the mixed-sign branch but defensive).
    let denom = da - db;
    let t = if denom.abs() < f32::EPSILON {
        0.5
    } else {
        da / denom
    };
    let t = t.clamp(0.0, 1.0);
    let p = va + t * (vb - va);
    let idx = (out_v.len() / 3) as u32;
    out_v.push(p.x);
    out_v.push(p.y);
    out_v.push(p.z);
    edge_cache.insert(key, idx);
    idx
}

/// Walk the boundary edge list into closed loops. Each loop is a
/// sequence of vertex indices in `out_v` such that consecutive
/// entries are connected by a boundary edge.
fn stitch_boundary_loops(edges: &[(u32, u32)]) -> Vec<Vec<u32>> {
    // Map start → end. If multiple edges share a start vertex
    // (pathological), the later one overrides — caller would have
    // already produced a degenerate mesh in that case.
    let mut next: HashMap<u32, u32> = HashMap::with_capacity(edges.len());
    for &(s, e) in edges {
        next.insert(s, e);
    }
    let mut visited: HashMap<u32, bool> = HashMap::with_capacity(edges.len());
    let mut loops: Vec<Vec<u32>> = Vec::new();
    for &(start, _) in edges {
        if visited.get(&start).copied().unwrap_or(false) {
            continue;
        }
        let mut loop_idx: Vec<u32> = Vec::new();
        let mut cur = start;
        // Cap the walk at the number of edges; if we ever cycle
        // beyond it we've hit a malformed graph (silent guard).
        let max_steps = edges.len() + 1;
        let mut steps = 0;
        loop {
            if visited.get(&cur).copied().unwrap_or(false) {
                break;
            }
            visited.insert(cur, true);
            loop_idx.push(cur);
            let nxt = match next.get(&cur) {
                Some(&n) => n,
                None => break,
            };
            if nxt == start {
                break;
            }
            cur = nxt;
            steps += 1;
            if steps > max_steps {
                break;
            }
        }
        if loop_idx.len() >= 3 {
            loops.push(loop_idx);
        }
    }
    loops
}

/// Triangulate one cap loop with earcutr and append the cap
/// triangles to `out_i`. The 3D loop lives in the plane defined by
/// `plane_point` and orthonormal basis `(e1, e2)`; we project to 2D
/// in that basis, triangulate, and lift the resulting triangle
/// indices back to the original 3D vertex indices.
fn triangulate_cap_loop(
    loop_indices: &[u32],
    out_v: &[f32],
    plane_point: Vec3,
    e1: Vec3,
    e2: Vec3,
    out_i: &mut Vec<u32>,
) {
    let mut flat_2d: Vec<f64> = Vec::with_capacity(loop_indices.len() * 2);
    for &idx in loop_indices {
        let base = (idx as usize) * 3;
        let p = Vec3::new(out_v[base], out_v[base + 1], out_v[base + 2]);
        let local = p - plane_point;
        let u = local.dot(e1) as f64;
        let v = local.dot(e2) as f64;
        flat_2d.push(u);
        flat_2d.push(v);
    }
    // No holes — single outer loop only. (Walls with cavities
    // produce one loop per connected component, each triangulated
    // independently by the caller.)
    let tris = match earcutr::earcut(&flat_2d, &[], 2) {
        Ok(t) => t,
        Err(_) => return,
    };
    for tri in tris.chunks_exact(3) {
        let a = loop_indices[tri[0]];
        let b = loop_indices[tri[1]];
        let c = loop_indices[tri[2]];
        out_i.push(a);
        out_i.push(b);
        out_i.push(c);
    }
}

// `Polygon2D` is unused here but imported transitively for the
// crate's mesh module layout; silence unused-import warnings.
#[allow(dead_code)]
const _USES_POLYGON: Option<Polygon2D> = None;

// ----- Tests ------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed axis-aligned cube, 8 vertices, 12 triangles, outward
    /// CCW winding. The cube spans `[min, max]` on each axis.
    fn cube(min: Vec3, max: Vec3) -> (Vec<f32>, Vec<u32>) {
        let v = vec![
            min.x, min.y, min.z,
            max.x, min.y, min.z,
            max.x, max.y, min.z,
            min.x, max.y, min.z,
            min.x, min.y, max.z,
            max.x, min.y, max.z,
            max.x, max.y, max.z,
            min.x, max.y, max.z,
        ];
        let i = vec![
            0, 2, 1, 0, 3, 2,
            4, 5, 6, 4, 6, 7,
            0, 1, 5, 0, 5, 4,
            2, 3, 7, 2, 7, 6,
            1, 2, 6, 1, 6, 5,
            0, 4, 7, 0, 7, 3,
        ];
        (v, i)
    }

    fn signed_volume(verts: &[f32], idx: &[u32]) -> f32 {
        let mut sum = 0.0_f64;
        for tri in idx.chunks_exact(3) {
            let a = tri[0] as usize;
            let b = tri[1] as usize;
            let c = tri[2] as usize;
            let v0 = Vec3::new(verts[a * 3], verts[a * 3 + 1], verts[a * 3 + 2]);
            let v1 = Vec3::new(verts[b * 3], verts[b * 3 + 1], verts[b * 3 + 2]);
            let v2 = Vec3::new(verts[c * 3], verts[c * 3 + 1], verts[c * 3 + 2]);
            sum += (v0.dot(v1.cross(v2)) / 6.0) as f64;
        }
        sum as f32
    }

    #[test]
    fn clip_unit_cube_in_half_along_z() {
        // 1×1×1 cube at origin clipped by z=0.5 plane with normal +Z
        // (removes the upper half) → lower half remains, volume 0.5.
        let (v, i) = cube(Vec3::ZERO, Vec3::ONE);
        let (cv, ci) = clip_by_plane(&v, &i, Vec3::new(0.0, 0.0, 0.5), Vec3::Z);
        let vol = signed_volume(&cv, &ci);
        assert!(
            (vol - 0.5).abs() < 1e-4,
            "expected ~0.5, got {vol} (verts={}, tris={})",
            cv.len() / 3,
            ci.len() / 3
        );
    }

    #[test]
    fn clip_with_plane_entirely_outside_keeps_host() {
        // Plane at z=2, normal +Z → cube (z in [0,1]) is entirely
        // on the keep (negative) side. Passthrough.
        let (v, i) = cube(Vec3::ZERO, Vec3::ONE);
        let (cv, ci) = clip_by_plane(&v, &i, Vec3::new(0.0, 0.0, 2.0), Vec3::Z);
        assert_eq!(cv.len() / 3, v.len() / 3);
        assert_eq!(ci.len() / 3, i.len() / 3);
        let vol = signed_volume(&cv, &ci);
        assert!((vol - 1.0).abs() < 1e-4, "vol should still be ~1, got {vol}");
    }

    #[test]
    fn clip_with_plane_entirely_inside_empties_host() {
        // Plane at z=-1, normal +Z → cube (z in [0,1]) is entirely
        // on the remove (positive) side. Result empty.
        let (v, i) = cube(Vec3::ZERO, Vec3::ONE);
        let (cv, ci) = clip_by_plane(&v, &i, Vec3::new(0.0, 0.0, -1.0), Vec3::Z);
        assert!(cv.is_empty() && ci.is_empty(), "expected empty result");
    }

    #[test]
    fn clip_oblique_plane_bisects_cube() {
        // Cube [0,1]^3 clipped by the plane through (0.5, 0.5, 0.5)
        // with normal (1,1,1)/sqrt(3). This diagonal plane passes
        // through the cube's centroid and bisects it: 4 corners
        // (000, 100, 010, 001) on the keep side, 4 corners
        // (111, 110, 101, 011) on the remove side. Volumes are
        // mirror-symmetric → exactly 0.5 each.
        let (v, i) = cube(Vec3::ZERO, Vec3::ONE);
        let n = Vec3::new(1.0, 1.0, 1.0).normalize();
        let (cv, ci) = clip_by_plane(&v, &i, Vec3::splat(0.5), n);
        let vol = signed_volume(&cv, &ci);
        assert!(
            (vol - 0.5).abs() < 1e-4,
            "expected ~0.5 (bisection), got {vol}"
        );
    }

    #[test]
    fn clip_corner_off_keeps_seven_eighths() {
        // Cube [0,1]^3 with a plane through (0.75, 0.75, 0.75) and
        // normal (1,1,1)/sqrt(3) — this cuts off just the corner
        // near (1,1,1). The removed tetrahedron has legs of length
        // 0.75 along the three axes (where the plane crosses each
        // edge from (1,1,1)), volume = (0.75)^3 / 6 = 0.070313.
        // Expected remaining: 1 − 0.070313 = 0.929688.
        let (v, i) = cube(Vec3::ZERO, Vec3::ONE);
        let n = Vec3::new(1.0, 1.0, 1.0).normalize();
        let (cv, ci) = clip_by_plane(&v, &i, Vec3::splat(0.75), n);
        let vol = signed_volume(&cv, &ci);
        let expected = 1.0 - 0.75_f32.powi(3) / 6.0;
        assert!(
            (vol - expected).abs() < 5e-4,
            "expected ~{expected} after corner cut, got {vol}"
        );
    }
}
