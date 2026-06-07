//! Apply CSG opening cuts to a `ProductMesh` in place.
//!
//! Closes the viewer-integrator's P0 #1 (GH #20). Two patterns are
//! covered, each by its own entry point:
//!
//! * **In-representation booleans** — `IfcBooleanClippingResult(host,
//!   void)`. ifcfast's extractor already tags every triangle's
//!   provenance in `ProductMesh.segments` (`"boolean_first_operand|..."`
//!   for the host, `"boolean_second_operand|..."` for the cutter), so
//!   the cut is purely consumer-side: see [`apply`].
//!
//! * **Cross-product `IfcRelVoidsElement` openings** — a solid
//!   `IfcWall` whose own representation is a plain `IfcExtrudedAreaSolid`
//!   with a separately-modelled `IfcOpeningElement` linked via
//!   `IfcRelVoidsElement`. Both products mesh independently; the cut
//!   needs the indexer's relationship arrays plus a stream-time buffer
//!   so opening meshes can fold into their host once both arrive:
//!   see [`CrossProductCut`].
//!
//! Reveal-all stance preserved: both paths are opt-in per call. The
//! substrate writer never invokes them, so `instances.parquet` /
//! `representations.parquet` keep their operand-by-operand fidelity.

use std::collections::{HashMap, HashSet};

use glam::Vec3;

use crate::entity_table::EntityTable;
use crate::geom::csg::{self, CsgKernelError};
use crate::mesh::{cut_validate, halfspace_clip, InstancePart, MeshSegment, ProductMesh};
#[cfg(feature = "prism-csg-fast")]
use crate::mesh::BoundedHalfspacePayload;

// The pure-data outcome + counter types moved to `crate::mesh::cut_stats`
// (GH #61) so mesh / prism builds without `csg` can still carry the
// counters. Re-export them here so existing `cut_openings::{Outcome,
// UnsupportedReason, CutOpeningsStats}` paths — including the csg-gated
// integration + proptest suites — keep resolving unchanged.
pub use crate::mesh::cut_stats::{CutOpeningsStats, Outcome, UnsupportedReason};

/// Apply opening cuts to `mesh` in place. Returns the per-product
/// outcome so callers can aggregate stats.
///
/// On `Outcome::Cut`, `mesh.vertices` / `mesh.indices` /
/// `mesh.segments` are replaced with the net solid; `mesh.parts` is
/// cleared because the cut destroys the per-fragment instance dedup
/// (a wall-minus-this-specific-window is unique to that wall).
///
/// On `Outcome::Passthrough` or `Outcome::Fallback`, the mesh is
/// untouched.
pub fn apply(mesh: &mut ProductMesh, unit_scale: f32) -> Outcome {
    let (host_segs, cutter_segs) = partition_segments(&mesh.segments);

    if cutter_segs.is_empty() {
        // No DIFFERENCE cutters. If the product carries a UNION /
        // INTERSECTION boolean operand (W4), the mesh stays reveal-all
        // (unchanged) but we surface the typed reason instead of a
        // plain passthrough — the net solid we'd ideally produce
        // (union / intersection) is not computed.
        return match detect_unsupported_boolean_op(&mesh.segments) {
            Some(reason) => Outcome::Unsupported(reason),
            None => Outcome::Passthrough,
        };
    }

    // Build the host mesh as the concatenation of all non-cutter
    // segments. For a typical wall with `IfcBooleanClippingResult` of
    // one extrusion and one void, that's just the single
    // `"boolean_first_operand|extrusion"` segment.
    let mut host = match assemble_submesh(&mesh.vertices, &mesh.indices, &host_segs) {
        Some(h) => h,
        // No host triangles at all — nothing meaningful to cut.
        // Treat as passthrough so the caller doesn't lose the cutter
        // geometry. (Pathological case: shouldn't fire on real files.)
        None => return Outcome::Passthrough,
    };

    // Partition cutters into half-space slabs and real solid cutters.
    // Half-spaces don't go through Manifold (GH #39 — Manifold's
    // boolean propagates a bbox-derived tolerance from oversized
    // half-space cutters and collapses the result to empty); instead
    // we clip the host directly against the slab's plane via
    // `mesh::halfspace_clip`. Solid cutters (real opening
    // extrusions, csg primitives) still go through `subtract_many`.
    let mut halfspace_planes: Vec<(Vec3, Vec3)> = Vec::new();
    let mut solid_cutters: Vec<(Vec<f32>, Vec<u32>)> = Vec::new();
    for seg in &cutter_segs {
        let Some(submesh) = assemble_submesh(&mesh.vertices, &mesh.indices, &[*seg]) else {
            continue;
        };
        if is_halfspace_cutter(&seg.source) {
            // Derive the plane (point on plane + agreement-direction
            // normal) from the thin slab geometry. The slab's first
            // triangle is a top-cap triangle whose CCW winding gives
            // a normal in the agreement direction. The slab's
            // centroid is essentially on the plane (the slab is
            // ≤ 1 mm thick).
            if let Some(plane) = derive_plane_from_slab(&submesh.0, &submesh.1) {
                halfspace_planes.push(plane);
            }
        } else {
            solid_cutters.push(submesh);
        }
    }

    if halfspace_planes.is_empty() && solid_cutters.is_empty() {
        return Outcome::Passthrough;
    }

    // ---- W6 bounded polygonal-halfspace fast-path (prism-csg-fast). ----
    // Reduce the host to a prism along the bounded cutters' shared sweep
    // axis ONCE, then evaluate EVERY carried `BoundedHalfspacePayload`
    // against that ORIGINAL prism — never against an already-cut host
    // (GH #64 #1: chaining bounded cuts let the 2nd+ cutter fail its
    // prism check and silently fall back to the over-aggressive infinite
    // clip). Tight, axis-parallel, host-crossing cutters are subtracted
    // by a region-decomposition of the footprint (one prism per disjoint
    // kept region — GH #64 #4: no coincident internal caps), instead of
    // the F6 infinite-plane clip. Each handled cutter's slab-derived
    // plane is dropped from `halfspace_planes` so the generic clip below
    // does NOT double-cut it. Non-tight / non-axis / non-reducible
    // payloads keep their plane for the infinite clip (correct there: an
    // over-sized boundary does not constrain the cut). Default builds
    // (feature off) skip this entirely and stay byte-identical.
    #[cfg(feature = "prism-csg-fast")]
    {
        let bounded = std::mem::take(&mut mesh.bounded_halfspaces);
        if let Some((new_v, new_i, handled)) =
            bounded_halfspace::try_bounded_cut_multi(&host, &bounded)
        {
            host = (new_v, new_i);
            for &i in &handled {
                drop_matching_plane(&mut halfspace_planes, &bounded[i]);
            }
        }
    }

    // Unit-aware "on-plane" tolerance (W3 / F2): a physical 1 mm in the
    // model's source units, so mm / imperial files clip at the same
    // physical scale as the metre files the clipper was tuned on.
    let on_plane_eps = cut_validate::on_plane_eps(unit_scale);

    // Apply half-space clips sequentially. Each clip reduces the
    // host; if the host empties part-way we're done — the remaining
    // half-spaces and solid cutters would just confirm the empty.
    for (plane_point, plane_normal) in &halfspace_planes {
        if host.1.is_empty() {
            break;
        }
        let (new_v, new_i) = halfspace_clip::clip_by_plane(
            &host.0,
            &host.1,
            *plane_point,
            *plane_normal,
            on_plane_eps,
        );
        host = (new_v, new_i);
    }

    // Run remaining solid cutters (real opening extrusions, etc.)
    // through Manifold. If the host has been emptied by the
    // half-space clips alone, skip the boolean step entirely.
    let (final_v, final_i) = if solid_cutters.is_empty() {
        host
    } else if host.1.is_empty() {
        host
    } else {
        let cutter_refs: Vec<(&[f32], &[u32])> = solid_cutters
            .iter()
            .map(|(v, i)| (v.as_slice(), i.as_slice()))
            .collect();
        match csg::subtract_many(&host.0, &host.1, &cutter_refs) {
            Ok(out) => out,
            Err(_e) => return classify_subtract_failure(&host.1, &solid_cutters),
        }
    };

    let triangle_count = (final_i.len() / 3) as u32;
    mesh.vertices = final_v;
    mesh.indices = final_i;
    mesh.segments = vec![MeshSegment {
        index_start: 0,
        index_count: triangle_count * 3,
        source: "cut_openings".to_string(),
    }];
    // The per-fragment instance dedup payload is no longer valid
    // after a cut — clear it. Substrate writers don't invoke this
    // path; consumers that care about parts (only ParquetSink today)
    // don't see cut meshes.
    mesh.parts.clear();
    Outcome::Cut
}

/// Recognise a half-space cutter segment by its source-tag chain.
/// Tags are emitted by `mesh::boolean::halfspace_solid` and
/// `polygonal_bounded_halfspace` with an `:agree` / `:disagree`
/// suffix; the suffix doesn't affect cut_openings — the plane
/// geometry (slab's first-triangle normal) already encodes the
/// agreement direction in world coords.
fn is_halfspace_cutter(source: &str) -> bool {
    chain_contains(source, "halfspace_plane:agree")
        || chain_contains(source, "halfspace_plane:disagree")
        || chain_contains(source, "halfspace_bounded:agree")
        || chain_contains(source, "halfspace_bounded:disagree")
}

/// Derive `(plane_point, plane_normal_into_halfspace)` from a thin
/// half-space slab's geometry. The slab is built by
/// `boolean::halfspace_solid` / `polygonal_bounded_halfspace` as a
/// one-sided extrusion of a polygon, with the first triangle a
/// top-cap CCW triangle whose normal points along the agreement
/// direction. Returns `None` if the slab is degenerate (zero-area
/// first triangle or empty vertex buffer).
fn derive_plane_from_slab(vertices: &[f32], indices: &[u32]) -> Option<(Vec3, Vec3)> {
    if indices.len() < 3 || vertices.len() < 9 {
        return None;
    }
    let i0 = indices[0] as usize;
    let i1 = indices[1] as usize;
    let i2 = indices[2] as usize;
    if (i0 * 3 + 2).max(i1 * 3 + 2).max(i2 * 3 + 2) >= vertices.len() {
        return None;
    }
    let v0 = Vec3::new(
        vertices[i0 * 3],
        vertices[i0 * 3 + 1],
        vertices[i0 * 3 + 2],
    );
    let v1 = Vec3::new(
        vertices[i1 * 3],
        vertices[i1 * 3 + 1],
        vertices[i1 * 3 + 2],
    );
    let v2 = Vec3::new(
        vertices[i2 * 3],
        vertices[i2 * 3 + 1],
        vertices[i2 * 3 + 2],
    );
    let raw_normal = (v1 - v0).cross(v2 - v0);
    if raw_normal.length_squared() < 1e-12 {
        return None;
    }
    let agree = raw_normal.normalize();

    // Plane point: centroid of all slab vertices. The slab is thin
    // (≤ 1 mm in the agree direction), so the centroid is on (or
    // within half a thickness of) the plane — well below
    // building-element tolerance.
    let mut sum = Vec3::ZERO;
    let n = (vertices.len() / 3) as f32;
    if n < 4.0 {
        return None;
    }
    for chunk in vertices.chunks_exact(3) {
        sum += Vec3::new(chunk[0], chunk[1], chunk[2]);
    }
    let plane_point = sum / n;
    Some((plane_point, agree))
}

/// Remove from `planes` the slab-derived plane that matches `payload`'s
/// cutting plane (same point + normal, within a generous tolerance), so
/// the generic infinite-plane clip does not re-cut a cutter the W6
/// bounded path already handled. Removes at most one entry.
#[cfg(feature = "prism-csg-fast")]
fn drop_matching_plane(planes: &mut Vec<(Vec3, Vec3)>, payload: &BoundedHalfspacePayload) {
    // 1 mm point tolerance (working frame is near-origin) + 0.5° normal
    // tolerance. The slab plane point is the slab centroid (offset by
    // ≤½ slab thickness from the payload plane along the normal), so a
    // mm-scale point tolerance is comfortably loose without risking a
    // false match across two genuinely-distinct cutters.
    const PT_EPS: f32 = 2.0;
    const N_DOT_MIN: f32 = 0.9999;
    if let Some(idx) = planes.iter().position(|(pp, pn)| {
        pn.dot(payload.plane_normal) >= N_DOT_MIN
            && (*pp - payload.plane_point).dot(payload.plane_normal).abs() <= PT_EPS
    }) {
        planes.remove(idx);
    }
}

/// W6 bounded polygonal-halfspace cut (prism-csg-fast). Pure-Rust 2D
/// reduction of `IfcPolygonalBoundedHalfSpace` cutters: when each
/// boundary is tight and the host is a prism along the shared plane
/// normal, the kept solid is a *region-decomposition* of the host
/// footprint into disjoint columns, each swept over the sweep interval
/// the bounded cutters leave intact. No CSG kernel; reuses `polygon_bool`
/// + `extrude_polygon`.
///
/// Multi-cutter correctness (GH #64 #1): all cutters are evaluated
/// against the ORIGINAL host prism, never against an already-cut mesh —
/// chaining 3D cuts broke the prism reduction for the 2nd+ cutter and
/// dropped it to the over-cutting infinite clip.
///
/// Interface correctness (GH #64 #4): the output is a set of
/// footprint-DISJOINT prisms (the `difference`/`intersection` partition),
/// so no two prisms share a coincident cap — the old two-glued-bands
/// construction produced internal double faces over the host−boundary
/// region.
#[cfg(feature = "prism-csg-fast")]
mod bounded_halfspace {
    use super::*;
    use crate::mesh::extrusion::{extrude_polygon, LocalMesh};
    use crate::mesh::polygon_bool::{self, Shape};
    use crate::mesh::profile::Polygon2D;
    use glam::{Mat4, Vec2, Vec4};

    /// Sweep coordinates of host vertices must cluster onto exactly two
    /// planes (the two prism caps) within this fraction of the host's
    /// total sweep extent for the reduction to apply.
    const CAP_CLUSTER_FRAC: f32 = 0.02;
    /// A boundary is *tight* only if `host − boundary` exceeds this
    /// fraction of the host footprint area. Below it the boundary
    /// effectively contains the host (Revit's oversized convention) and
    /// the well-tested infinite-plane clip is the correct result. A
    /// relative threshold also absorbs the sub-area slivers i_overlay can
    /// leave at millimetre scale.
    const TIGHT_AREA_FRAC: f64 = 0.005;
    /// Two normals count as the same sweep axis when |dot| ≥ this.
    const AXIS_DOT_MIN: f32 = 0.9999;

    /// The host reduced to a single straight prism swept along `n`,
    /// computed ONCE and shared across every bounded cutter.
    struct HostPrism {
        e1: Vec3,
        e2: Vec3,
        n: Vec3,
        origin: Vec3,
        s_min: f32,
        s_max: f32,
        fp: Polygon2D,
    }

    /// One kept prism in the region-decomposition: a footprint region
    /// (disjoint from every other band's footprint) swept over
    /// `[s_lo, s_hi]` in the prism's sweep coordinate.
    struct Band {
        fp: Shape,
        s_lo: f32,
        s_hi: f32,
    }

    /// A bounded cutter that engages the 2D path: tight, axis-parallel to
    /// the host prism, and crossing the host sweep span.
    struct Engaged {
        /// Index back into the caller's payload slice (so `apply` can drop
        /// the matching slab plane from the infinite-clip list).
        idx: usize,
        /// Boundary footprint in the prism's `(e1, e2)` frame.
        boundary: Shape,
        /// Plane crossing in the prism's sweep coordinate.
        s_p: f32,
        /// `true` if the subtracted side is `s > s_p` (cutter normal
        /// parallel to the prism axis), `false` if `s < s_p` (anti-parallel).
        removes_above: bool,
    }

    /// Attempt the bounded cut over all carried `payloads`. Returns the
    /// new host `(vertices, indices)` plus the indices of the payloads
    /// actually handled in 2D (their slab planes must be dropped from the
    /// infinite-clip list), or `None` when nothing engaged — host not
    /// reducible to a prism along the reference axis, or every cutter
    /// non-tight / off-axis / non-crossing — in which case `apply` leaves
    /// the host untouched and every plane flows to the infinite clip.
    pub(super) fn try_bounded_cut_multi(
        host: &(Vec<f32>, Vec<u32>),
        payloads: &[BoundedHalfspacePayload],
    ) -> Option<(Vec<f32>, Vec<u32>, Vec<usize>)> {
        if payloads.is_empty() || host.1.is_empty() {
            return None;
        }

        // Reference frame from the first payload. Cutters whose normal is
        // not parallel to this axis are left to the infinite-plane
        // fallback (a wall clipped by two non-parallel bounded halfspaces
        // is rare; the common case is one product, one shared axis).
        let prism = reduce_host_to_prism(host, &payloads[0])?;

        let mut engaged: Vec<Engaged> = Vec::new();
        for (idx, p) in payloads.iter().enumerate() {
            let ni = p.plane_normal.normalize_or_zero();
            let d = ni.dot(prism.n);
            if d.abs() < AXIS_DOT_MIN {
                continue; // different sweep axis → fallback
            }
            let boundary_fp =
                project_boundary(&p.boundary, &p.boundary_xform, prism.origin, prism.e1, prism.e2);
            if boundary_fp.outer.len() < 3 {
                continue;
            }
            if !is_tight_boundary(&prism.fp, &boundary_fp) {
                continue; // oversized boundary → infinite clip is correct
            }
            let s_p = (p.plane_point - prism.origin).dot(prism.n);
            if s_p <= prism.s_min + 1e-4 || s_p >= prism.s_max - 1e-4 {
                continue; // plane does not cross the host → fallback
            }
            engaged.push(Engaged {
                idx,
                boundary: polygon_bool::shape_from_polygon2d(&boundary_fp),
                s_p,
                removes_above: d > 0.0,
            });
        }
        if engaged.is_empty() {
            return None;
        }

        // Region-decompose. Start from the whole footprint at full sweep
        // height; for each engaged cutter split every band into the part
        // OUTSIDE its boundary (interval unchanged) and the part INSIDE
        // (interval clamped to the kept side of the plane). `difference`
        // and `intersection` partition each band into disjoint pieces, so
        // all bands stay footprint-disjoint — each extrudes to one prism
        // with no coincident internal caps.
        let host_area = polygon_area(&prism.fp.outer).abs() as f64;
        let min_area = (host_area * 1e-6).max(1e-9);
        let host_shape = polygon_bool::shape_from_polygon2d(&prism.fp);
        let mut bands = vec![Band {
            fp: host_shape,
            s_lo: prism.s_min,
            s_hi: prism.s_max,
        }];
        for e in &engaged {
            let mut next: Vec<Band> = Vec::with_capacity(bands.len() + 1);
            for band in &bands {
                // Outside this boundary: cutter doesn't touch it.
                for sh in polygon_bool::difference(&band.fp, std::slice::from_ref(&e.boundary)) {
                    if shape_area(&sh) > min_area {
                        next.push(Band {
                            fp: sh,
                            s_lo: band.s_lo,
                            s_hi: band.s_hi,
                        });
                    }
                }
                // Inside this boundary: clamp the interval to the kept side.
                let (lo, hi) = if e.removes_above {
                    (band.s_lo, band.s_hi.min(e.s_p))
                } else {
                    (band.s_lo.max(e.s_p), band.s_hi)
                };
                if hi - lo > 1e-4 {
                    for sh in polygon_bool::intersection(&band.fp, std::slice::from_ref(&e.boundary)) {
                        if shape_area(&sh) > min_area {
                            next.push(Band { fp: sh, s_lo: lo, s_hi: hi });
                        }
                    }
                }
            }
            bands = next;
            if bands.is_empty() {
                break; // host fully consumed by the cutters
            }
        }

        // Extrude every surviving band along n, back into the working frame.
        let basis = basis_to_frame(prism.e1, prism.e2, prism.n, prism.origin);
        let mut out = LocalMesh::new();
        for band in &bands {
            let poly = polygon_bool::polygon2d_from_shape(&band.fp);
            if poly.outer.len() < 3 {
                continue;
            }
            let band_origin = translate_along_axis(&basis, band.s_lo);
            let sub = extrude_polygon(&poly, Vec3::Z, band.s_hi - band.s_lo, band_origin);
            append(&mut out, &sub);
        }

        let handled: Vec<usize> = engaged.iter().map(|e| e.idx).collect();
        Some((out.vertices, out.indices, handled))
    }

    /// Reduce the host mesh to a single straight prism swept along the
    /// reference payload's plane normal, or `None` if it isn't one (the
    /// non-reducible fallback). Computed once and reused for all cutters.
    fn reduce_host_to_prism(
        host: &(Vec<f32>, Vec<u32>),
        ref_payload: &BoundedHalfspacePayload,
    ) -> Option<HostPrism> {
        let n = ref_payload.plane_normal.normalize_or_zero();
        if n.length_squared() < 0.5 {
            return None;
        }
        // Orthonormal in-plane basis (e1, e2) ⟂ n.
        let helper = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        let e1 = n.cross(helper).normalize_or_zero();
        if e1.length_squared() < 0.5 {
            return None;
        }
        let e2 = n.cross(e1).normalize_or_zero();
        let origin = ref_payload.plane_point;

        // Sweep coordinate s = (v - origin)·n for every host vertex.
        let verts = &host.0;
        if verts.len() < 9 {
            return None;
        }
        let mut s_min = f32::INFINITY;
        let mut s_max = f32::NEG_INFINITY;
        for c in verts.chunks_exact(3) {
            let s = (Vec3::new(c[0], c[1], c[2]) - origin).dot(n);
            s_min = s_min.min(s);
            s_max = s_max.max(s);
        }
        let span = s_max - s_min;
        if span <= 1e-4 {
            return None;
        }
        // Every vertex must sit within CAP_CLUSTER_FRAC·span of one of the
        // two extreme sweep planes — i.e. the host is a straight prism
        // swept along n (a Z-perpendicular wall extrusion cut by a plane
        // whose normal is the sweep axis).
        let cap_eps = (span * CAP_CLUSTER_FRAC).max(1e-3);
        for c in verts.chunks_exact(3) {
            let s = (Vec3::new(c[0], c[1], c[2]) - origin).dot(n);
            if (s - s_min).abs() > cap_eps && (s - s_max).abs() > cap_eps {
                return None;
            }
        }

        // Host footprint = convex hull of vertices in (e1, e2). Exact for
        // a convex prism cap. A non-convex host is over-approximated, so
        // it is rejected by comparing the hull's swept volume against the
        // host mesh's signed volume.
        let mut pts2d: Vec<Vec2> = Vec::with_capacity(verts.len() / 3);
        for c in verts.chunks_exact(3) {
            let d = Vec3::new(c[0], c[1], c[2]) - origin;
            pts2d.push(Vec2::new(d.dot(e1), d.dot(e2)));
        }
        let hull = convex_hull(&pts2d)?;
        if hull.len() < 3 {
            return None;
        }
        let fp = Polygon2D { outer: hull, holes: Vec::new() };

        let hull_area = polygon_area(&fp.outer).abs();
        let prism_vol = hull_area * span;
        let mesh_vol = signed_volume(&host.0, &host.1).abs();
        if mesh_vol <= 0.0 || (prism_vol - mesh_vol).abs() > 0.02 * prism_vol {
            return None;
        }

        Some(HostPrism { e1, e2, n, origin, s_min, s_max, fp })
    }

    /// A boundary is tight unless it effectively contains the host
    /// footprint. Tested by polygon containment — `host − boundary` must
    /// exceed `TIGHT_AREA_FRAC` of the host area — NOT by AABB comparison
    /// (GH #64 #2: a genuinely-tight *rotated* rectangle inflates its
    /// axis-aligned bbox and was mis-read as "not tight", over-cutting
    /// angled / non-orthogonal-grid buildings). Frame-agnostic: both
    /// footprints are already in the prism's `(e1, e2)` frame.
    ///
    /// A boundary entirely DISJOINT from the host is (correctly) tight:
    /// `host − boundary = host`, so the cutter is handled here and the 2D
    /// path removes nothing (`host ∩ boundary = ∅`) — the faithful result
    /// for a bounded halfspace whose column misses the host. It is NOT a
    /// candidate for the infinite-plane fallback: that clip ignores the
    /// boundary and would shear off the whole subtracted side (the F6
    /// over-cut). So "handled → no-op → plane dropped" is deliberate; see
    /// `bounded_halfspace_missing_host_is_uncut` in the integration tests.
    pub(super) fn is_tight_boundary(host_fp: &Polygon2D, boundary_fp: &Polygon2D) -> bool {
        let host_shape = polygon_bool::shape_from_polygon2d(host_fp);
        let boundary_shape = polygon_bool::shape_from_polygon2d(boundary_fp);
        let remainder = polygon_bool::difference(&host_shape, &[boundary_shape]);
        let rem_area: f64 = remainder.iter().map(shape_area).sum();
        let host_area = shape_area(&host_shape);
        rem_area > TIGHT_AREA_FRAC * host_area.max(1.0)
    }

    /// Net area of a facade shape: |outer| − Σ|holes|, clamped at 0.
    fn shape_area(s: &Shape) -> f64 {
        let outer = s.first().map(|r| polygon_bool::signed_area(r).abs()).unwrap_or(0.0);
        let holes: f64 = s.iter().skip(1).map(|r| polygon_bool::signed_area(r).abs()).sum();
        (outer - holes).max(0.0)
    }

    fn project_boundary(
        boundary: &Polygon2D,
        boundary_xform: &Mat4,
        origin: Vec3,
        e1: Vec3,
        e2: Vec3,
    ) -> Polygon2D {
        let f = |v: &Vec2| {
            let w = *boundary_xform * Vec4::new(v.x, v.y, 0.0, 1.0);
            let d = Vec3::new(w.x, w.y, w.z) - origin;
            Vec2::new(d.dot(e1), d.dot(e2))
        };
        Polygon2D {
            outer: boundary.outer.iter().map(f).collect(),
            holes: boundary.holes.iter().map(|h| h.iter().map(f).collect()).collect(),
        }
    }

    /// 4×4 mapping `(u, v, w, 1)` in the `(e1, e2, n)` basis (origin
    /// `base`) back to the working frame. With `dir = +Z` this feeds
    /// `extrude_polygon` to rebuild the band in the working frame.
    fn basis_to_frame(e1: Vec3, e2: Vec3, n: Vec3, base: Vec3) -> Mat4 {
        Mat4::from_cols(
            Vec4::new(e1.x, e1.y, e1.z, 0.0),
            Vec4::new(e2.x, e2.y, e2.z, 0.0),
            Vec4::new(n.x, n.y, n.z, 0.0),
            Vec4::new(base.x, base.y, base.z, 1.0),
        )
    }

    /// Shift a basis frame along its +Z (sweep) column by `s` units.
    fn translate_along_axis(basis: &Mat4, s: f32) -> Mat4 {
        let mut m = *basis;
        let axis = Vec3::new(basis.z_axis.x, basis.z_axis.y, basis.z_axis.z);
        m.w_axis.x += axis.x * s;
        m.w_axis.y += axis.y * s;
        m.w_axis.z += axis.z * s;
        m
    }

    fn append(dst: &mut crate::mesh::extrusion::LocalMesh, src: &crate::mesh::extrusion::LocalMesh) {
        let base = (dst.vertices.len() / 3) as u32;
        dst.vertices.extend_from_slice(&src.vertices);
        dst.indices.extend(src.indices.iter().map(|i| i + base));
    }

    /// Andrew's monotone-chain convex hull of 2D points, CCW.
    fn convex_hull(points: &[Vec2]) -> Option<Vec<Vec2>> {
        let mut pts: Vec<Vec2> = points.to_vec();
        pts.sort_by(|a, b| {
            a.x.partial_cmp(&b.x)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
        });
        pts.dedup_by(|a, b| (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6);
        let npts = pts.len();
        if npts < 3 {
            return None;
        }
        let cross = |o: Vec2, a: Vec2, b: Vec2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
        let mut hull: Vec<Vec2> = Vec::with_capacity(npts + 1);
        // Lower hull.
        for &p in &pts {
            while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
                hull.pop();
            }
            hull.push(p);
        }
        // Upper hull.
        let lower_len = hull.len() + 1;
        for &p in pts.iter().rev() {
            while hull.len() >= lower_len
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop(); // last point == first
        if hull.len() < 3 {
            None
        } else {
            Some(hull)
        }
    }

    /// Signed area of a 2D ring (shoelace). CCW positive.
    fn polygon_area(ring: &[Vec2]) -> f32 {
        let nn = ring.len();
        if nn < 3 {
            return 0.0;
        }
        let mut a = 0.0;
        for i in 0..nn {
            let p = ring[i];
            let q = ring[(i + 1) % nn];
            a += p.x * q.y - q.x * p.y;
        }
        a * 0.5
    }

    /// Signed mesh volume (divergence theorem) of a flat-buffer mesh.
    /// Accumulates in f64 (GH #64 #9): the sum sits directly under the
    /// 2 % prism-reduction gate, where f32 cancellation across many large
    /// terms (mm-scale building coords cubed) would smear the comparison.
    fn signed_volume(vertices: &[f32], indices: &[u32]) -> f32 {
        let mut v6 = 0.0_f64;
        for t in indices.chunks_exact(3) {
            let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
            if (a * 3 + 2).max(b * 3 + 2).max(c * 3 + 2) >= vertices.len() {
                continue;
            }
            let ax = vertices[a * 3] as f64;
            let ay = vertices[a * 3 + 1] as f64;
            let az = vertices[a * 3 + 2] as f64;
            let bx = vertices[b * 3] as f64;
            let by = vertices[b * 3 + 1] as f64;
            let bz = vertices[b * 3 + 2] as f64;
            let cx = vertices[c * 3] as f64;
            let cy = vertices[c * 3 + 1] as f64;
            let cz = vertices[c * 3 + 2] as f64;
            v6 += ax * (by * cz - bz * cy) + ay * (bz * cx - bx * cz) + az * (bx * cy - by * cx);
        }
        (v6 / 6.0) as f32
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn poly(ring: &[(f32, f32)]) -> Polygon2D {
            Polygon2D {
                outer: ring.iter().map(|&(x, y)| Vec2::new(x, y)).collect(),
                holes: Vec::new(),
            }
        }

        /// Rotate a ring by `deg` about the origin — for the rotated-
        /// boundary test (#2).
        fn rotate(ring: &[(f32, f32)], deg: f32) -> Polygon2D {
            let r = deg.to_radians();
            let (s, c) = (r.sin(), r.cos());
            Polygon2D {
                outer: ring
                    .iter()
                    .map(|&(x, y)| Vec2::new(x * c - y * s, x * s + y * c))
                    .collect(),
                holes: Vec::new(),
            }
        }

        #[test]
        fn tight_boundary_is_tight() {
            // Host 1000×200, boundary 300×200 centred — strictly smaller
            // in X → tight.
            let host = poly(&[(-500.0, -100.0), (500.0, -100.0), (500.0, 100.0), (-500.0, 100.0)]);
            let boundary =
                poly(&[(-150.0, -100.0), (150.0, -100.0), (150.0, 100.0), (-150.0, 100.0)]);
            assert!(is_tight_boundary(&host, &boundary));
        }

        #[test]
        fn oversized_boundary_is_not_tight() {
            // Revit's oversize convention: boundary 5000×5000 around a
            // 1000×200 host → NOT tight (cheap plane clip is correct).
            let host = poly(&[(-500.0, -100.0), (500.0, -100.0), (500.0, 100.0), (-500.0, 100.0)]);
            let boundary = poly(&[
                (-2500.0, -2500.0),
                (2500.0, -2500.0),
                (2500.0, 2500.0),
                (-2500.0, 2500.0),
            ]);
            assert!(!is_tight_boundary(&host, &boundary));
        }

        /// GH #64 #2 — the case the old AABB test got WRONG. A thin
        /// 1400×100 strip rotated 45° has an axis-aligned bbox of
        /// ≈1061×1061 that fully CONTAINS the 800×800 host bbox — so the
        /// old `is_tight_boundary` (AABB-containment) declared it "not
        /// tight" and routed it to the over-cutting infinite-plane clip,
        /// even though the strip only crosses a diagonal band of the
        /// host. The polygon-containment test sees a large `host −
        /// boundary` remainder and correctly classifies it as tight.
        #[test]
        fn rotated_strip_boundary_is_tight() {
            let host = poly(&[(-400.0, -400.0), (400.0, -400.0), (400.0, 400.0), (-400.0, 400.0)]);
            // 1400×100 strip about the origin, rotated 45°.
            let strip = [(-700.0, -50.0), (700.0, -50.0), (700.0, 50.0), (-700.0, 50.0)];
            let boundary = rotate(&strip, 45.0);
            assert!(
                is_tight_boundary(&host, &boundary),
                "a rotated thin strip whose AABB engulfs the host is still tight",
            );
        }

        /// And a genuinely-rotated rectangular boundary that IS tight in
        /// the rotated frame stays tight (rotation invariance, the
        /// happy-path complement to the strip case).
        #[test]
        fn rotated_tight_boundary_is_tight() {
            let host_ring = [(-500.0, -100.0), (500.0, -100.0), (500.0, 100.0), (-500.0, 100.0)];
            let bnd_ring = [(-150.0, -100.0), (150.0, -100.0), (150.0, 100.0), (-150.0, 100.0)];
            let host = rotate(&host_ring, 30.0);
            let boundary = rotate(&bnd_ring, 30.0);
            assert!(is_tight_boundary(&host, &boundary));
        }

        /// A rotated oversized boundary must still read as not-tight — the
        /// containment test is rotation-invariant in both directions.
        #[test]
        fn rotated_oversized_boundary_is_not_tight() {
            let host_ring = [(-500.0, -100.0), (500.0, -100.0), (500.0, 100.0), (-500.0, 100.0)];
            let bnd_ring =
                [(-2500.0, -2500.0), (2500.0, -2500.0), (2500.0, 2500.0), (-2500.0, 2500.0)];
            let host = rotate(&host_ring, 37.0);
            let boundary = rotate(&bnd_ring, 37.0);
            assert!(!is_tight_boundary(&host, &boundary));
        }

        #[test]
        fn convex_hull_recovers_rectangle() {
            let pts = vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(10.0, 4.0),
                Vec2::new(0.0, 4.0),
                Vec2::new(5.0, 2.0), // interior point dropped
            ];
            let hull = convex_hull(&pts).expect("hull");
            assert_eq!(hull.len(), 4);
            assert!((polygon_area(&hull).abs() - 40.0).abs() < 1e-3);
        }
    }
}

/// Stream-time buffer for the cross-product `IfcRelVoidsElement`
/// case. Walls authored as a solid extrusion plus a separately-
/// modelled `IfcOpeningElement` linked by `IfcRelVoidsElement` mesh
/// independently — the indexer hands us a `(opening, host)` row per
/// relation, but the products surface in entity-table order, not
/// in a topologically sorted way that would let us fold on the fly.
///
/// `CrossProductCut` solves that by buffering both sides and folding
/// at flush time. The caller (typically the `MeshSink` wrapper in
/// `extract_meshes`) routes each incoming `ProductMesh` through
/// `route` — opening products get held as cutters, host products
/// get held as hosts, anything else falls through untouched. After
/// the streaming pass completes, [`flush`] runs the in-rep [`apply`]
/// on each buffered host first (so a host with BOTH an
/// `IfcBooleanClippingResult` and cross-product openings folds
/// cleanly), then subtracts the gathered openings via
/// `geom::csg::subtract_many`.
///
/// Frame contract: all buffered meshes are expected to live in
/// `BakeFrame::Local` form — vertices near origin, true world
/// position carried separately on `ProductMesh.mesh_anchor`. The
/// fold translates each opening's vertices by
/// `(opening.mesh_anchor - host.mesh_anchor)` before passing to the
/// CSG kernel, which keeps everything in the host's local frame and
/// stays f32-safe (host/opening anchors are typically <10 m apart).
/// A buffered opening cutter, captured at [`CrossProductCut::route`]
/// time. Carries enough to drive EITHER fold strategy:
/// * the **manifold fold** uses the baked `vertices` / `indices`
///   (translated by the host/opening anchor difference), and
/// * the **prism fast-path** (`prism-csg-fast` feature) re-derives the
///   opening's parametric prism from `rep_step_id` + `world_transform`
///   + `mesh_anchor`, never touching the baked triangles.
///
/// `rep_step_id` is `Some` only when the opening meshed as exactly one
/// *direct* (identity-instance) fragment — the precondition the prism
/// path needs to map a single `IfcExtrudedAreaSolid` back to params.
/// `world_transform` / `rep_step_id` are unread without the feature.
struct HeldOpening {
    vertices: Vec<f32>,
    indices: Vec<u32>,
    mesh_anchor: [f64; 3],
    #[cfg_attr(not(feature = "prism-csg-fast"), allow(dead_code))]
    rep_step_id: Option<u64>,
    #[cfg_attr(not(feature = "prism-csg-fast"), allow(dead_code))]
    world_transform: [f32; 16],
}

pub struct CrossProductCut {
    /// For each host step_id, the set of openings expected per
    /// `IfcRelVoidsElement`. A host may appear with multiple
    /// openings (typical for walls with several doors / windows).
    expected_openings: HashMap<u64, Vec<u64>>,
    /// Membership test for "is this product an opening?" — drives
    /// the suppression decision in `route`.
    openings: HashSet<u64>,
    /// Membership test for "is this product a host?".
    hosts: HashSet<u64>,
    /// Opening meshes that have arrived, keyed by opening step_id.
    /// See [`HeldOpening`] — the smallest payload that drives BOTH the
    /// manifold fold (baked buffers) and the prism fast-path (params
    /// re-derivation), so the rest of `ProductMesh` is dropped here.
    held_openings: HashMap<u64, HeldOpening>,
    /// Host meshes waiting for their openings, keyed by host
    /// step_id. The full `ProductMesh` is held so the existing
    /// MeshSink encoding logic (guid, entity, anchor, byte buffers)
    /// can run on the folded result without re-derivation.
    held_hosts: HashMap<u64, ProductMesh>,
}

/// What `CrossProductCut::route` decided for an incoming mesh.
#[derive(Debug)]
pub enum Routed {
    /// Product was identified as an `IfcOpeningElement` cutter and
    /// stashed for later folding. Caller MUST drop the mesh without
    /// forwarding it to the inner sink — in cut mode openings are
    /// not user-visible products.
    Suppressed,
    /// Product was identified as a host with at least one expected
    /// opening and stashed for later folding. Caller MUST drop the
    /// mesh without forwarding it; the eventual folded result comes
    /// out of `flush`.
    Held,
    /// Product is neither an opening nor a host (the common case).
    /// Caller continues with their normal in-rep `apply` pipeline.
    PassThrough(ProductMesh),
}

impl CrossProductCut {
    /// Build the index from the indexer's parallel arrays. Both
    /// slices must be the same length (one row per
    /// `IfcRelVoidsElement`).
    pub fn from_indexer(voids_opening: &[u64], voids_host: &[u64]) -> Self {
        let mut expected_openings: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut openings: HashSet<u64> = HashSet::new();
        let mut hosts: HashSet<u64> = HashSet::new();
        for (opening, host) in voids_opening.iter().zip(voids_host.iter()) {
            // Skip self-voids (pathological but cheap to guard).
            if opening == host {
                continue;
            }
            openings.insert(*opening);
            hosts.insert(*host);
            expected_openings.entry(*host).or_default().push(*opening);
        }
        Self {
            expected_openings,
            openings,
            hosts,
            held_openings: HashMap::new(),
            held_hosts: HashMap::new(),
        }
    }

    /// `true` if the index is empty (no `IfcRelVoidsElement` in the
    /// file). Callers can use this to short-circuit the wrapper and
    /// keep the hot path identical to non-cut behaviour.
    pub fn is_empty(&self) -> bool {
        self.expected_openings.is_empty()
    }

    /// Classify a mesh against the relationship index and stash it
    /// if it's a participant. Returns [`Routed::PassThrough`] for
    /// the common case where the product is neither an opening nor
    /// a host — the caller continues with their normal pipeline.
    pub fn route(&mut self, mesh: ProductMesh) -> Routed {
        let id = mesh.ifc_id;
        if self.openings.contains(&id) {
            let rep_step_id = single_direct_rep_step_id(&mesh.parts);
            self.held_openings.insert(
                id,
                HeldOpening {
                    vertices: mesh.vertices,
                    indices: mesh.indices,
                    mesh_anchor: mesh.mesh_anchor,
                    rep_step_id,
                    world_transform: mesh.world_transform,
                },
            );
            return Routed::Suppressed;
        }
        if self.hosts.contains(&id) {
            self.held_hosts.insert(id, mesh);
            return Routed::Held;
        }
        Routed::PassThrough(mesh)
    }

    /// Fold every buffered host with its arrived openings and yield
    /// the (mesh, outcome) pairs ready for the caller's emit path.
    ///
    /// Per-host sequence: run the in-rep [`apply`] first (so a host
    /// authored as `IfcBooleanClippingResult` plus a cross-product
    /// opening collapses the in-rep operands before the cross fold).
    /// Then, when `table` is `Some` and the host was untouched by the
    /// in-rep pass (`Passthrough`), try the **prism fast-path**
    /// (`prism-csg-fast` feature): re-derive host + opening params and
    /// subtract in 2D — see [`try_prism_cut`]. The prism path handles
    /// only the reducible same-axis through-cut; anything else returns
    /// `None` and we fall back to the manifold fold, which translates
    /// each opening's vertices by `(opening.mesh_anchor -
    /// host.mesh_anchor)` and calls `geom::csg::subtract_many`.
    ///
    /// `table` is the entity table the prism path consults for
    /// `extrude_params`; pass `None` to force the manifold fold (the
    /// default-build behaviour, and required for `BakeFrame::World`
    /// callers — the prism result is emitted near-origin and only
    /// matches a `BakeFrame::Local` host mesh). The param is accepted
    /// in all builds; without the feature it is ignored.
    ///
    /// Combined outcome:
    /// * `Cut` — the prism or manifold subtraction produced the net
    ///   solid (or consumed the host, leaving empty geometry).
    /// * `Fallback` — cutters existed but every subtraction failed.
    /// * `Passthrough` — no openings affected the host (none arrived,
    ///   or the prism path reported the cutters miss the host sweep).
    pub fn flush(
        &mut self,
        unit_scale: f32,
        table: Option<&EntityTable>,
    ) -> Vec<(ProductMesh, Outcome)> {
        let _ = table; // consulted only by the prism-csg-fast path
        let mut out = Vec::with_capacity(self.held_hosts.len());
        let held_hosts = std::mem::take(&mut self.held_hosts);
        for (host_id, mut host_mesh) in held_hosts {
            // Run in-rep cut first. Combined outcome accounting is
            // simplified to "did any subtraction happen on this
            // product?" — see the doc above.
            let in_rep = apply(&mut host_mesh, unit_scale);

            // step_ids of the openings that actually arrived for this
            // host (geometryless / unmeshed openings never landed in
            // `held_openings` and are silently skipped).
            let expected = self
                .expected_openings
                .get(&host_id)
                .cloned()
                .unwrap_or_default();
            let arrived: Vec<u64> = expected
                .iter()
                .copied()
                .filter(|oid| self.held_openings.contains_key(oid))
                .collect();

            // ---- Prism fast-path (feature-gated, table-gated). -------
            // Only when the in-rep pass left the host body intact: the
            // prism path re-derives params from the ORIGINAL extrusion,
            // so it must not run on a host the in-rep cut already
            // mutated. A `None` return drops through to the manifold
            // fold below with the host mesh untouched.
            #[cfg(feature = "prism-csg-fast")]
            let prism_outcome: Option<Outcome> =
                if matches!(in_rep, Outcome::Passthrough) {
                    table.and_then(|t| {
                        let openings: Vec<&HeldOpening> = arrived
                            .iter()
                            .filter_map(|oid| self.held_openings.get(oid))
                            .collect();
                        try_prism_cut(&mut host_mesh, &openings, t)
                    })
                } else {
                    None
                };
            #[cfg(not(feature = "prism-csg-fast"))]
            let prism_outcome: Option<Outcome> = None;

            if let Some(outcome) = prism_outcome {
                out.push((host_mesh, outcome));
                continue;
            }

            // ---- Manifold fold (default path / prism fallback). ------
            // Translate each arrived opening into the host frame by the
            // anchor difference, then subtract via the mesh kernel.
            let cutter_buffers: Vec<(Vec<f32>, Vec<u32>)> = arrived
                .iter()
                .filter_map(|oid| {
                    let op = self.held_openings.get(oid)?;
                    let off = [
                        (op.mesh_anchor[0] - host_mesh.mesh_anchor[0]) as f32,
                        (op.mesh_anchor[1] - host_mesh.mesh_anchor[1]) as f32,
                        (op.mesh_anchor[2] - host_mesh.mesh_anchor[2]) as f32,
                    ];
                    let translated: Vec<f32> = op
                        .vertices
                        .chunks_exact(3)
                        .flat_map(|c| {
                            [c[0] + off[0], c[1] + off[1], c[2] + off[2]]
                        })
                        .collect();
                    Some((translated, op.indices.clone()))
                })
                .collect();

            let combined = if cutter_buffers.is_empty() {
                // No cross-product cutters arrived → outcome stays
                // whatever the in-rep apply said (typically
                // Passthrough for a plain extruded wall).
                in_rep
            } else {
                let cutter_refs: Vec<(&[f32], &[u32])> = cutter_buffers
                    .iter()
                    .map(|(v, i)| (v.as_slice(), i.as_slice()))
                    .collect();
                match csg::subtract_many(
                    &host_mesh.vertices,
                    &host_mesh.indices,
                    &cutter_refs,
                ) {
                    Ok((verts, idx)) => {
                        let triangle_count = (idx.len() / 3) as u32;
                        host_mesh.vertices = verts;
                        host_mesh.indices = idx;
                        host_mesh.segments = vec![MeshSegment {
                            index_start: 0,
                            index_count: triangle_count * 3,
                            source: "cut_openings".to_string(),
                        }];
                        host_mesh.parts.clear();
                        Outcome::Cut
                    }
                    Err(_) => {
                        // If the in-rep cut already succeeded, keep
                        // that as the win. Otherwise classify the
                        // cross-product failure the same way the in-rep
                        // path does (W3): attribute it to non-manifold
                        // input when the host or a translated opening
                        // is not a closed manifold, else opaque
                        // Fallback. Post-failure only — never gates the
                        // cut.
                        if matches!(in_rep, Outcome::Cut) {
                            Outcome::Cut
                        } else {
                            classify_subtract_failure(&host_mesh.indices, &cutter_buffers)
                        }
                    }
                }
            };

            out.push((host_mesh, combined));
        }
        // Any opening meshes still held belong to hosts that never
        // arrived (geometryless host, host outside the meshable
        // product set). Drop them — in cut mode they're not visible
        // products on their own.
        self.held_openings.clear();
        out
    }
}

/// The single representation-item step_id of a product that meshed as
/// exactly one *direct* (identity-instance) fragment, else `None`. The
/// prism fast-path needs this to map the baked mesh back to one
/// `IfcExtrudedAreaSolid`; multi-fragment or `IfcMappedItem`-instanced
/// products carry a non-identity `instance_transform` the prism frame
/// composition (which uses only the product world transform) would not
/// account for, so they are excluded → manifold fallback.
fn single_direct_rep_step_id(parts: &[InstancePart]) -> Option<u64> {
    if parts.len() != 1 {
        return None;
    }
    let p = &parts[0];
    if !is_identity_mat4(&p.instance_transform) {
        return None;
    }
    Some(p.rep_step_id)
}

/// Column-major 4×4 identity test with a tight absolute tolerance —
/// `instance_transform` is exactly identity for direct geometry, so
/// this only rejects genuinely-instanced fragments.
fn is_identity_mat4(m: &[f32; 16]) -> bool {
    const I: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    m.iter().zip(I.iter()).all(|(a, b)| (a - b).abs() < 1e-6)
}

/// Attempt the prism fast-path on one host + its arrived openings.
///
/// Returns `Some(outcome)` when the path applied (host mesh mutated in
/// place as needed), or `None` when the configuration is outside the
/// reducible case — missing/non-extrusion params on the host or ANY
/// opening, or `prism_csg` reporting `NotParametric` (non-parallel
/// sweep axes, oblique extrusion, partial/pocket cut). On `None` the
/// host mesh is left untouched so the caller's manifold fold takes
/// over for the WHOLE host (no mixed prism/manifold cutter handling —
/// that is a deferred slab-decomposition follow-up).
///
/// # Frame contract (F3)
///
/// All prisms are composed into the host's mesh-anchor-local frame,
/// which is exactly the frame a `BakeFrame::Local` host mesh lives in:
/// vertices = `rot_only(world_transform) · local_xform · profile`, with
/// the world translation carried separately on `mesh_anchor` (added
/// back downstream). The host's anchor is the working origin; the
/// cutter is rebased by the f64 anchor *difference* via
/// [`prism_csg::rebase_params`], so only a small (<10 m, adjacent
/// products) offset ever touches the f32 matrix — UTM magnitudes never
/// collapse. This is why `flush` must pass `None` for `BakeFrame::World`
/// callers: there the host mesh is in world coordinates and the
/// near-origin prism result would not align.
#[cfg(feature = "prism-csg-fast")]
fn try_prism_cut(
    host_mesh: &mut ProductMesh,
    openings: &[&HeldOpening],
    table: &EntityTable,
) -> Option<Outcome> {
    use crate::mesh::extrusion::extrude_params;
    use crate::mesh::prism_csg::{self, PrismCsgOutcome, PrismParams};

    // Host must be a single direct extrusion with resolvable params.
    let host_rep = single_direct_rep_step_id(&host_mesh.parts)?;
    let host_rot = rot_only(&host_mesh.world_transform);
    let mut host_prism: PrismParams = extrude_params(table, host_rep)?.into();
    host_prism.local_xform = host_rot * host_prism.local_xform;
    let host_anchor = host_mesh.mesh_anchor;

    // EVERY opening must reduce to a parametric prism in the host frame;
    // a single non-extrusion / multi-fragment opening bails the whole
    // host to the manifold fold (which handles the mixed set correctly).
    let mut cutters: Vec<PrismParams> = Vec::with_capacity(openings.len());
    for op in openings {
        let rep = op.rep_step_id?;
        let mut p: PrismParams = extrude_params(table, rep)?.into();
        p.local_xform = rot_only(&op.world_transform) * p.local_xform;
        cutters.push(prism_csg::rebase_params(&p, op.mesh_anchor, host_anchor));
    }
    if cutters.is_empty() {
        return None;
    }

    match prism_csg::subtract_many(&host_prism, &cutters) {
        PrismCsgOutcome::Cut(mesh) => {
            rewrite_host_geometry(host_mesh, mesh.vertices, mesh.indices);
            Some(Outcome::Cut)
        }
        PrismCsgOutcome::Empty => {
            // Openings consumed the host entirely — legitimate net-empty
            // solid. Emit empty geometry (the caller skips empty meshes);
            // the cut still "happened", so count it as Cut.
            rewrite_host_geometry(host_mesh, Vec::new(), Vec::new());
            Some(Outcome::Cut)
        }
        // Cutters do not overlap the host along the sweep axis → host is
        // unaffected; leave its mesh as-is and report passthrough.
        PrismCsgOutcome::Unchanged => Some(Outcome::Passthrough),
        // Outside the reducible case → manifold fallback.
        PrismCsgOutcome::NotParametric => None,
    }
}

/// Replace a host's world-baked geometry with a cut result and collapse
/// it to the single `cut_openings` segment, clearing the per-fragment
/// instance payload (a wall-minus-this-opening is unique — no dedup).
/// Mirrors the rewrite the in-rep [`apply`] and manifold fold perform.
#[cfg(feature = "prism-csg-fast")]
fn rewrite_host_geometry(host_mesh: &mut ProductMesh, vertices: Vec<f32>, indices: Vec<u32>) {
    let index_count = indices.len() as u32;
    host_mesh.vertices = vertices;
    host_mesh.indices = indices;
    host_mesh.segments = vec![MeshSegment {
        index_start: 0,
        index_count,
        source: "cut_openings".to_string(),
    }];
    host_mesh.parts.clear();
}

/// The rotation/linear part of a column-major world transform as a
/// `Mat4` with its translation column zeroed. The `BakeFrame::Local`
/// bake applies exactly this linear part to geometry (`transform_vector3`
/// drops translation) and carries the world position on `mesh_anchor`,
/// so composing it into the prism's `local_xform` reproduces the baked
/// host frame without ever putting world translation into f32.
#[cfg(feature = "prism-csg-fast")]
fn rot_only(world_transform: &[f32; 16]) -> glam::Mat4 {
    let mut m = glam::Mat4::from_cols_array(world_transform);
    m.w_axis.x = 0.0;
    m.w_axis.y = 0.0;
    m.w_axis.z = 0.0;
    m
}

/// Classify segments by whether their source tag marks them as a
/// boolean subtractor. A segment's `source` is a compound tag like
/// `"boolean_first_operand|extrusion"` or
/// `"boolean_second_operand|halfspace_bounded"` — see
/// `mesh::boolean::retag` for the construction rule. Any segment
/// whose chain contains `"boolean_second_operand"` is treated as a
/// cutter; everything else is host.
fn partition_segments(segments: &[MeshSegment]) -> (Vec<&MeshSegment>, Vec<&MeshSegment>) {
    let mut hosts = Vec::with_capacity(segments.len());
    let mut cutters = Vec::with_capacity(segments.len());
    for s in segments {
        if is_cutter(&s.source) {
            cutters.push(s);
        } else {
            hosts.push(s);
        }
    }
    (hosts, cutters)
}

fn is_cutter(source: &str) -> bool {
    // `|` is the chain separator in the compound source tag emitted by
    // `boolean::retag`. After W1's chain refactor every wrapping
    // boolean's role is preserved in the chain (outermost first), so a
    // fragment that is structurally a cutter at ANY level carries
    // `boolean_second_operand` somewhere in its chain — even when the
    // innermost level is a host (e.g. nested `BooleanResult(host=wall,
    // cutter=BooleanResult(host=door, cutter=handle))` makes the door
    // fragment a cutter at the outer level despite being a host at the
    // inner one).
    chain_contains(source, "boolean_second_operand")
}

/// Scan a product's segments for a non-DIFFERENCE boolean operand
/// (W4 — tagged by `boolean::second_operand_role`). Returns the typed
/// reason for the first such operand: `UnionWithOverlap` for a
/// `boolean_union_operand` token, `IntersectionNotImplemented` for
/// `boolean_intersection_operand`. These operands are NOT cutters (so
/// they are never subtracted — the F4 correctness fix), but their
/// presence means the net solid we'd ideally produce is a union /
/// intersection we don't compute, so we surface the signal rather than
/// reporting a plain passthrough. `None` when every operand is a plain
/// DIFFERENCE host / cutter. Union is checked first only for
/// determinism; a product carrying both is pathological.
fn detect_unsupported_boolean_op(segments: &[MeshSegment]) -> Option<UnsupportedReason> {
    if segments
        .iter()
        .any(|s| chain_contains(&s.source, "boolean_union_operand"))
    {
        return Some(UnsupportedReason::UnionWithOverlap);
    }
    if segments
        .iter()
        .any(|s| chain_contains(&s.source, "boolean_intersection_operand"))
    {
        return Some(UnsupportedReason::IntersectionNotImplemented);
    }
    None
}

/// Classify a failed manifold subtract (W3). When the host or any
/// solid cutter is not a closed manifold, the kernel's rejection is
/// attributable to bad input — the Revit "open / odd-paired opening
/// solid" reality — so surface `Unsupported(NonManifoldInput)` instead
/// of an opaque `Fallback`. Otherwise the failure has no diagnostic and
/// stays `Fallback`. Either outcome leaves the mesh untouched
/// (reveal-all). This runs ONLY after the kernel already rejected the
/// input, so the conservative (false-negative-prone) manifold check can
/// never suppress a cut the kernel would have accepted.
fn classify_subtract_failure(
    host_indices: &[u32],
    solid_cutters: &[(Vec<f32>, Vec<u32>)],
) -> Outcome {
    let bad_input = !cut_validate::is_manifold_mesh(host_indices)
        || solid_cutters
            .iter()
            .any(|(_, i)| !cut_validate::is_manifold_mesh(i));
    if bad_input {
        Outcome::Unsupported(UnsupportedReason::NonManifoldInput)
    } else {
        Outcome::Fallback
    }
}

/// True if `link` appears at least once as a token in the compound
/// `source` chain (pipe-separated). Reading helper for the W1 chain
/// encoding — see [`boolean::retag`] for the chain construction rule.
/// Exposed publicly for integration tests and for downstream agents
/// that want to introspect provenance without parsing the chain
/// themselves.
pub fn chain_contains(source: &str, link: &str) -> bool {
    source.split('|').any(|l| l == link)
}

/// Count how many times `link` appears in the compound `source` chain.
/// A 3-deep nested boolean where the same role applies at multiple
/// levels (e.g. cutter-of-cutter) returns `2`. Useful for diagnosing
/// composite-tree behaviour and as a stats counter for the
/// `Outcome::Unsupported(BspDepthExceeded)` recursion-depth guard.
pub fn chain_count(source: &str, link: &str) -> usize {
    source.split('|').filter(|l| *l == link).count()
}

/// Pull the triangles addressed by `segments` out of the shared
/// `vertices` / `indices` buffers into a compact (vertices, indices)
/// pair where the indices are remapped to 0..N over a deduplicated
/// vertex set. Returns `None` if the slice is empty.
fn assemble_submesh(
    vertices: &[f32],
    indices: &[u32],
    segments: &[&MeshSegment],
) -> Option<(Vec<f32>, Vec<u32>)> {
    if segments.is_empty() {
        return None;
    }

    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut out_vertices: Vec<f32> = Vec::new();
    let mut out_indices: Vec<u32> = Vec::new();

    for seg in segments {
        let start = seg.index_start as usize;
        let end = start + seg.index_count as usize;
        if end > indices.len() {
            // Malformed segment range — skip rather than crash.
            continue;
        }
        for &orig_idx in &indices[start..end] {
            let new_idx = *remap.entry(orig_idx).or_insert_with(|| {
                let n = out_vertices.len() / 3;
                let base = (orig_idx as usize) * 3;
                // Guard against truncated vertex buffer just in case.
                if base + 2 < vertices.len() {
                    out_vertices.push(vertices[base]);
                    out_vertices.push(vertices[base + 1]);
                    out_vertices.push(vertices[base + 2]);
                    n as u32
                } else {
                    u32::MAX
                }
            });
            if new_idx == u32::MAX {
                return None;
            }
            out_indices.push(new_idx);
        }
    }

    if out_vertices.is_empty() || out_indices.is_empty() {
        None
    } else {
        Some((out_vertices, out_indices))
    }
}

/// Expose `partition_segments` to integration tests and other crates
/// that want to verify which segments classify as host vs cutter
/// without invoking the full `apply` path. The double-underscore name
/// signals "test/inspection surface, do not depend on this in
/// production". Returns `(hosts, cutters)`.
pub fn _expose_partition<'a>(
    segments: &'a [MeshSegment],
) -> (Vec<&'a MeshSegment>, Vec<&'a MeshSegment>) {
    partition_segments(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1×1 box at given origin, returns (verts, idx) in our flat
    /// buffer form.
    fn box_at(min: [f32; 3], max: [f32; 3]) -> (Vec<f32>, Vec<u32>) {
        let v: Vec<f32> = vec![
            min[0], min[1], min[2],
            max[0], min[1], min[2],
            max[0], max[1], min[2],
            min[0], max[1], min[2],
            min[0], min[1], max[2],
            max[0], min[1], max[2],
            max[0], max[1], max[2],
            min[0], max[1], max[2],
        ];
        let i: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2,
            4, 5, 6, 4, 6, 7,
            0, 1, 5, 0, 5, 4,
            2, 3, 7, 2, 7, 6,
            1, 2, 6, 1, 6, 5,
            0, 4, 7, 0, 7, 3,
        ];
        (v, i)
    }

    /// Build a synthetic ProductMesh that mimics the extractor's
    /// output for a wall authored as
    /// `IfcBooleanClippingResult(big_box, small_box)`: vertices for
    /// both fragments concatenated, indices for the first fragment
    /// then the second fragment, two segments tagged respectively.
    fn synthetic_wall_with_opening() -> ProductMesh {
        let (host_v, host_i) = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let (cut_v, cut_i) = box_at([0.25, 0.25, 0.25], [0.75, 0.75, 0.75]);

        let host_vert_count = (host_v.len() / 3) as u32;
        let cut_idx_shifted: Vec<u32> = cut_i.iter().map(|i| i + host_vert_count).collect();

        let mut vertices = host_v;
        vertices.extend_from_slice(&cut_v);
        let host_index_count = host_i.len() as u32;
        let cut_index_count = cut_idx_shifted.len() as u32;
        let mut indices = host_i;
        indices.extend_from_slice(&cut_idx_shifted);

        ProductMesh {
            guid: "wall-test-guid".into(),
            entity: "IfcWall".into(),
            ifc_id: 1,
            vertices,
            indices,
            source: "boolean_first_operand|extrusion",
            segments: vec![
                MeshSegment {
                    index_start: 0,
                    index_count: host_index_count,
                    source: "boolean_first_operand|extrusion".to_string(),
                },
                MeshSegment {
                    index_start: host_index_count,
                    index_count: cut_index_count,
                    source: "boolean_second_operand|extrusion".to_string(),
                },
            ],
            placement_origin: [0.0, 0.0, 0.0],
            parts: Vec::new(),
            world_transform: [
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
            world_origin: [0.0, 0.0, 0.0],
            mesh_anchor: [0.0, 0.0, 0.0],
            surface_color: None,
            bounded_halfspaces: Vec::new(),
        }
    }

    #[test]
    fn partition_classifies_segments_by_chain_link() {
        assert!(is_cutter("boolean_second_operand|extrusion"));
        assert!(is_cutter("boolean_first_operand|boolean_second_operand|halfspace_bounded"));
        assert!(!is_cutter("boolean_first_operand|extrusion"));
        assert!(!is_cutter("extrusion"));
        assert!(!is_cutter("mapped"));
    }

    #[test]
    fn apply_passes_through_when_no_cutters() {
        let (v, i) = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let idx_count = i.len() as u32;
        let mut mesh = ProductMesh {
            guid: "plain-wall".into(),
            entity: "IfcWall".into(),
            ifc_id: 2,
            vertices: v,
            indices: i,
            source: "extrusion",
            segments: vec![MeshSegment {
                index_start: 0,
                index_count: idx_count,
                source: "extrusion".to_string(),
            }],
            placement_origin: [0.0, 0.0, 0.0],
            parts: Vec::new(),
            world_transform: [
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
            world_origin: [0.0, 0.0, 0.0],
            mesh_anchor: [0.0, 0.0, 0.0],
            surface_color: None,
            bounded_halfspaces: Vec::new(),
        };
        let before_verts = mesh.vertices.clone();
        assert_eq!(apply(&mut mesh, 1.0), Outcome::Passthrough);
        assert_eq!(mesh.vertices, before_verts);
        assert_eq!(mesh.segments.len(), 1);
        assert_eq!(mesh.segments[0].source, "extrusion");
    }

    #[test]
    fn apply_cuts_opening_and_rewrites_segments() {
        let mut mesh = synthetic_wall_with_opening();
        // Sanity: input has two segments.
        assert_eq!(mesh.segments.len(), 2);

        let outcome = apply(&mut mesh, 1.0);
        assert_eq!(outcome, Outcome::Cut);

        // Output has exactly one segment, tagged "cut_openings".
        assert_eq!(mesh.segments.len(), 1);
        assert_eq!(mesh.segments[0].source, "cut_openings");
        assert_eq!(mesh.segments[0].index_start, 0);
        assert_eq!(mesh.segments[0].index_count as usize, mesh.indices.len());

        // Volume should equal host (1.0) minus opening (0.5³ = 0.125).
        let m = csg::build_manifold(&mesh.vertices, &mesh.indices)
            .expect("cut wall is manifold");
        let expected = 1.0_f64 - 0.5_f64.powi(3);
        assert!(
            (m.volume() - expected).abs() < 1e-3,
            "expected volume ≈ {expected}, got {}",
            m.volume()
        );
    }

    #[test]
    fn apply_clears_parts_after_cut() {
        let mut mesh = synthetic_wall_with_opening();
        // Pretend the extractor populated parts for substrate dedup.
        mesh.parts.push(crate::mesh::InstancePart {
            rep_step_id: 42,
            instance_transform: [
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
            local_vertices: vec![0.0, 0.0, 0.0],
            local_indices: vec![],
            index_start: 0,
            index_count: 0,
            source: "boolean_first_operand|extrusion".into(),
            surface_color: None,
        });
        let _ = apply(&mut mesh, 1.0);
        assert!(mesh.parts.is_empty(), "parts must clear post-cut");
    }

    #[test]
    fn stats_accumulator_threads_outcomes() {
        let mut stats = CutOpeningsStats::default();
        Outcome::Cut.accumulate(&mut stats);
        Outcome::Cut.accumulate(&mut stats);
        Outcome::Passthrough.accumulate(&mut stats);
        Outcome::Fallback.accumulate(&mut stats);
        assert_eq!(stats.cut, 2);
        assert_eq!(stats.passthrough, 1);
        assert_eq!(stats.fallback, 1);
    }

    /// Build a two-operand product whose second operand carries the
    /// given chain source — used to exercise the W4 operator-aware
    /// paths (union / intersection operands are NOT cutters).
    fn wall_with_second_operand(second_source: &str) -> ProductMesh {
        let mut mesh = synthetic_wall_with_opening();
        mesh.segments[1].source = second_source.to_string();
        mesh
    }

    #[test]
    fn union_operand_is_not_a_cutter() {
        // The F4 correctness fix: a UNION / INTERSECTION operand must
        // NOT be subtracted. `is_cutter` only matches DIFFERENCE.
        assert!(!is_cutter("boolean_union_operand|extrusion"));
        assert!(!is_cutter("boolean_intersection_operand|extrusion"));
        assert!(is_cutter("boolean_second_operand|extrusion"));
    }

    #[test]
    fn union_operand_surfaces_unsupported_and_preserves_mesh() {
        let mut mesh = wall_with_second_operand("boolean_union_operand|extrusion");
        let before_verts = mesh.vertices.clone();
        let before_indices = mesh.indices.clone();
        let outcome = apply(&mut mesh, 1.0);
        assert_eq!(
            outcome,
            Outcome::Unsupported(UnsupportedReason::UnionWithOverlap)
        );
        // Reveal-all: the mesh is untouched (both operands still visible).
        assert_eq!(mesh.vertices, before_verts);
        assert_eq!(mesh.indices, before_indices);
        assert_eq!(mesh.segments.len(), 2);
    }

    #[test]
    fn intersection_operand_surfaces_unsupported() {
        let mut mesh = wall_with_second_operand("boolean_intersection_operand|extrusion");
        let outcome = apply(&mut mesh, 1.0);
        assert_eq!(
            outcome,
            Outcome::Unsupported(UnsupportedReason::IntersectionNotImplemented)
        );
    }

    #[test]
    fn non_manifold_cutter_classified_on_failure() {
        // A single triangle is not a closed manifold; an empty host is
        // trivially "bad input" too. The classifier attributes the
        // (hypothetical) kernel rejection to NonManifoldInput.
        let open_cutter = (vec![0.0; 9], vec![0u32, 1, 2]);
        let closed_host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        assert_eq!(
            classify_subtract_failure(&closed_host.1, &[open_cutter]),
            Outcome::Unsupported(UnsupportedReason::NonManifoldInput)
        );
        // Both manifold → no diagnostic, stays Fallback.
        let closed_cutter = box_at([0.25, 0.25, 0.25], [0.75, 0.75, 0.75]);
        assert_eq!(
            classify_subtract_failure(&closed_host.1, &[closed_cutter]),
            Outcome::Fallback
        );
    }
}

// Silence unused-import warning on the geom::csg::CsgKernelError type
// in non-test compiles where the error variant naming would otherwise
// be flagged. We keep the alias for callers that want to refer to it.
#[allow(dead_code)]
type _CsgError = CsgKernelError;
