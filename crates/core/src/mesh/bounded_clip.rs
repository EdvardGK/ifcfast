//! Pure-Rust clip of a closed mesh by an `IfcPolygonalBoundedHalfSpace`
//! (GH #194).
//!
//! An `IfcBooleanClippingResult` whose second operand is a polygonal
//! bounded half-space removes `host ∩ K`, where `K` is the half-space on
//! the remove side of the base plane intersected with the boundary
//! polygon's column (the polygon projected onto the plane and swept along
//! the plane normal — the same footprint [`bounded_footprint`] gives the
//! finite CSG cutter, so every route agrees on the shape). Since GH #194
//! the clip is the element's own shape and runs in every mode, including
//! the wasm build, which has no Manifold — so the bounded cut needs a
//! pure-Rust route.
//!
//! Two routes, in order:
//!
//! 1. **Column contains the removed part** (Revit's usual oversized
//!    boundary): every point of `host` on the remove side projects inside
//!    the footprint, so `host ∩ K = host ∩ halfspace` and the result is
//!    exactly the plane clip ([`clip_by_plane`]).
//! 2. **Convex footprint, general case**: `K` is a convex right prism
//!    (base polygon on the plane, open along `+n`). The host is split by
//!    every plane of `K` (base plane + one side plane per footprint edge)
//!    with a shared edge cache, so the split surface stays watertight;
//!    triangles inside all planes are dropped; and the hole is closed
//!    face by face on `∂K`: the rim edges lying on a face are chained,
//!    the chains are linked along the face boundary (Weiler–Atherton,
//!    both oriented counter-clockwise seen from the cap's outward normal,
//!    walking through `K`'s base corners where needed), and each face's
//!    loops are triangulated with `earcutr`. The output must pass the
//!    closed-manifold edge check and a volume bound, or the route
//!    reports [`BoundedClip::Failed`].
//!
//! A non-convex footprint that is not covered by route 1 is reported as
//! [`BoundedClip::NonConvex`] — the caller falls back to Manifold (when
//! `csg` is compiled in) or leaves the host unclipped and marks it.
//!
//! Vertices are classified IN / ON / OUT per plane of `K` with the `eps`
//! band (ON = within `eps`, never split), so a host vertex on a boundary
//! plane is reused instead of spawning a near-duplicate split point. A
//! host face lying entirely ON a plane is resolved by orientation: with
//! K's own orientation it is inside `K` and removed, against it it only
//! touches `K` and stays — never a zero-thickness fin. Revit boundaries
//! are routinely flush with the wall faces, so this is the common case.
//! Distances and split points are `f64`.

use std::collections::{HashMap, HashSet};

use glam::{DVec2, DVec3, Mat4, Vec3};

use crate::mesh::halfspace_clip::{bounded_footprint, clip_by_plane};
use crate::mesh::profile::Polygon2D;

/// Outcome of [`clip_bounded`].
#[derive(Debug)]
pub enum BoundedClip {
    /// `K` does not reach the host: nothing removed, host unchanged.
    Unchanged,
    /// The clipped host (empty buffers = the host was consumed).
    Clipped {
        vertices: Vec<f32>,
        indices: Vec<u32>,
    },
    /// The footprint is not convex and the column does not contain the
    /// removed part — outside the pure-Rust domain.
    NonConvex,
    /// Degenerate boundary, non-closed host, or the construction failed
    /// its own closure / volume check.
    Failed,
}

/// Label of a vertex against one plane of `K`.
const IN: i8 = -1;
const ON: i8 = 0;
const OUT: i8 = 1;
const UNSET: i8 = 2;

/// Subtract the bounded half-space `K` from the closed mesh
/// `(vertices, indices)`. Plane / boundary are in the mesh's frame;
/// `plane_normal` points into the removed side (the
/// [`crate::mesh::BoundedHalfspacePayload`] convention). `eps` is the
/// on-plane guard in source units ([`crate::mesh::halfspace_clip::on_plane_eps`]).
pub fn clip_bounded(
    vertices: &[f32],
    indices: &[u32],
    boundary: &Polygon2D,
    boundary_xform: Mat4,
    plane_point: Vec3,
    plane_normal: Vec3,
    eps: f32,
) -> BoundedClip {
    if indices.len() < 3 || vertices.len() < 9 {
        return BoundedClip::Unchanged;
    }
    let Some((e1, e2, n, fp)) =
        bounded_footprint(boundary, boundary_xform, plane_point, plane_normal)
    else {
        return BoundedClip::Failed;
    };
    let eps64 = (eps as f64).max(1.0e-9);
    let e1 = e1.as_dvec3();
    let e2 = e2.as_dvec3();
    let n = n.as_dvec3();
    let p = plane_point.as_dvec3();

    let ring = clean_ring(&fp.outer, eps64);
    if ring.len() < 3 {
        return BoundedClip::Failed;
    }

    // Host extent on the remove side. Nothing past the plane → no cut.
    let pos: Vec<DVec3> = vertices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64))
        .collect();
    let s: Vec<f64> = pos.iter().map(|v| (*v - p).dot(n)).collect();
    let s_max = s.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if s_max <= eps64 {
        return BoundedClip::Unchanged;
    }

    // Route 1: the column contains every point of the host on the remove
    // side → exactly the plane clip.
    let to2 = |v: DVec3| -> DVec2 {
        let d = v - p;
        DVec2::new(d.dot(e1), d.dot(e2))
    };
    let convex = is_convex_ccw(&ring);
    let inside_fp = |q: DVec2| -> bool {
        if convex {
            point_in_convex(&ring, q, eps64)
        } else {
            point_in_polygon(&ring, q)
        }
    };
    let mut contained = true;
    'scan: for tri in indices.as_chunks::<3>().0 {
        for k in 0..3 {
            let a = tri[k] as usize;
            let b = tri[(k + 1) % 3] as usize;
            if a >= pos.len() || b >= pos.len() {
                return BoundedClip::Failed;
            }
            if s[a] > -eps64 && !inside_fp(to2(pos[a])) {
                contained = false;
                break 'scan;
            }
            if (s[a] > 0.0) != (s[b] > 0.0) {
                let t = s[a] / (s[a] - s[b]);
                let x = pos[a] + (pos[b] - pos[a]) * t.clamp(0.0, 1.0);
                if !inside_fp(to2(x)) {
                    contained = false;
                    break 'scan;
                }
            }
        }
    }
    if contained && (convex || ring_edges_miss_removed(&ring, &pos, &s, eps64, to2)) {
        let (v, i) = clip_by_plane(vertices, indices, plane_point, plane_normal, eps);
        // `clip_by_plane` caps with a bare earcut, which can drop
        // collinear cap vertices and leave the shell open. When the input
        // was closed, only accept a closed result; otherwise let the
        // general construction (which closes its caps) take it.
        let input_closed = crate::mesh::qto::is_closed_manifold(indices);
        if i.is_empty() || !input_closed || crate::mesh::qto::is_closed_manifold(&i) {
            return BoundedClip::Clipped {
                vertices: v,
                indices: i,
            };
        }
    }
    if convex {
        return subtract_ring_retrying(&pos, indices, &ring, e1, e2, n, p, s_max, eps64);
    }

    // Non-convex footprint: K is the union of convex columns over a convex
    // decomposition of the footprint (they share only their diagonals), so
    // host − K = host − K₁ − K₂ − … . Each later piece meets the earlier
    // pieces' caps on the shared diagonal plane with its own orientation,
    // which the coplanar rule removes — no internal wall is left behind.
    let Some(pieces) = convex_decomposition(&ring) else {
        return BoundedClip::NonConvex;
    };
    let mut cur_v: Vec<f32> = vertices.to_vec();
    let mut cur_i: Vec<u32> = indices.to_vec();
    let mut changed = false;
    for piece in &pieces {
        let pos: Vec<DVec3> = cur_v
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64))
            .collect();
        let s_max = pos
            .iter()
            .map(|q| (*q - p).dot(n))
            .fold(f64::NEG_INFINITY, f64::max);
        if s_max <= eps64 {
            break;
        }
        match subtract_ring_retrying(&pos, &cur_i, piece, e1, e2, n, p, s_max, eps64) {
            BoundedClip::Unchanged => {}
            BoundedClip::Clipped { vertices, indices } => {
                changed = true;
                cur_v = vertices;
                cur_i = indices;
                if cur_i.is_empty() {
                    break;
                }
            }
            _ => return BoundedClip::NonConvex,
        }
    }
    if changed {
        BoundedClip::Clipped {
            vertices: cur_v,
            indices: cur_i,
        }
    } else {
        BoundedClip::Unchanged
    }
}

/// [`subtract_convex_column`] with a deterministic eps retry: a degenerate
/// incidence inside the eps band (a K edge lying in a host face at an
/// angle the two-face rule cannot resolve, a coplanar patch with both
/// orientations) makes the construction refuse; a narrower then a wider
/// band usually moves the incidence off the degeneracy. Every attempt is
/// still verified closed + volume-bounded.
#[allow(clippy::too_many_arguments)]
fn subtract_ring_retrying(
    pos: &[DVec3],
    indices: &[u32],
    ring: &[DVec2],
    e1: DVec3,
    e2: DVec3,
    n: DVec3,
    p: DVec3,
    s_max: f64,
    eps: f64,
) -> BoundedClip {
    let mut last = BoundedClip::Failed;
    for f in [1.0, 0.125, 8.0] {
        last = subtract_convex_column(pos, indices, ring, e1, e2, n, p, s_max, eps * f);
        if !matches!(last, BoundedClip::Failed) {
            return last;
        }
    }
    last
}

/// Hertel–Mehlhorn: triangulate the CCW ring (earcut) and greedily merge
/// triangles across diagonals while the union stays strictly convex.
/// Returns CCW convex pieces covering the ring exactly (at most 4× the
/// optimal count). `None` when the ring cannot be triangulated.
fn convex_decomposition(ring: &[DVec2]) -> Option<Vec<Vec<DVec2>>> {
    let flat: Vec<f64> = ring.iter().flat_map(|q| [q.x, q.y]).collect();
    let tri = earcutr::earcut(&flat, &[], 2).ok()?;
    if tri.is_empty() {
        return None;
    }
    let area = |poly: &[usize]| -> f64 {
        let m = poly.len();
        (0..m)
            .map(|i| ring[poly[i]].perp_dot(ring[poly[(i + 1) % m]]))
            .sum::<f64>()
    };
    let mut polys: Vec<Vec<usize>> = tri
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let mut p = vec![t[0], t[1], t[2]];
            if area(&p) < 0.0 {
                p.reverse();
            }
            p
        })
        .collect();
    let convex = |poly: &[usize]| -> bool {
        let pts: Vec<DVec2> = poly.iter().map(|&i| ring[i]).collect();
        is_convex_ccw(&pts)
    };
    loop {
        let mut merged = false;
        'outer: for a in 0..polys.len() {
            for b in (a + 1)..polys.len() {
                let (pa, pb) = (&polys[a], &polys[b]);
                // Shared edge u→v in pa appears as v→u in pb.
                for ia in 0..pa.len() {
                    let (u, v) = (pa[ia], pa[(ia + 1) % pa.len()]);
                    let Some(ib) =
                        (0..pb.len()).find(|&k| pb[k] == v && pb[(k + 1) % pb.len()] == u)
                    else {
                        continue;
                    };
                    // pa from v around to u, then pb from u around to v
                    // (endpoints shared, not repeated).
                    let mut m: Vec<usize> =
                        (0..pa.len()).map(|k| pa[(ia + 1 + k) % pa.len()]).collect();
                    m.extend((1..pb.len() - 1).map(|k| pb[(ib + 1 + k) % pb.len()]));
                    if convex(&m) {
                        polys[a] = m;
                        polys.remove(b);
                        merged = true;
                        break 'outer;
                    }
                }
            }
        }
        if !merged {
            break;
        }
    }
    Some(
        polys
            .into_iter()
            .map(|p| p.into_iter().map(|i| ring[i]).collect())
            .collect(),
    )
}

/// Plane clip (plain `IfcHalfSpaceSolid` / `IfcBoxedHalfSpace`) with a
/// closed-shell guarantee. [`clip_by_plane`] runs first — the historical
/// cut-mode primitive, returned as-is whenever it closes. Its cap is a
/// bare earcut that can drop collinear cap vertices; when the input was
/// index-closed but that left the shell open, the cap is rebuilt by the
/// general construction with `K` = the half-space alone. If that also
/// refuses, the `clip_by_plane` result stands (the pre-GH #194 behaviour).
pub fn clip_plane_closed(
    vertices: &[f32],
    indices: &[u32],
    plane_point: Vec3,
    plane_normal: Vec3,
    eps: f32,
) -> (Vec<f32>, Vec<u32>) {
    let (v, i) = clip_by_plane(vertices, indices, plane_point, plane_normal, eps);
    if i.is_empty()
        || !crate::mesh::qto::is_closed_manifold(indices)
        || crate::mesh::qto::is_closed_manifold(&i)
    {
        return (v, i);
    }
    let n = plane_normal.as_dvec3().normalize_or_zero();
    if n.length_squared() < 0.5 {
        return (v, i);
    }
    let p = plane_point.as_dvec3();
    let pos: Vec<DVec3> = vertices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64))
        .collect();
    let s_max = pos
        .iter()
        .map(|q| (*q - p).dot(n))
        .fold(f64::NEG_INFINITY, f64::max);
    let (e1, e2) = basis(n);
    match subtract_convex_column(
        &pos,
        indices,
        &[],
        e1,
        e2,
        n,
        p,
        s_max,
        (eps as f64).max(1.0e-9),
    ) {
        BoundedClip::Clipped { vertices, indices } => (vertices, indices),
        // Nothing strictly past the band: `clip_by_plane` only shaved
        // on-plane slivers (its band counts ON as removed) and broke the
        // shell doing it — the host is unchanged.
        BoundedClip::Unchanged => (vertices.to_vec(), indices.to_vec()),
        _ => (v, i),
    }
}

/// Route 1 for a NON-convex footprint: point containment of the removed
/// points is not enough (the hull of the removed part could still cross a
/// reflex notch), so additionally require that the footprint boundary
/// misses the 2D bounding box of the removed points entirely. Then the
/// box lies wholly inside the footprint (it contains removed points that
/// are inside), and so does the removed part's projection.
fn ring_edges_miss_removed(
    ring: &[DVec2],
    pos: &[DVec3],
    s: &[f64],
    eps: f64,
    to2: impl Fn(DVec3) -> DVec2,
) -> bool {
    let mut lo = DVec2::splat(f64::INFINITY);
    let mut hi = DVec2::splat(f64::NEG_INFINITY);
    for (v, &sv) in pos.iter().zip(s) {
        if sv > -eps {
            let q = to2(*v);
            lo = lo.min(q);
            hi = hi.max(q);
        }
    }
    let lo = lo - DVec2::splat(eps);
    let hi = hi + DVec2::splat(eps);
    let m = ring.len();
    (0..m).all(|i| !segment_hits_box(ring[i], ring[(i + 1) % m], lo, hi))
}

/// Liang–Barsky: does segment `a→b` touch the axis-aligned box?
fn segment_hits_box(a: DVec2, b: DVec2, lo: DVec2, hi: DVec2) -> bool {
    let d = b - a;
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for (p, q) in [
        (-d.x, a.x - lo.x),
        (d.x, hi.x - a.x),
        (-d.y, a.y - lo.y),
        (d.y, hi.y - a.y),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// Drop duplicate and collinear vertices from a CCW ring.
fn clean_ring(outer: &[glam::Vec2], eps: f64) -> Vec<DVec2> {
    let mut r: Vec<DVec2> = Vec::with_capacity(outer.len());
    for v in outer {
        let q = DVec2::new(v.x as f64, v.y as f64);
        if r.last().is_none_or(|l: &DVec2| (*l - q).length() > eps) {
            r.push(q);
        }
    }
    while r.len() >= 2 && (r[0] - r[r.len() - 1]).length() <= eps {
        r.pop();
    }
    // Collinear removal (relative cross product), repeated until stable.
    loop {
        let m = r.len();
        if m < 3 {
            return r;
        }
        let mut drop = None;
        for i in 0..m {
            let a = r[(i + m - 1) % m];
            let b = r[i];
            let c = r[(i + 1) % m];
            let u = b - a;
            let w = c - b;
            let denom = u.length() * w.length();
            if denom <= 0.0 || (u.perp_dot(w) / denom).abs() < 1.0e-9 && u.dot(w) > 0.0 {
                drop = Some(i);
                break;
            }
        }
        match drop {
            Some(i) => {
                r.remove(i);
            }
            None => return r,
        }
    }
}

fn is_convex_ccw(ring: &[DVec2]) -> bool {
    let m = ring.len();
    (0..m).all(|i| {
        let a = ring[i];
        let b = ring[(i + 1) % m];
        let c = ring[(i + 2) % m];
        (b - a).perp_dot(c - b) > 0.0
    })
}

fn point_in_convex(ring: &[DVec2], q: DVec2, eps: f64) -> bool {
    let m = ring.len();
    (0..m).all(|i| {
        let a = ring[i];
        let b = ring[(i + 1) % m];
        let e = b - a;
        let len = e.length();
        len <= 0.0 || e.perp_dot(q - a) / len >= -eps
    })
}

fn point_in_polygon(ring: &[DVec2], q: DVec2) -> bool {
    let m = ring.len();
    let mut inside = false;
    let mut j = m - 1;
    for i in 0..m {
        let (a, b) = (ring[i], ring[j]);
        if (a.y > q.y) != (b.y > q.y) && q.x < (b.x - a.x) * (q.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// One plane of `K`, outward normal `m` (K on the negative side).
#[derive(Clone, Copy)]
struct KPlane {
    point: DVec3,
    m: DVec3,
}

impl KPlane {
    fn dist(&self, v: DVec3) -> f64 {
        (v - self.point).dot(self.m)
    }
}

/// Boundary vertex of one face polygon of `K`.
#[derive(Clone, Copy)]
struct BVert {
    pos: DVec3,
    /// Base-corner index (footprint vertex) or `None` for the virtual top.
    corner: Option<usize>,
}

struct Face {
    plane: usize,
    /// In-plane basis with `u × w` = the cap's outward normal (`-m`,
    /// pointing into `K`), so CCW in `(u, w)` is CCW seen from outside
    /// the clipped solid.
    u: DVec3,
    w: DVec3,
    o: DVec3,
    bv: Vec<BVert>,
    /// Edge `i` runs `bv[i] → bv[i+1]`; the adjacent plane, `None` = virtual.
    adj: Vec<Option<usize>>,
}

impl Face {
    fn to2(&self, v: DVec3) -> DVec2 {
        let d = v - self.o;
        DVec2::new(d.dot(self.u), d.dot(self.w))
    }
}

#[derive(Clone)]
struct Work {
    pos: Vec<DVec3>,
    lab: Vec<i8>,
    np: usize,
}

impl Work {
    fn label(&self, v: u32, j: usize) -> i8 {
        self.lab[v as usize * self.np + j]
    }
    fn push(&mut self, p: DVec3) -> u32 {
        let id = self.pos.len() as u32;
        self.pos.push(p);
        self.lab.extend(std::iter::repeat_n(UNSET, self.np));
        id
    }
}

#[allow(clippy::too_many_arguments)]
fn subtract_convex_column(
    pos_in: &[DVec3],
    indices: &[u32],
    ring: &[DVec2],
    e1: DVec3,
    e2: DVec3,
    n: DVec3,
    p: DVec3,
    s_max: f64,
    eps: f64,
) -> BoundedClip {
    // ---- topology: use the input as-is when it is index-closed; else
    // weld exact duplicate positions (per-face vertex copies) and retry.
    // Welding an already-closed mesh could collapse distinct vertices
    // that round to the same f32 and break its pairing, so it's a
    // fallback only.
    let raw: Vec<[u32; 3]> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [t[0], t[1], t[2]])
        .collect();
    let (pos, mut tris): (Vec<DVec3>, Vec<[u32; 3]>) =
        if crate::mesh::qto::is_closed_manifold(indices) {
            (pos_in.to_vec(), raw)
        } else {
            let mut remap: HashMap<[u64; 3], u32> = HashMap::with_capacity(pos_in.len());
            let mut pos: Vec<DVec3> = Vec::with_capacity(pos_in.len());
            let mut vmap: Vec<u32> = Vec::with_capacity(pos_in.len());
            for v in pos_in {
                let key = [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
                let id = *remap.entry(key).or_insert_with(|| {
                    pos.push(*v);
                    (pos.len() - 1) as u32
                });
                vmap.push(id);
            }
            let tris: Vec<[u32; 3]> = raw
                .iter()
                .map(|t| {
                    [
                        vmap[t[0] as usize],
                        vmap[t[1] as usize],
                        vmap[t[2] as usize],
                    ]
                })
                .filter(|t| t[0] != t[1] && t[1] != t[2] && t[2] != t[0])
                .collect();
            let flat: Vec<u32> = tris.iter().flatten().copied().collect();
            if !crate::mesh::qto::is_closed_manifold(&flat) {
                return BoundedClip::Failed;
            }
            (pos, tris)
        };
    let vol_in = signed_volume(&pos, &tris);

    // ---- planes of K ----------------------------------------------------
    let m = ring.len();
    let corner3: Vec<DVec3> = ring.iter().map(|q| p + e1 * q.x + e2 * q.y).collect();
    let mut planes: Vec<KPlane> = Vec::with_capacity(m + 1);
    planes.push(KPlane { point: p, m: -n });
    for k in 0..m {
        let a = corner3[k];
        let b = corner3[(k + 1) % m];
        let t = (b - a).normalize();
        planes.push(KPlane {
            point: a,
            m: t.cross(n).normalize(),
        });
    }
    let np = planes.len();

    let mut wk = Work {
        lab: vec![UNSET; pos.len() * np],
        pos,
        np,
    };

    // ---- split by every plane (shared edge cache per pass) --------------
    let mut coplanar_in = vec![false; np];
    for (j, pl) in planes.iter().enumerate() {
        // Vertices within eps of the plane are ON it (no split there). A
        // host face lying entirely ON the plane is resolved by orientation:
        // SAME outward orientation as K's face → its material is inside K,
        // the face goes with no cap behind it (IN); OPPOSITE → it only
        // touches K from outside and stays with no cap in front of it
        // (OUT). Either way no zero-thickness fin is built. Revit
        // boundaries are routinely flush with the wall faces, so this is
        // the common case, not an edge case. Both orientations over the
        // same face region is ambiguous → give up. Decided by coplanar
        // AREA, so zero-area slivers (noise normals) cannot vote.
        let (mut same, mut opp) = (0.0_f64, 0.0_f64);
        for t in &tris {
            let v = [
                wk.pos[t[0] as usize],
                wk.pos[t[1] as usize],
                wk.pos[t[2] as usize],
            ];
            if !v.iter().all(|x| pl.dist(*x).abs() <= eps) {
                continue;
            }
            // Only faces that can overlap K's face region matter.
            let relevant = planes
                .iter()
                .enumerate()
                .all(|(k, pk)| k == j || v.iter().any(|x| pk.dist(*x) < eps));
            if !relevant {
                continue;
            }
            let nrm = (v[1] - v[0]).cross(v[2] - v[0]);
            let a = nrm.dot(pl.m);
            if a > 0.0 {
                same += a;
            } else {
                opp -= a;
            }
        }
        // `nrm` is twice the area. A coplanar patch smaller than the band
        // itself (eps × eps) is below resolution: slivers don't vote.
        let floor = 2.0 * eps * eps;
        let (same, opp) = (
            if same < floor { 0.0 } else { same },
            if opp < floor { 0.0 } else { opp },
        );
        let big = same.max(opp);
        if big > 0.0 && same.min(opp) > 1.0e-6 * big {
            return BoundedClip::Failed;
        }
        coplanar_in[j] = same > opp;
        let mut dist: Vec<f64> = wk.pos.iter().map(|q| pl.dist(*q)).collect();
        for (v, &d) in dist.iter().enumerate() {
            let slot = v * np + j;
            if wk.lab[slot] == UNSET {
                wk.lab[slot] = if d < -eps {
                    IN
                } else if d > eps {
                    OUT
                } else {
                    ON
                };
            }
        }
        let mut cache: HashMap<(u32, u32), u32> = HashMap::new();
        let mut out: Vec<[u32; 3]> = Vec::with_capacity(tris.len() + 16);
        for t in &tris {
            let l = [wk.label(t[0], j), wk.label(t[1], j), wk.label(t[2], j)];
            let n_in = l.iter().filter(|&&x| x == IN).count();
            let n_out = l.iter().filter(|&&x| x == OUT).count();
            if n_in == 0 || n_out == 0 {
                out.push(*t);
                continue;
            }
            // Only IN–OUT edges cross the plane; both ends are more than
            // eps from it, so the split point is a genuine interior point.
            let mut split = |a: u32, b: u32, wk: &mut Work, dist: &mut Vec<f64>| -> u32 {
                let key = if a < b { (a, b) } else { (b, a) };
                if let Some(&x) = cache.get(&key) {
                    return x;
                }
                let (da, db) = (dist[a as usize], dist[b as usize]);
                let t = (da / (da - db)).clamp(0.0, 1.0);
                let pa = wk.pos[a as usize];
                let pb = wk.pos[b as usize];
                let x = wk.push(pa + (pb - pa) * t);
                dist.push(0.0);
                for k in 0..np {
                    let lab = if k == j {
                        ON
                    } else {
                        let (la, lb) = (wk.label(a, k), wk.label(b, k));
                        if la == UNSET || lb == UNSET {
                            UNSET
                        } else if la == IN || lb == IN {
                            IN
                        } else if la == OUT || lb == OUT {
                            OUT
                        } else {
                            ON
                        }
                    };
                    wk.lab[x as usize * np + k] = lab;
                }
                cache.insert(key, x);
                x
            };
            if n_in + n_out == 2 {
                // One vertex ON the plane: a single split of the IN–OUT edge.
                let r = l.iter().position(|&x| x == ON).unwrap();
                let (o, a, b) = (t[r], t[(r + 1) % 3], t[(r + 2) % 3]);
                let x = split(a, b, &mut wk, &mut dist);
                out.push([o, a, x]);
                out.push([o, x, b]);
            } else if n_in == 1 {
                let r = l.iter().position(|&x| x == IN).unwrap();
                let (k, r1, r2) = (t[r], t[(r + 1) % 3], t[(r + 2) % 3]);
                let x1 = split(k, r1, &mut wk, &mut dist);
                let x2 = split(k, r2, &mut wk, &mut dist);
                out.push([k, x1, x2]);
                out.push([x1, r1, r2]);
                out.push([x1, r2, x2]);
            } else {
                let r = l.iter().position(|&x| x == OUT).unwrap();
                let (rr, k1, k2) = (t[r], t[(r + 1) % 3], t[(r + 2) % 3]);
                let x1 = split(k1, rr, &mut wk, &mut dist);
                let x2 = split(k2, rr, &mut wk, &mut dist);
                out.push([k1, k2, x2]);
                out.push([k1, x2, x1]);
                out.push([x2, rr, x1]);
            }
        }
        tris = out;
    }

    // ---- classify: drop triangles inside every plane --------------------
    // Side of a triangle against plane j: IN / OUT if any vertex is; a
    // triangle entirely ON the plane (coplanar host face) takes the
    // plane's coplanar resolution.
    let side = |t: &[u32; 3], j: usize, wk: &Work| -> i8 {
        let l = [wk.label(t[0], j), wk.label(t[1], j), wk.label(t[2], j)];
        if l.contains(&IN) {
            IN
        } else if l.contains(&OUT) {
            OUT
        } else if coplanar_in[j] {
            IN
        } else {
            OUT
        }
    };
    let mut kept: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
    // Dropped triangles by directed edge: the rim's other side.
    let mut dropped_by_edge: HashMap<(u32, u32), [u32; 3]> = HashMap::new();
    let mut dropped = 0usize;
    for t in &tris {
        if (0..np).all(|j| side(t, j, &wk) == IN) {
            dropped += 1;
            for k in 0..3 {
                dropped_by_edge.insert((t[k], t[(k + 1) % 3]), *t);
            }
        } else {
            kept.push(*t);
        }
    }
    if dropped == 0 {
        return BoundedClip::Unchanged;
    }
    if kept.is_empty() {
        return BoundedClip::Clipped {
            vertices: Vec::new(),
            indices: Vec::new(),
        };
    }

    // ---- rim: kept directed edges whose twin was dropped ---------------
    let mut kept_dir: HashSet<(u32, u32)> = HashSet::with_capacity(kept.len() * 3);
    for t in &kept {
        for k in 0..3 {
            kept_dir.insert((t[k], t[(k + 1) % 3]));
        }
    }
    // Cap edges (reverse of rim edges), grouped by the face they lie on.
    let mut face_edges: Vec<Vec<(u32, u32)>> = vec![Vec::new(); np];
    // Rim edges lying along a K edge whose face could not be decided
    // geometrically: (cap edge, the two candidate faces).
    let mut ambiguous: Vec<((u32, u32), [usize; 2])> = Vec::new();
    for t in &kept {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if kept_dir.contains(&(b, a)) {
                continue;
            }
            // The rim edge lies on the plane that separates the kept
            // triangle (OUT side) from the dropped one: both ends ON it.
            let on: Vec<usize> = (0..np)
                .filter(|&j| wk.label(a, j) == ON && wk.label(b, j) == ON && side(t, j, &wk) == OUT)
                .collect();
            match on.as_slice() {
                [j] => face_edges[*j].push((b, a)),
                // The rim runs along a K edge line L: the host surface
                // contains L (Revit anchors the clip plane on a wall
                // corner, so this is common). In the cross-section ⟂ L,
                // K is a wedge between its faces j and k, the dropped
                // triangle D a ray inside the wedge and the host interior
                // lies on D's inner side — so of the two faces exactly one
                // leaves L on D's inner side, and that one carries the cap.
                // Directions use L = m_j × m_k (exact), not the rim edge,
                // which can be a sub-ulp sliver.
                [j, k] => {
                    let (mj, mk) = (planes[*j].m, planes[*k].m);
                    let face = dropped_by_edge.get(&(b, a)).and_then(|td| {
                        let l = mj.cross(mk).normalize_or_zero();
                        let pa = wk.pos[a as usize];
                        let third = td.iter().copied().find(|&v| v != a && v != b)?;
                        let sgn = (pa - wk.pos[b as usize]).dot(l).signum();
                        // D = (b, a, third): outward normal (a−b) × (third−a).
                        let n_d = l.cross(wk.pos[third as usize] - pa) * sgn;
                        let dj = -(mk - mj * mk.dot(mj));
                        let dk = -(mj - mk * mj.dot(mk));
                        let (sj, sk) = (dj.dot(n_d), dk.dot(n_d));
                        let tiny = 1.0e-9 * n_d.length();
                        match (sj < -tiny, sk < -tiny) {
                            (true, false) => Some(*j),
                            (false, true) => Some(*k),
                            _ => None,
                        }
                    });
                    match face {
                        Some(f) => face_edges[f].push((b, a)),
                        None => ambiguous.push(((b, a), [*j, *k])),
                    }
                }
                _ => return BoundedClip::Failed,
            }
        }
    }
    // Undecidable K-edge rim edges (sliver triangles): try the face
    // assignments, first that closes wins. Bounded — more than a handful
    // means the incidence is not a sliver but a real degeneracy.
    if ambiguous.len() > 4 {
        return BoundedClip::Failed;
    }
    let ctx = CapCtx {
        planes: &planes,
        corner3: &corner3,
        n,
        p,
        s_max,
        vol_in,
    };
    for combo in 0u32..(1u32 << ambiguous.len()) {
        let mut fe = face_edges.clone();
        for (bit, (edge, cand)) in ambiguous.iter().enumerate() {
            fe[cand[((combo >> bit) & 1) as usize]].push(*edge);
        }
        if let Some((vertices, indices)) = close_with_caps(wk.clone(), &kept, &fe, &ctx) {
            return BoundedClip::Clipped { vertices, indices };
        }
    }
    BoundedClip::Failed
}

/// Fixed inputs of the cap construction.
struct CapCtx<'a> {
    planes: &'a [KPlane],
    corner3: &'a [DVec3],
    n: DVec3,
    p: DVec3,
    s_max: f64,
    vol_in: f64,
}

/// Build the caps on `K`'s faces for the given rim assignment, assemble
/// with the kept surface, and verify: closed 2-manifold and
/// `0 ≤ volume ≤ input volume`. `None` when any step refuses.
fn close_with_caps(
    mut wk: Work,
    kept: &[[u32; 3]],
    face_edges: &[Vec<(u32, u32)>],
    ctx: &CapCtx,
) -> Option<(Vec<f32>, Vec<u32>)> {
    let (planes, corner3, n, p, s_max, vol_in) =
        (ctx.planes, ctx.corner3, ctx.n, ctx.p, ctx.s_max, ctx.vol_in);
    let np = planes.len();
    let m = corner3.len();
    // ---- faces of K -----------------------------------------------------
    let h = (s_max.max(0.0) + 1.0) * 2.0;
    let mut faces: Vec<Face> = Vec::with_capacity(np);
    {
        // Base: corners CCW seen from +n; edge k is adjacent to side k+1.
        let c = n;
        let (u, w) = basis(c);
        faces.push(Face {
            plane: 0,
            u,
            w,
            o: p,
            bv: (0..m)
                .map(|k| BVert {
                    pos: corner3[k],
                    corner: Some(k),
                })
                .collect(),
            adj: (0..m).map(|k| Some(k + 1)).collect(),
        });
        for k in 0..m {
            let j = k + 1;
            let c = -planes[j].m;
            let (u, w) = basis(c);
            let k1 = (k + 1) % m;
            let prev_side = (k + m - 1) % m + 1;
            let next_side = k1 + 1;
            faces.push(Face {
                plane: j,
                u,
                w,
                o: corner3[k],
                bv: vec![
                    BVert {
                        pos: corner3[k],
                        corner: Some(k),
                    },
                    BVert {
                        pos: corner3[k] + n * h,
                        corner: None,
                    },
                    BVert {
                        pos: corner3[k1] + n * h,
                        corner: None,
                    },
                    BVert {
                        pos: corner3[k1],
                        corner: Some(k1),
                    },
                ],
                adj: vec![Some(prev_side), None, Some(next_side), Some(0)],
            });
        }
    }

    // ---- build caps -----------------------------------------------------
    // A K corner that coincides with a rim vertex (a host vertex or split
    // point ON all three of its planes) IS that vertex — reuse it, or the
    // walk would add a coincident duplicate and open the seam.
    let mut corner_vid: Vec<Option<u32>> = vec![None; m];
    for edges in face_edges {
        for &(a, _) in edges {
            for (k, slot) in corner_vid.iter_mut().enumerate() {
                let prev_side = (k + m - 1) % m + 1;
                if slot.is_none()
                    && wk.label(a, 0) == ON
                    && wk.label(a, prev_side) == ON
                    && wk.label(a, k + 1) == ON
                {
                    *slot = Some(a);
                }
            }
        }
    }
    let mut walked_corner = false;
    let mut cap_tris: Vec<[u32; 3]> = Vec::new();
    // Side faces first so the base knows whether its corners were reached.
    let order: Vec<usize> = (1..np).chain(std::iter::once(0)).collect();
    for &fi in &order {
        let face = &faces[fi];
        let edges = &face_edges[face.plane];
        let loops: Vec<Vec<u32>> = if edges.is_empty() {
            if fi == 0 && walked_corner {
                // No rim on the base plane but the side caps reached its
                // corners: the whole base polygon lies inside the host.
                let mut lp = Vec::with_capacity(m);
                for k in 0..m {
                    lp.push(corner_id(&mut wk, &mut corner_vid, k, corner3));
                }
                vec![lp]
            } else {
                continue;
            }
        } else {
            face_loops(
                &mut wk,
                face,
                edges,
                &mut corner_vid,
                corner3,
                &mut walked_corner,
            )?
        };
        cap_tris.extend(triangulate_face(&wk, face, &loops)?);
    }

    // ---- assemble + verify ----------------------------------------------
    let mut all = kept.to_vec();
    all.extend(cap_tris);
    let flat: Vec<u32> = all.iter().flatten().copied().collect();
    if !crate::mesh::qto::is_closed_manifold(&flat) {
        return None;
    }
    let vol_out = signed_volume(&wk.pos, &all);
    let tol = 1.0e-6 * vol_in.abs().max(1.0e-12) + 1.0e-9;
    if vol_out < -tol || vol_out > vol_in + tol {
        return None;
    }

    // Compact to referenced vertices, f32 out.
    let mut remap: HashMap<u32, u32> = HashMap::with_capacity(flat.len() / 2);
    let mut out_v: Vec<f32> = Vec::new();
    let mut out_i: Vec<u32> = Vec::with_capacity(flat.len());
    for &i in &flat {
        let id = *remap.entry(i).or_insert_with(|| {
            let q = wk.pos[i as usize];
            out_v.push(q.x as f32);
            out_v.push(q.y as f32);
            out_v.push(q.z as f32);
            (out_v.len() / 3 - 1) as u32
        });
        out_i.push(id);
    }
    Some((out_v, out_i))
}

fn basis(c: DVec3) -> (DVec3, DVec3) {
    let helper = if c.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = c.cross(helper).normalize();
    let w = c.cross(u).normalize();
    (u, w)
}

fn corner_id(wk: &mut Work, corner_vid: &mut [Option<u32>], k: usize, corner3: &[DVec3]) -> u32 {
    if let Some(id) = corner_vid[k] {
        return id;
    }
    let id = wk.push(corner3[k]);
    corner_vid[k] = Some(id);
    id
}

fn signed_volume(pos: &[DVec3], tris: &[[u32; 3]]) -> f64 {
    let o = pos.first().copied().unwrap_or(DVec3::ZERO);
    let mut v6 = 0.0;
    for t in tris {
        let a = pos[t[0] as usize] - o;
        let b = pos[t[1] as usize] - o;
        let c = pos[t[2] as usize] - o;
        v6 += a.dot(b.cross(c));
    }
    v6 / 6.0
}

/// Where a chain endpoint sits on the face boundary: `(edge, fraction)`.
fn locate(wk: &Work, face: &Face, v: u32) -> Option<(usize, f64)> {
    let nb = face.bv.len();
    for i in 0..nb {
        let Some(q) = face.adj[i] else { continue };
        if wk.label(v, q) != ON {
            continue;
        }
        let a = face.bv[i].pos;
        let b = face.bv[(i + 1) % nb].pos;
        let e = b - a;
        let l2 = e.length_squared();
        let f = if l2 > 0.0 {
            ((wk.pos[v as usize] - a).dot(e) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        return Some((i, f));
    }
    None
}

/// Chain the cap edges on one face and link the chains along the face
/// boundary into closed loops (outer CCW, holes CW, seen from `face.c`).
fn face_loops(
    wk: &mut Work,
    face: &Face,
    edges: &[(u32, u32)],
    corner_vid: &mut [Option<u32>],
    corner3: &[DVec3],
    walked_corner: &mut bool,
) -> Option<Vec<Vec<u32>>> {
    let mut next: HashMap<u32, u32> = HashMap::with_capacity(edges.len());
    let mut ends: HashSet<u32> = HashSet::with_capacity(edges.len());
    for &(a, b) in edges {
        if next.insert(a, b).is_some() {
            return None;
        }
        ends.insert(b);
    }
    let mut visited: HashSet<u32> = HashSet::with_capacity(edges.len());
    let mut chains: Vec<Vec<u32>> = Vec::new();
    let mut starts: Vec<u32> = edges
        .iter()
        .map(|e| e.0)
        .filter(|a| !ends.contains(a))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    for st in starts {
        let mut ch = vec![st];
        let mut cur = st;
        while let Some(&nx) = next.get(&cur) {
            if !visited.insert(cur) {
                return None;
            }
            ch.push(nx);
            cur = nx;
            if ch.len() > edges.len() + 1 {
                return None;
            }
        }
        chains.push(ch);
    }
    let mut loops: Vec<Vec<u32>> = Vec::new();
    // Closed rim loops lying entirely on this face.
    let mut rest: Vec<u32> = edges
        .iter()
        .map(|e| e.0)
        .filter(|a| !visited.contains(a))
        .collect();
    rest.sort_unstable();
    for st in rest {
        if visited.contains(&st) {
            continue;
        }
        let mut lp = Vec::new();
        let mut cur = st;
        loop {
            if !visited.insert(cur) {
                return None;
            }
            lp.push(cur);
            cur = *next.get(&cur)?;
            if cur == st {
                break;
            }
            if lp.len() > edges.len() {
                return None;
            }
        }
        loops.push(lp);
    }
    if chains.is_empty() {
        return Some(loops);
    }

    // Link open chains along the face boundary (Weiler–Atherton).
    let nb = face.bv.len();
    let entry_loc: Vec<(usize, f64)> = chains
        .iter()
        .map(|c| locate(wk, face, c[0]))
        .collect::<Option<_>>()?;
    let mut used = vec![false; chains.len()];
    for c0 in 0..chains.len() {
        if used[c0] {
            continue;
        }
        let mut lp: Vec<u32> = Vec::new();
        let mut cur = c0;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > chains.len() + 1 {
                return None;
            }
            used[cur] = true;
            for &v in &chains[cur] {
                if lp.last() != Some(&v) {
                    lp.push(v);
                }
            }
            let exit = *chains[cur].last().unwrap();
            // Direct continuation at the same vertex.
            let direct = (0..chains.len()).find(|&k| chains[k][0] == exit && (k == c0 || !used[k]));
            let nxt = if let Some(k) = direct {
                k
            } else {
                let (ie, fe) = locate(wk, face, exit)?;
                let pe = ie as f64 + fe;
                let mut best: Option<(f64, usize)> = None;
                for (k, &(is, fs)) in entry_loc.iter().enumerate() {
                    if k != c0 && used[k] {
                        continue;
                    }
                    let mut d = (is as f64 + fs) - pe;
                    if d < -1.0e-12 {
                        d += nb as f64;
                    }
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, k));
                    }
                }
                let (_, k) = best?;
                let (is, fs) = entry_loc[k];
                // Corners between the exit edge and the entry edge.
                let steps = if is == ie && fs >= fe - 1.0e-12 {
                    0
                } else {
                    let s = (is + nb - ie) % nb;
                    if s == 0 {
                        nb
                    } else {
                        s
                    }
                };
                for st in 0..=steps {
                    face.adj[(ie + st) % nb]?; // virtual edge → give up
                }
                for st in 1..=steps {
                    let bvx = face.bv[(ie + st) % nb];
                    let k_c = bvx.corner?;
                    let id = corner_id(wk, corner_vid, k_c, corner3);
                    *walked_corner = true;
                    if lp.last() != Some(&id) {
                        lp.push(id);
                    }
                }
                k
            };
            if nxt == c0 {
                break;
            }
            cur = nxt;
        }
        if lp.len() >= 2 && lp.first() == lp.last() {
            lp.pop();
        }
        loops.push(lp);
    }
    Some(loops)
}

/// Triangulate one face's loops (outer CCW + holes CW, seen from
/// `face.c`), oriented so every triangle faces `face.c`.
fn triangulate_face(wk: &Work, face: &Face, loops: &[Vec<u32>]) -> Option<Vec<[u32; 3]>> {
    let pts: Vec<Vec<DVec2>> = loops
        .iter()
        .map(|l| l.iter().map(|&v| face.to2(wk.pos[v as usize])).collect())
        .collect();
    let area = |r: &[DVec2]| -> f64 {
        let m = r.len();
        (0..m).map(|i| r[i].perp_dot(r[(i + 1) % m])).sum::<f64>() * 0.5
    };
    let mut outers: Vec<usize> = Vec::new();
    let mut holes: Vec<usize> = Vec::new();
    let mut out: Vec<[u32; 3]> = Vec::new();
    for (i, r) in pts.iter().enumerate() {
        if r.len() < 3 {
            if r.len() == 2 {
                // Two-vertex loop: a zero-width rim sliver. Its two cap
                // edges `a→b`, `b→a` cancel topologically without a face.
                continue;
            }
            return None;
        }
        let perim: f64 = (0..r.len())
            .map(|q| (r[(q + 1) % r.len()] - r[q]).length())
            .sum();
        if area(r).abs() <= 1.0e-6 * perim * perim {
            // Zero-width sliver loop (split points a hair from a host
            // vertex): a fan of zero-area triangles closes it.
            let l = &loops[i];
            for q in 1..l.len() - 1 {
                out.push([l[0], l[q], l[q + 1]]);
            }
            continue;
        }
        if area(r) > 0.0 {
            outers.push(i);
        } else {
            holes.push(i);
        }
    }
    let mut hole_of: Vec<Vec<usize>> = vec![Vec::new(); outers.len()];
    for &hi in &holes {
        // Smallest-area outer containing the hole's first vertex.
        let q = pts[hi][0];
        let mut best: Option<(f64, usize)> = None;
        for (k, &oi) in outers.iter().enumerate() {
            if point_in_polygon(&pts[oi], q) {
                let a = area(&pts[oi]);
                if best.is_none_or(|(ba, _)| a < ba) {
                    best = Some((a, k));
                }
            }
        }
        let (_, k) = best?;
        hole_of[k].push(hi);
    }
    for (k, &oi) in outers.iter().enumerate() {
        let mut flat: Vec<f64> = Vec::new();
        let mut ids: Vec<u32> = Vec::new();
        let mut hole_idx: Vec<usize> = Vec::new();
        for (q, &v) in pts[oi].iter().zip(&loops[oi]) {
            flat.push(q.x);
            flat.push(q.y);
            ids.push(v);
        }
        for &hi in &hole_of[k] {
            hole_idx.push(ids.len());
            for (q, &v) in pts[hi].iter().zip(&loops[hi]) {
                flat.push(q.x);
                flat.push(q.y);
                ids.push(v);
            }
        }
        let tri = earcutr::earcut(&flat, &hole_idx, 2).ok()?;
        let mut polys: Vec<[u32; 3]> = Vec::with_capacity(tri.len() / 3);
        if !tri.is_empty() {
            // earcut emits one consistent winding; orient it to the loop's CCW.
            let mut sum = 0.0;
            for t in tri.as_chunks::<3>().0 {
                let (a, b, c) = (
                    DVec2::new(flat[t[0] * 2], flat[t[0] * 2 + 1]),
                    DVec2::new(flat[t[1] * 2], flat[t[1] * 2 + 1]),
                    DVec2::new(flat[t[2] * 2], flat[t[2] * 2 + 1]),
                );
                sum += (b - a).perp_dot(c - a);
            }
            for t in tri.as_chunks::<3>().0 {
                let (a, b, c) = (ids[t[0]], ids[t[1]], ids[t[2]]);
                polys.push(if sum >= 0.0 { [a, b, c] } else { [a, c, b] });
            }
        }
        let rings: Vec<&[u32]> = std::iter::once(loops[oi].as_slice())
            .chain(hole_of[k].iter().map(|&h| loops[h].as_slice()))
            .collect();
        out.extend(fill_ring_gaps(polys, &rings, |v| {
            face.to2(wk.pos[v as usize])
        })?);
    }
    Some(out)
}

/// earcut filters duplicate and exactly-collinear ring vertices (split
/// points a hair from a host vertex are routine here), so its output can
/// miss ring edges and leave the solid open where the neighbouring faces
/// still reference those vertices. The gap between the triangulation's
/// boundary and the ring is a set of zero-width loops: ring edges earcut
/// never emitted plus, reversed, triangulation boundary edges that are
/// not ring edges. Close each with a fan of (zero-area) triangles so every
/// ring edge appears exactly once. `None` when a gap has real area — the
/// triangulation itself is wrong, not merely filtered.
fn fill_ring_gaps(
    tris: Vec<[u32; 3]>,
    rings: &[&[u32]],
    p2: impl Fn(u32) -> DVec2,
) -> Option<Vec<[u32; 3]>> {
    let mut cnt: HashMap<(u32, u32), i32> = HashMap::new();
    for t in &tris {
        for k in 0..3 {
            *cnt.entry((t[k], t[(k + 1) % 3])).or_insert(0) += 1;
        }
    }
    // Net boundary multiplicity of the triangulation per directed edge.
    let mut bnd: HashMap<(u32, u32), i32> = HashMap::new();
    for (&(u, v), &c) in &cnt {
        let net = c - cnt.get(&(v, u)).copied().unwrap_or(0);
        if net > 0 {
            bnd.insert((u, v), net);
        }
    }
    let mut ring: HashMap<(u32, u32), i32> = HashMap::new();
    for r in rings {
        let m = r.len();
        for q in 0..m {
            let (a, b) = (r[q], r[(q + 1) % m]);
            if a != b {
                *ring.entry((a, b)).or_insert(0) += 1;
            }
        }
    }
    // Gap edges: ring − boundary, plus reversed (boundary − ring).
    let mut gap: Vec<(u32, u32)> = Vec::new();
    for (&e, &c) in &ring {
        let have = bnd.get(&e).copied().unwrap_or(0);
        for _ in have..c {
            gap.push(e);
        }
    }
    for (&(u, v), &c) in &bnd {
        let want = ring.get(&(u, v)).copied().unwrap_or(0);
        for _ in want..c {
            gap.push((v, u));
        }
    }
    let mut out = tris;
    if gap.is_empty() {
        return Some(out);
    }
    gap.sort_unstable();
    // BTreeMap: the loop decomposition (and so the emitted triangles) must
    // not depend on hash order — bitwise parity gates (GH #152).
    let mut next: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
    for &(a, b) in &gap {
        next.entry(a).or_default().push(b);
    }
    let total = gap.len();
    let mut used = 0usize;
    while used < total {
        let start = *next.iter().find(|(_, v)| !v.is_empty())?.0;
        let mut lp = vec![start];
        let mut cur = start;
        loop {
            let nx = next.get_mut(&cur)?.pop()?;
            used += 1;
            if nx == start {
                break;
            }
            lp.push(nx);
            cur = nx;
            if lp.len() > total {
                return None;
            }
        }
        if lp.len() < 3 {
            continue;
        }
        let pts: Vec<DVec2> = lp.iter().map(|&v| p2(v)).collect();
        let m = pts.len();
        let area: f64 = (0..m)
            .map(|i| pts[i].perp_dot(pts[(i + 1) % m]))
            .sum::<f64>()
            * 0.5;
        let perim: f64 = (0..m).map(|i| (pts[(i + 1) % m] - pts[i]).length()).sum();
        if area.abs() > 1.0e-6 * perim * perim {
            return None;
        }
        for q in 1..m - 1 {
            out.push([lp[0], lp[q], lp[q + 1]]);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;

    /// Axis-aligned box `[0,sx]×[0,sy]×[0,sz]` as an outward, index-welded
    /// closed mesh.
    fn boxm(sx: f32, sy: f32, sz: f32) -> (Vec<f32>, Vec<u32>) {
        let v = vec![
            0.0, 0.0, 0.0, sx, 0.0, 0.0, sx, sy, 0.0, 0.0, sy, 0.0, 0.0, 0.0, sz, sx, 0.0, sz, sx,
            sy, sz, 0.0, sy, sz,
        ];
        let i = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        (v, i)
    }

    fn vol(v: &[f32], i: &[u32]) -> f64 {
        let mut s = 0.0;
        for t in i.as_chunks::<3>().0 {
            let p = |k: u32| {
                DVec3::new(
                    v[k as usize * 3] as f64,
                    v[k as usize * 3 + 1] as f64,
                    v[k as usize * 3 + 2] as f64,
                )
            };
            s += p(t[0]).dot(p(t[1]).cross(p(t[2])));
        }
        s / 6.0
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Polygon2D {
        Polygon2D {
            outer: vec![
                Vec2::new(x0, y0),
                Vec2::new(x1, y0),
                Vec2::new(x1, y1),
                Vec2::new(x0, y1),
            ],
            holes: Vec::new(),
        }
    }

    fn clipped(r: BoundedClip) -> (Vec<f32>, Vec<u32>) {
        match r {
            BoundedClip::Clipped { vertices, indices } => (vertices, indices),
            other => panic!("expected Clipped, got {other:?}"),
        }
    }

    /// Wall 10×1×3, remove z > 2 but only for x in [0,4]: a step. Removed
    /// volume 4·1·1 = 4 → 26.
    #[test]
    fn partial_column_notches_the_top() {
        let (v, i) = boxm(10.0, 1.0, 3.0);
        let r = clip_bounded(
            &v,
            &i,
            &rect(-1.0, -1.0, 4.0, 2.0),
            Mat4::IDENTITY,
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::Z,
            1e-3,
        );
        let (ov, oi) = clipped(r);
        assert!(crate::mesh::qto::is_closed_manifold(&oi));
        assert!((vol(&ov, &oi) - 26.0).abs() < 1e-4, "{}", vol(&ov, &oi));
    }

    /// Column fully inside the wall's top: a pocket 2×0.5 wide, 1 deep.
    #[test]
    fn column_inside_host_footprint_cuts_a_pocket() {
        let (v, i) = boxm(10.0, 1.0, 3.0);
        let r = clip_bounded(
            &v,
            &i,
            &rect(3.0, 0.25, 5.0, 0.75),
            Mat4::IDENTITY,
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::Z,
            1e-3,
        );
        let (ov, oi) = clipped(r);
        assert!(crate::mesh::qto::is_closed_manifold(&oi));
        assert!((vol(&ov, &oi) - 29.0).abs() < 1e-4, "{}", vol(&ov, &oi));
    }

    /// Tilted plane + partial column: differential against the Manifold
    /// subtract of the finite cutter the cut-openings path builds.
    #[cfg(feature = "csg")]
    #[test]
    fn partial_columns_match_manifold_oracle() {
        let (v, i) = boxm(10.0, 1.0, 3.0);
        let cases: Vec<(Polygon2D, Vec3, Vec3)> = vec![
            (
                rect(-20.0, -20.0, 5.0, 20.0),
                Vec3::new(0.0, 0.0, 2.0),
                Vec3::new(0.3, 0.0, 1.0).normalize(),
            ),
            (
                rect(2.0, -1.0, 7.0, 0.6),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(0.1, -0.2, 1.0).normalize(),
            ),
            (
                rect(0.0, 0.0, 4.0, 1.0),
                Vec3::new(0.0, 0.0, 2.5),
                Vec3::new(-0.2, 0.0, 1.0).normalize(),
            ),
            (
                // Column along −X from the end face: remove x > 8 above z=1.
                rect(8.0, -5.0, 30.0, 5.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::Z,
            ),
            (
                // Side-on column (normal +Y) through part of the length.
                Polygon2D {
                    outer: vec![
                        Vec2::new(3.0, 0.5),
                        Vec2::new(6.0, 0.5),
                        Vec2::new(6.0, 2.0),
                        Vec2::new(3.0, 2.0),
                    ],
                    holes: Vec::new(),
                },
                Vec3::new(0.0, 0.5, 0.0),
                Vec3::Y,
            ),
        ];
        for (k, (b, pp, nn)) in cases.into_iter().enumerate() {
            // Boundary authored in a frame whose XY plane is ⟂ n.
            let xf = if nn == Vec3::Y {
                Mat4::from_cols(
                    glam::Vec4::new(1.0, 0.0, 0.0, 0.0),
                    glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
                    glam::Vec4::new(0.0, -1.0, 0.0, 0.0),
                    glam::Vec4::new(0.0, 0.0, 0.0, 1.0),
                )
            } else {
                Mat4::IDENTITY
            };
            let got = clip_bounded(&v, &i, &b, xf, pp, nn, 1e-3);
            let (ov, oi) = clipped(got);
            assert!(crate::mesh::qto::is_closed_manifold(&oi), "case {k} open");
            let cutter =
                crate::mesh::halfspace_clip::bounded_halfspace_cutter(&v, &b, xf, pp, nn, 1e-3)
                    .expect("cutter");
            let (mv, mi) =
                crate::geom::csg::subtract(&v, &i, &cutter.0, &cutter.1).expect("manifold");
            let (a, e) = (vol(&ov, &oi), vol(&mv, &mi));
            assert!(
                (a - e).abs() < 1e-3 * e.abs().max(1.0),
                "case {k}: {a} vs manifold {e}"
            );
        }
    }

    /// Seeded randomized differential vs Manifold: rotated rectangular
    /// columns, tilted planes, flush and partial boundaries, one and two
    /// chained clips. Every case must clip (no `Failed`) and match.
    #[cfg(feature = "csg")]
    #[test]
    fn randomized_differential_vs_manifold() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32
        };
        let (bv, bi) = boxm(10.0, 1.0, 3.0);
        let mut failed = 0;
        let mut ran = 0;
        for case in 0..300 {
            let (mut v, mut i) = (bv.clone(), bi.clone());
            let chain = 1 + (case % 2);
            for _ in 0..chain {
                // Flush boundaries half the time (snap to the wall faces).
                let flush = rnd() < 0.5;
                let (y0, y1) = if flush {
                    (0.0, 1.0)
                } else {
                    (-0.5 + rnd() * 0.8, 0.3 + rnd() * 1.2)
                };
                let x0 = -2.0 + rnd() * 8.0;
                let x1 = x0 + 1.0 + rnd() * 8.0;
                let b = rect(x0, y0, x1, y1);
                let n = Vec3::new(rnd() * 0.6 - 0.3, rnd() * 0.2 - 0.1, 1.0).normalize();
                let pp = Vec3::new(0.0, 0.0, 0.5 + rnd() * 2.0);
                let cutter = crate::mesh::halfspace_clip::bounded_halfspace_cutter(
                    &v,
                    &b,
                    Mat4::IDENTITY,
                    pp,
                    n,
                    1e-3,
                );
                let got = clip_bounded(&v, &i, &b, Mat4::IDENTITY, pp, n, 1e-3);
                let (nv, ni) = match got {
                    BoundedClip::Clipped { vertices, indices } => (vertices, indices),
                    BoundedClip::Unchanged => (v.clone(), i.clone()),
                    other => {
                        failed += 1;
                        eprintln!("case {case}: {other:?}");
                        break;
                    }
                };
                ran += 1;
                if let Some(c) = cutter {
                    if let Ok((mv, mi)) = crate::geom::csg::subtract(&v, &i, &c.0, &c.1) {
                        let (a, e) = (vol(&nv, &ni), vol(&mv, &mi));
                        assert!(
                            (a - e).abs() < 2e-3 * e.abs().max(1.0),
                            "case {case}: {a} vs manifold {e}"
                        );
                    }
                }
                assert!(
                    ni.is_empty() || crate::mesh::qto::is_closed_manifold(&ni),
                    "case {case}: open"
                );
                v = nv;
                i = ni;
            }
        }
        assert_eq!(failed, 0, "{failed} failed of {ran}");
    }

    /// Harder randomized differential: rotated + scaled (mm) hosts,
    /// rotated convex k-gon footprints, chains of up to three clips.
    /// Every clip that succeeds must match Manifold and be closed.
    #[cfg(feature = "csg")]
    #[test]
    fn randomized_differential_rotated_kgons_mm() {
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32
        };
        let mut failed = 0;
        let mut ran = 0;
        for case in 0..600 {
            let scale = if case % 3 == 0 { 1000.0 } else { 1.0 };
            let ang = rnd() * std::f32::consts::TAU;
            let rot = Mat4::from_rotation_z(ang);
            let (bv0, bi) = boxm(10.0, 1.0, 3.0);
            let mut v: Vec<f32> = bv0
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|c| {
                    let q = rot.transform_point3(Vec3::new(c[0], c[1], c[2])) * scale;
                    [q.x, q.y, q.z]
                })
                .collect();
            let mut i = bi.clone();
            let chain = 1 + (case % 3);
            for _ in 0..chain {
                let k = 3 + (rnd() * 4.0) as usize;
                let r = 1.0 + rnd() * 5.0;
                let c = Vec2::new(rnd() * 10.0, rnd() * 1.0);
                let a0 = rnd() * std::f32::consts::TAU;
                let outer: Vec<Vec2> = (0..k)
                    .map(|q| {
                        let a = a0 + q as f32 * std::f32::consts::TAU / k as f32;
                        c + Vec2::new(a.cos(), a.sin()) * r
                    })
                    .collect();
                let b = Polygon2D {
                    outer,
                    holes: Vec::new(),
                };
                let xf = Mat4::from_scale(Vec3::splat(scale)) * rot;
                let n = rot
                    .transform_vector3(Vec3::new(rnd() * 0.6 - 0.3, rnd() * 0.2 - 0.1, 1.0))
                    .normalize();
                let pp = Vec3::new(0.0, 0.0, (0.5 + rnd() * 2.0) * scale);
                let cutter =
                    crate::mesh::halfspace_clip::bounded_halfspace_cutter(&v, &b, xf, pp, n, 1e-3);
                let got = clip_bounded(&v, &i, &b, xf, pp, n, 1e-3);
                let (nv, ni) = match got {
                    BoundedClip::Clipped { vertices, indices } => (vertices, indices),
                    BoundedClip::Unchanged => (v.clone(), i.clone()),
                    other => {
                        failed += 1;
                        eprintln!("case {case}: {other:?}");
                        break;
                    }
                };
                ran += 1;
                if let Some(c) = cutter {
                    if let Ok((mv, mi)) = crate::geom::csg::subtract(&v, &i, &c.0, &c.1) {
                        let (a, e) = (vol(&nv, &ni), vol(&mv, &mi));
                        let s3 = (scale as f64).powi(3);
                        assert!(
                            (a - e).abs() < 2e-3 * e.abs().max(s3),
                            "case {case}: {a} vs manifold {e}"
                        );
                    }
                }
                assert!(
                    ni.is_empty() || crate::mesh::qto::is_closed_manifold(&ni),
                    "case {case}: open"
                );
                if ni.is_empty() {
                    break;
                }
                v = nv;
                i = ni;
            }
        }
        // Adversarial chains of three random k-gon clips on rotated mm
        // hosts hit exact incidences (a K edge lying in a host face) the
        // construction refuses rather than guesses at; the caller falls
        // back and marks them. Pinned at ≤ 0.5 %, and never a wrong volume.
        assert!(failed * 200 <= ran, "{failed} failed of {ran}");
    }

    /// GH #194 fixture geometry (Snowdon wall, feet): the boundary is
    /// flush with three wall faces and crosses the fourth.
    #[test]
    fn flush_three_sides_partial_fourth() {
        let (v0, i) = boxm(59.75, 2.0, 11.5);
        let v: Vec<f32> = v0
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[1] - 1.0, c[2]])
            .collect();
        let b = rect(0.0, -0.3645833, 59.75, 1.0);
        let (ov, oi) = clipped(clip_bounded(
            &v,
            &i,
            &b,
            Mat4::IDENTITY,
            Vec3::new(0.0, -0.3645833, 11.0),
            Vec3::Z,
            1e-3,
        ));
        let want = 59.75 * 2.0 * 11.5 - 59.75 * 1.3645833 * 0.5;
        assert!(crate::mesh::qto::is_closed_manifold(&oi));
        assert!(
            (vol(&ov, &oi) - want).abs() < 1e-2,
            "{} vs {want}",
            vol(&ov, &oi)
        );
    }

    /// The exact extractor inputs of `tests/fixtures/clip_single_pbhs_194.ifc`.
    #[test]
    fn snowdon_single_clip_exact_inputs() {
        let v: Vec<f32> = vec![
            59.75, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 59.75, -1.0, 0.0, 59.75, 1.0, 11.5,
            0.0, 1.0, 11.5, 0.0, -1.0, 11.5, 59.75, -1.0, 11.5,
        ];
        let i: Vec<u32> = vec![
            6, 7, 4, 2, 0, 3, 4, 5, 6, 0, 2, 1, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        let xf = Mat4::from_cols(
            glam::Vec4::new(0.0, -1.0, 0.0, 0.0),
            glam::Vec4::new(-1.0, 0.0, 0.0, 0.0),
            glam::Vec4::new(0.0, 0.0, -1.0, 0.0),
            glam::Vec4::new(0.0, -0.364_583_34, 11.0, 1.0),
        );
        let b = Polygon2D {
            outer: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(-1.364_583_4, 0.0),
                Vec2::new(-1.364_583_4, -59.75),
                Vec2::new(0.0, -59.75),
            ],
            holes: Vec::new(),
        };
        let (ov, oi) = clipped(clip_bounded(
            &v,
            &i,
            &b,
            xf,
            Vec3::new(0.0, -0.364_583_34, 11.0),
            Vec3::new(0.0, 8.742278e-8, 1.0),
            1e-3,
        ));
        let want = 59.75 * 2.0 * 11.5 - 59.75 * 1.364_583_3 * 0.5;
        assert!(crate::mesh::qto::is_closed_manifold(&oi));
        assert!(
            (vol(&ov, &oi) - want).abs() < 1e-2,
            "{} vs {want}",
            vol(&ov, &oi)
        );
    }

    /// Real G55 inputs (ARK walls, mm) captured at the extractor. Each
    /// must clip in pure Rust and match Manifold's `host − finite cutter`
    /// up to the cutter's `eps` keep-side margin.
    #[cfg(feature = "csg")]
    fn g55_case(v: &[f32], b: &[[f32; 2]], x: [f32; 16], p: [f32; 3], n: [f32; 3]) -> (f64, f64) {
        let i: Vec<u32> = vec![
            6, 7, 4, 2, 0, 3, 4, 5, 6, 0, 2, 1, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        let bp = Polygon2D {
            outer: b.iter().map(|q| Vec2::new(q[0], q[1])).collect(),
            holes: Vec::new(),
        };
        let (xf, pp, nn) = (Mat4::from_cols_array(&x), Vec3::from(p), Vec3::from(n));
        let (ov, oi) = clipped(clip_bounded(v, &i, &bp, xf, pp, nn, 1e-3));
        assert!(crate::mesh::qto::is_closed_manifold(&oi));
        let c = crate::mesh::halfspace_clip::bounded_halfspace_cutter(v, &bp, xf, pp, nn, 1e-3)
            .unwrap();
        let (mv, mi) = crate::geom::csg::subtract(v, &i, &c.0, &c.1).unwrap();
        (vol(&ov, &oi), vol(&mv, &mi))
    }

    /// G55_ARK wall `3v6xHsj_j7QOQXr1adee4I`: the clip plane is anchored on
    /// the wall's corner, so a K edge (base ∩ side) lies IN the host's
    /// front face — the rim runs along it and the two-face rule must pick
    /// the face whose region enters the host.
    #[cfg(feature = "csg")]
    #[test]
    fn g55_k_edge_in_host_face() {
        let v = [
            1582.972, 75.0, 0.0, 0.0, 75.0, 0.0, 0.0, -75.0, 0.0, 1582.972, -75.0, 0.0, 1582.972,
            75.0, 700.0, 0.0, 75.0, 700.0, 0.0, -75.0, 700.0, 1582.972, -75.0, 700.0,
        ];
        let (got, mf) = g55_case(
            &v,
            &[
                [0.0, 0.0],
                [0.0, 600.0],
                [-129.32231, 600.0],
                [-129.32231, 0.0],
            ],
            [
                -0.15643446,
                -0.98768836,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                -0.98768836,
                0.15643446,
                0.0,
                0.0,
                1562.7416,
                -75.0,
                100.0,
                1.0,
            ],
            [1562.7416, -75.0, 100.0],
            [0.98768836, -0.15643437, 0.0],
        );
        assert!((got - mf).abs() / mf < 1e-6, "{got} vs manifold {mf}");
    }

    /// G55_ARK wall `3POXqHuM96DwZzM2c25Gxg` / `…Gxl`: an L-shaped (non-convex)
    /// boundary, flush with the wall faces — convex decomposition.
    #[cfg(feature = "csg")]
    #[test]
    fn g55_nonconvex_boundary() {
        let v = [
            3354.5503, 160.0, 0.0, 0.0, 160.0, 0.0, 0.0, -160.0, 0.0, 3354.5503, -160.0, 0.0,
            3354.5503, 160.0, 3600.0, 0.0, 160.0, 3600.0, 0.0, -160.0, 3600.0, 3354.5503, -160.0,
            3600.0,
        ];
        let (got, mf) = g55_case(
            &v,
            &[
                [0.0, 0.0],
                [438.51013, 0.0],
                [438.51013, 320.0],
                [0.0, 320.0],
                [-2916.04, 320.0],
                [-2916.04, 160.0],
                [0.0, 160.0],
            ],
            [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 2916.04, -160.0,
                3570.0, 1.0,
            ],
            [2916.04, -160.0, 3570.0],
            [-8.742278e-8, 0.0, 1.0],
        );
        assert!((got - mf).abs() / mf < 1e-6, "{got} vs manifold {mf}");
    }

    /// Oversized boundary → identical to the plane clip.
    #[test]
    fn covering_column_is_the_plane_clip() {
        let (v, i) = boxm(10.0, 1.0, 3.0);
        let r = clip_bounded(
            &v,
            &i,
            &rect(-100.0, -100.0, 100.0, 100.0),
            Mat4::IDENTITY,
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::Z,
            1e-3,
        );
        let (ov, oi) = clipped(r);
        let (pv, pi) = clip_by_plane(&v, &i, Vec3::new(0.0, 0.0, 2.0), Vec3::Z, 1e-3);
        assert_eq!(ov, pv);
        assert_eq!(oi, pi);
    }

    /// Column misses the host → unchanged.
    #[test]
    fn column_missing_host_is_unchanged() {
        let (v, i) = boxm(10.0, 1.0, 3.0);
        let r = clip_bounded(
            &v,
            &i,
            &rect(20.0, 20.0, 30.0, 30.0),
            Mat4::IDENTITY,
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::Z,
            1e-3,
        );
        assert!(matches!(r, BoundedClip::Unchanged), "{r:?}");
    }

    /// Chained partial columns (clip of clip, non-convex host).
    #[test]
    fn chained_partial_columns_compose() {
        let (v, i) = boxm(10.0, 1.0, 3.0);
        let (v1, i1) = clipped(clip_bounded(
            &v,
            &i,
            &rect(-1.0, -1.0, 4.0, 2.0),
            Mat4::IDENTITY,
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::Z,
            1e-3,
        ));
        let (v2, i2) = clipped(clip_bounded(
            &v1,
            &i1,
            &rect(6.0, -1.0, 11.0, 2.0),
            Mat4::IDENTITY,
            Vec3::new(0.0, 0.0, 1.5),
            Vec3::Z,
            1e-3,
        ));
        assert!(crate::mesh::qto::is_closed_manifold(&i2));
        // 30 − 4 − 4·1·1.5 = 20
        assert!((vol(&v2, &i2) - 20.0).abs() < 1e-4, "{}", vol(&v2, &i2));
    }

    /// Footprint edge coincident with the host end face (Revit-typical).
    #[test]
    fn coincident_boundary_edge_stays_closed() {
        let (v, i) = boxm(10.0, 1.0, 3.0);
        let (ov, oi) = clipped(clip_bounded(
            &v,
            &i,
            &rect(0.0, 0.0, 4.0, 1.0),
            Mat4::IDENTITY,
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::Z,
            1e-3,
        ));
        assert!(crate::mesh::qto::is_closed_manifold(&oi));
        assert!((vol(&ov, &oi) - 26.0).abs() < 1e-3, "{}", vol(&ov, &oi));
    }

    /// Non-convex (L-shaped) footprint that does not cover: convex
    /// decomposition + sequential column subtraction, checked against
    /// Manifold with the same finite cutter.
    #[cfg(feature = "csg")]
    #[test]
    fn nonconvex_partial_decomposes() {
        let (v, i) = boxm(10.0, 1.0, 3.0);
        let l = Polygon2D {
            outer: vec![
                Vec2::new(2.0, -1.0),
                Vec2::new(6.0, -1.0),
                Vec2::new(6.0, 0.5),
                Vec2::new(4.0, 0.5),
                Vec2::new(4.0, 2.0),
                Vec2::new(2.0, 2.0),
            ],
            holes: Vec::new(),
        };
        let pp = Vec3::new(0.0, 0.0, 2.0);
        let (ov, oi) = clipped(clip_bounded(&v, &i, &l, Mat4::IDENTITY, pp, Vec3::Z, 1e-3));
        assert!(crate::mesh::qto::is_closed_manifold(&oi));
        let c = crate::mesh::halfspace_clip::bounded_halfspace_cutter(
            &v,
            &l,
            Mat4::IDENTITY,
            pp,
            Vec3::Z,
            1e-3,
        )
        .unwrap();
        let (mv, mi) = crate::geom::csg::subtract(&v, &i, &c.0, &c.1).unwrap();
        // 30 − (4·0.5 + 2·0.5)·1 = 27
        assert!((vol(&ov, &oi) - 27.0).abs() < 1e-4, "{}", vol(&ov, &oi));
        // The finite CSG cutter starts `eps` behind the plane (its
        // clean-overlap margin), so Manifold removes an extra
        // eps × footprint-in-host (0.003 here); the pure-Rust clip is exact.
        assert!(
            (vol(&ov, &oi) - vol(&mv, &mi) - 3.0e-3).abs() < 1e-4,
            "{} vs manifold {}",
            vol(&ov, &oi),
            vol(&mv, &mi)
        );
    }
}
