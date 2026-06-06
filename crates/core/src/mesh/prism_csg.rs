//! Pure-Rust prism-minus-prism CSG — the "prism algebra" Layer A of the
//! manifold replacement (GH #58 W9). The ~85–95 % real-world case:
//! a wall (extruded profile) minus a door / window (another extruded
//! profile) where both sweep the same world axis. When that holds the
//! 3D Boolean reduces to a *2D* Boolean on the profiles + a re-extrude,
//! which is cheap, robust (via [`crate::mesh::polygon_bool`] /
//! i_overlay), and needs no C++ kernel. Anything outside the reducible
//! case returns [`PrismCsgOutcome::NotParametric`] and the caller falls
//! back to manifold.
//!
//! # Frame contract (the F3 fix — read before using)
//!
//! Every [`PrismParams`] passed to one [`subtract`] / [`subtract_many`]
//! call MUST live in a single shared working frame, and that frame MUST
//! be **near-origin rebased** (mesh_anchor-local for cross-product
//! cuts, body-local for in-representation booleans). `local_xform`
//! carries only the *solid-local* `IfcAxis2Placement3D` (small, ~±10 m),
//! NEVER the product's world `ObjectPlacement` (which can be UTM-scale
//! and would collapse f32). This module performs all geometry in that
//! frame and emits its result mesh in the same frame, so it composes
//! with the existing world-transform + anchor-rebase bake loop without
//! ever touching world coordinates. The audit flagged a proposed
//! `placement_mat4`-carrying sidecar as re-introducing the v0.4.15
//! anchor bug precisely because it ignored this; the contract is in the
//! type docs so a caller can't silently violate it.
//!
//! # Scope of the current implementation
//!
//! Through-cut only (the cutter spans the host's full sweep range — a
//! door/window penetrating a wall), with both prisms extruded
//! perpendicular to their profile plane (`ExtrudedDirection ≈ local Z`,
//! the IFC default). Oblique extrusion, partial recesses, and pockets
//! (cutter ending inside the host) return `NotParametric` for now;
//! their slab-decomposition is a documented follow-up. This keeps the
//! first cut correct-by-construction and verifiable against analytic
//! volume; coverage widens once the through-cut path is proven against
//! the proptest baseline.

use glam::{Mat4, Vec2, Vec3, Vec4};

use crate::mesh::extrusion::{extrude_polygon, ExtrudeParams, LocalMesh};
use crate::mesh::polygon_bool;
use crate::mesh::profile::Polygon2D;

/// Sweep axes are treated as parallel when `|n_h · n_c| ≥ 1 − AXIS_EPS`
/// (≈ 0.8°). Below that the cutter sweeps a different world direction
/// and no closed-form prism algebra exists → `NotParametric`.
const AXIS_EPS: f32 = 1.0e-4;

/// `ExtrudedDirection` is treated as perpendicular to the profile plane
/// when its in-plane component is below this (i.e. dir ≈ ±local Z).
/// Oblique extrusion shears the footprint projection → `NotParametric`.
const PERP_EPS: f32 = 1.0e-4;

/// Sweep-range / overlap comparisons use this slack (working-frame
/// units; the frame is near-origin so this is an absolute ~µm at metre
/// scale — tightened/loosened by the caller's rebase, not unit-aware
/// here because the 2D Boolean's own robustness dominates).
const SWEEP_EPS: f32 = 1.0e-5;

/// Parametric description of an extruded prism in a caller-chosen LOCAL
/// frame. See the module-level **Frame contract**: all params in one
/// `subtract` call share a single near-origin frame, and `local_xform`
/// is the solid-local placement only — never the world ObjectPlacement.
#[derive(Debug, Clone)]
pub struct PrismParams {
    /// Profile (outer ring + holes) in the profile's own 2D coordinates.
    pub profile: Polygon2D,
    /// Unit extrusion direction in the local (pre-`local_xform`) frame.
    /// The reducible case requires this ≈ ±Z (perpendicular extrusion).
    pub dir: Vec3,
    /// Sweep depth along `dir`, in local units.
    pub depth: f32,
    /// Solid-local placement: profile/local frame → the shared working
    /// frame. Must NOT carry the world ObjectPlacement (F3).
    pub local_xform: Mat4,
}

impl From<ExtrudeParams> for PrismParams {
    /// An `IfcExtrudedAreaSolid`'s resolved parameters ARE prism params
    /// (same four fields), in the solid's body-local frame. The frame
    /// contract is the caller's responsibility: when combining prisms
    /// from different products (cross-product `IfcRelVoidsElement`),
    /// rebase into one shared near-origin frame via [`rebase_params`]
    /// first.
    fn from(p: ExtrudeParams) -> Self {
        PrismParams {
            profile: p.profile,
            dir: p.dir,
            depth: p.depth,
            local_xform: p.local_xform,
        }
    }
}

/// Re-express a prism's params from one mesh-anchor frame into another
/// by the anchor difference, for the cross-product cut where host and
/// cutter are separate products with separate anchors (the same rigid
/// offset `CrossProductCut` applies to opening vertices).
///
/// **F3-safe by construction.** `from_anchor` / `to_anchor` are f64
/// world-scale anchors (possibly UTM, ~600 km), but only their
/// *difference* — typically < 10 m for adjacent products — touches the
/// f32 `local_xform`. The UTM magnitude never enters the matrix, so the
/// prism algebra stays near-origin and f32-precise. Passing a raw world
/// `ObjectPlacement` translation into `local_xform` would violate the
/// frame contract and collapse under f32; this helper is the supported
/// way to move a prism between anchor frames.
pub fn rebase_params(p: &PrismParams, from_anchor: [f64; 3], to_anchor: [f64; 3]) -> PrismParams {
    let offset = Vec3::new(
        (from_anchor[0] - to_anchor[0]) as f32,
        (from_anchor[1] - to_anchor[1]) as f32,
        (from_anchor[2] - to_anchor[2]) as f32,
    );
    let mut out = p.clone();
    out.local_xform.w_axis.x += offset.x;
    out.local_xform.w_axis.y += offset.y;
    out.local_xform.w_axis.z += offset.z;
    out
}

/// Result of a prism-algebra subtraction.
#[derive(Debug)]
pub enum PrismCsgOutcome {
    /// Net solid as a triangle mesh in the shared working frame.
    Cut(LocalMesh),
    /// The cutter(s) fully consumed the host — empty solid.
    Empty,
    /// The cutter(s) do not overlap the host along the sweep axis — the
    /// host is unaffected; the caller keeps it as-is.
    Unchanged,
    /// Not reducible to prism algebra (non-parallel sweep axes, oblique
    /// extrusion, partial/pocket cut, or degenerate input). The caller
    /// falls back to mesh CSG (manifold).
    NotParametric,
}

/// Subtract a single cutter prism from a host prism. See
/// [`subtract_many`] for the multi-cutter form; this is the one-cutter
/// convenience wrapper.
pub fn subtract(host: &PrismParams, cutter: &PrismParams) -> PrismCsgOutcome {
    subtract_many(host, std::slice::from_ref(cutter))
}

/// Subtract the union of `cutters` from `host`, all in the shared
/// working frame (see the module **Frame contract**).
///
/// Returns `NotParametric` unless every operand is a perpendicular
/// extrusion, every cutter's sweep axis is parallel to the host's, and
/// every overlapping cutter is a *through-cut* (spans the host's full
/// sweep range). Partial / pocket cuts and oblique / cross-axis cutters
/// fall back to manifold via `NotParametric`.
pub fn subtract_many(host: &PrismParams, cutters: &[PrismParams]) -> PrismCsgOutcome {
    // --- Host must be a perpendicular extrusion with real depth. ------
    let Some(host_frame) = SweepFrame::from_prism(host) else {
        return PrismCsgOutcome::NotParametric;
    };
    if host.depth.abs() <= SWEEP_EPS {
        return PrismCsgOutcome::NotParametric;
    }

    // Host footprint in the sweep-frame basis; host sweep range [0, H].
    let host_2d = host_frame.project_profile(host);
    let host_depth = host.depth.abs();

    // --- Gather through-cut cutter footprints. ------------------------
    let mut clip_shapes: Vec<polygon_bool::Shape> = Vec::new();
    let mut any_overlap = false;
    for cutter in cutters {
        let Some(cut_frame) = SweepFrame::from_prism(cutter) else {
            return PrismCsgOutcome::NotParametric;
        };
        // Sweep axes must be (anti)parallel to the host's.
        if host_frame.axis.dot(cut_frame.axis).abs() < 1.0 - AXIS_EPS {
            return PrismCsgOutcome::NotParametric;
        }
        // Cutter sweep interval expressed along the HOST axis, relative
        // to the host base plane (s = 0). The cutter base/top project to
        // two scalars; normalise lo < hi.
        let (s_lo, s_hi) = cutter_sweep_interval(&host_frame, cutter, &cut_frame);
        let lo = s_lo.max(0.0);
        let hi = s_hi.min(host_depth);
        if hi <= lo + SWEEP_EPS {
            // No overlap along the sweep — this cutter is irrelevant.
            continue;
        }
        any_overlap = true;
        // Through-cut requirement: the cutter must span the host's full
        // range. A partial recess / pocket needs slab decomposition we
        // don't do yet → bail to manifold rather than emit a wrong cut.
        let through = s_lo <= SWEEP_EPS && s_hi >= host_depth - SWEEP_EPS;
        if !through {
            return PrismCsgOutcome::NotParametric;
        }
        // Cutter footprint projected onto the HOST sweep basis.
        let cut_poly = host_frame.project_profile(cutter);
        clip_shapes.push(polygon_bool::shape_from_polygon2d(&cut_poly));
    }

    if !any_overlap {
        return PrismCsgOutcome::Unchanged;
    }

    // --- 2D difference, then re-extrude over the host sweep range. ----
    let host_shape = polygon_bool::shape_from_polygon2d(&host_2d);
    let result = polygon_bool::difference(&host_shape, &clip_shapes);
    if result.is_empty() {
        return PrismCsgOutcome::Empty;
    }

    let out_xform = host_frame.basis_to_frame();
    let mut mesh = LocalMesh::new();
    for shape in &result {
        let poly = polygon_bool::polygon2d_from_shape(shape);
        if poly.outer.len() < 3 {
            continue;
        }
        let sub = extrude_polygon(&poly, Vec3::Z, host_depth, out_xform);
        append_mesh(&mut mesh, &sub);
    }
    if mesh.indices.is_empty() {
        return PrismCsgOutcome::Empty;
    }
    PrismCsgOutcome::Cut(mesh)
}

/// Orthonormal sweep frame `(e1, e2, axis)` with origin `base` (a point
/// on the prism's base plane), all in the shared working frame. The
/// profile plane is perpendicular to `axis` (enforced at construction),
/// so projecting profile vertices onto `(e1, e2)` is a rigid map.
struct SweepFrame {
    e1: Vec3,
    e2: Vec3,
    axis: Vec3,
    base: Vec3,
}

impl SweepFrame {
    /// Build the sweep frame for a prism, or `None` if the extrusion is
    /// not perpendicular (oblique → footprint would shear) or the
    /// placement is degenerate.
    fn from_prism(p: &PrismParams) -> Option<SweepFrame> {
        let dir = p.dir.normalize_or_zero();
        if dir.length_squared() < 0.5 {
            return None;
        }
        // Perpendicular extrusion: dir ≈ ±local Z (in-plane part ~0).
        if (dir.x * dir.x + dir.y * dir.y).sqrt() > PERP_EPS {
            return None;
        }
        // World sweep axis = local_xform rotation applied to dir.
        let axis = transform_vec(&p.local_xform, dir).normalize_or_zero();
        if axis.length_squared() < 0.5 {
            return None;
        }
        // Base-plane point: the local origin (profile sits at z=0).
        let base = transform_point(&p.local_xform, Vec3::ZERO);
        // Deterministic in-plane basis ⟂ axis.
        let helper = if axis.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        let e1 = axis.cross(helper).normalize();
        let e2 = axis.cross(e1).normalize();
        Some(SweepFrame { e1, e2, axis, base })
    }

    /// Project this prism's own profile onto `(e1, e2)` → `Polygon2D`.
    fn project_profile(&self, p: &PrismParams) -> Polygon2D {
        let f = |v: &Vec2| self.project_point(transform_point(&p.local_xform, Vec3::new(v.x, v.y, 0.0)));
        Polygon2D {
            outer: p.profile.outer.iter().map(f).collect(),
            holes: p.profile.holes.iter().map(|h| h.iter().map(f).collect()).collect(),
        }
    }

    /// Project a working-frame point onto `(e1, e2)` relative to `base`.
    fn project_point(&self, world: Vec3) -> Vec2 {
        let d = world - self.base;
        Vec2::new(d.dot(self.e1), d.dot(self.e2))
    }

    /// 4×4 mapping `(u, v, w, 1)` in the sweep basis back into the
    /// working frame: `base + u·e1 + v·e2 + w·axis`. Feeding this to
    /// `extrude_polygon` as `local_xform` (with `dir = +Z`) rebuilds the
    /// net solid in the shared working frame.
    fn basis_to_frame(&self) -> Mat4 {
        Mat4::from_cols(
            Vec4::new(self.e1.x, self.e1.y, self.e1.z, 0.0),
            Vec4::new(self.e2.x, self.e2.y, self.e2.z, 0.0),
            Vec4::new(self.axis.x, self.axis.y, self.axis.z, 0.0),
            Vec4::new(self.base.x, self.base.y, self.base.z, 1.0),
        )
    }
}

/// Cutter sweep interval `[s_lo, s_hi]` along the HOST sweep axis,
/// measured from the host base plane (host base ⇒ s = 0). Uses the
/// cutter's base-plane point and its top (base + depth·cutter_axis),
/// projected onto the host axis. Returned normalised so `s_lo ≤ s_hi`.
fn cutter_sweep_interval(
    host_frame: &SweepFrame,
    cutter: &PrismParams,
    cut_frame: &SweepFrame,
) -> (f32, f32) {
    let base_s = (cut_frame.base - host_frame.base).dot(host_frame.axis);
    let top = cut_frame.base + cut_frame.axis * cutter.depth.abs();
    let top_s = (top - host_frame.base).dot(host_frame.axis);
    (base_s.min(top_s), base_s.max(top_s))
}

fn transform_point(m: &Mat4, p: Vec3) -> Vec3 {
    let r = *m * Vec4::new(p.x, p.y, p.z, 1.0);
    Vec3::new(r.x, r.y, r.z)
}

fn transform_vec(m: &Mat4, v: Vec3) -> Vec3 {
    let r = *m * Vec4::new(v.x, v.y, v.z, 0.0);
    Vec3::new(r.x, r.y, r.z)
}

/// Append `src`'s vertices/indices onto `dst`, shifting indices. Seams
/// between sub-shapes are left as coincident-but-unwelded vertices —
/// fine for the volume / render consumers; a weld pass is a follow-up
/// if a downstream manifold check needs it.
fn append_mesh(dst: &mut LocalMesh, src: &LocalMesh) {
    let base = (dst.vertices.len() / 3) as u32;
    dst.vertices.extend_from_slice(&src.vertices);
    dst.indices.extend(src.indices.iter().map(|i| i + base));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis-aligned rectangular profile centred at the origin, side
    /// `sx × sy`, wound CCW.
    fn rect_profile(sx: f32, sy: f32) -> Polygon2D {
        let (hx, hy) = (sx / 2.0, sy / 2.0);
        Polygon2D {
            outer: vec![
                Vec2::new(-hx, -hy),
                Vec2::new(hx, -hy),
                Vec2::new(hx, hy),
                Vec2::new(-hx, hy),
            ],
            holes: Vec::new(),
        }
    }

    /// Z-extruded prism at a given base translation, identity rotation.
    fn z_prism(profile: Polygon2D, depth: f32, base: Vec3) -> PrismParams {
        PrismParams {
            profile,
            dir: Vec3::Z,
            depth,
            local_xform: Mat4::from_translation(base),
        }
    }

    /// Signed mesh volume (closed outward-CCW ⇒ +enclosed volume).
    fn signed_volume(m: &LocalMesh) -> f64 {
        let v = &m.vertices;
        let mut v6 = 0.0_f64;
        for t in m.indices.chunks_exact(3) {
            let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
            let p = |i: usize| {
                (
                    v[i * 3] as f64,
                    v[i * 3 + 1] as f64,
                    v[i * 3 + 2] as f64,
                )
            };
            let (ax, ay, az) = p(a);
            let (bx, by, bz) = p(b);
            let (cx, cy, cz) = p(c);
            v6 += ax * (by * cz - bz * cy) + ay * (bz * cx - bx * cz) + az * (bx * cy - by * cx);
        }
        (v6 / 6.0).abs()
    }

    fn expect_cut(o: PrismCsgOutcome) -> LocalMesh {
        match o {
            PrismCsgOutcome::Cut(m) => m,
            other => panic!("expected Cut, got {other:?}"),
        }
    }

    #[test]
    fn through_cut_centered_window() {
        // 4×4×3 host, 1×1×3 cutter spanning the full height, centred.
        let host = z_prism(rect_profile(4.0, 4.0), 3.0, Vec3::ZERO);
        let cutter = z_prism(rect_profile(1.0, 1.0), 3.0, Vec3::ZERO);
        let mesh = expect_cut(subtract(&host, &cutter));
        // (16 − 1) × 3 = 45.
        let vol = signed_volume(&mesh);
        assert!((vol - 45.0).abs() < 1e-3, "volume {vol}, expected 45");
    }

    #[test]
    fn oversized_cutter_clamps_to_host_sweep() {
        // Cutter taller than host and protruding both faces — still a
        // through-cut; volume must clamp to the host's 3-unit range.
        let host = z_prism(rect_profile(4.0, 4.0), 3.0, Vec3::ZERO);
        let cutter = z_prism(rect_profile(1.0, 1.0), 10.0, Vec3::new(0.0, 0.0, -2.0));
        let mesh = expect_cut(subtract(&host, &cutter));
        let vol = signed_volume(&mesh);
        assert!((vol - 45.0).abs() < 1e-3, "volume {vol}, expected 45");
    }

    #[test]
    fn xy_disjoint_cutter_preserves_host() {
        // Cutter overlaps the host's sweep range but its footprint is
        // outside the host footprint → 2D difference leaves host intact.
        let host = z_prism(rect_profile(4.0, 4.0), 3.0, Vec3::ZERO);
        let cutter = z_prism(rect_profile(1.0, 1.0), 3.0, Vec3::new(20.0, 0.0, 0.0));
        let mesh = expect_cut(subtract(&host, &cutter));
        let vol = signed_volume(&mesh);
        assert!((vol - 48.0).abs() < 1e-3, "volume {vol}, expected 48 (host)");
    }

    #[test]
    fn sweep_disjoint_cutter_is_unchanged() {
        // Cutter sits entirely above the host along the sweep axis.
        let host = z_prism(rect_profile(4.0, 4.0), 3.0, Vec3::ZERO);
        let cutter = z_prism(rect_profile(1.0, 1.0), 2.0, Vec3::new(0.0, 0.0, 5.0));
        assert!(matches!(subtract(&host, &cutter), PrismCsgOutcome::Unchanged));
    }

    #[test]
    fn cutter_consuming_host_is_empty() {
        let host = z_prism(rect_profile(2.0, 2.0), 3.0, Vec3::ZERO);
        let cutter = z_prism(rect_profile(5.0, 5.0), 3.0, Vec3::ZERO);
        assert!(matches!(subtract(&host, &cutter), PrismCsgOutcome::Empty));
    }

    #[test]
    fn partial_recess_falls_back() {
        // Cutter ends inside the host (s_hi = 1.5 < host depth 3) →
        // not a through-cut → NotParametric (manifold fallback).
        let host = z_prism(rect_profile(4.0, 4.0), 3.0, Vec3::ZERO);
        let cutter = z_prism(rect_profile(1.0, 1.0), 1.5, Vec3::ZERO);
        assert!(matches!(subtract(&host, &cutter), PrismCsgOutcome::NotParametric));
    }

    #[test]
    fn non_parallel_axis_falls_back() {
        let host = z_prism(rect_profile(4.0, 4.0), 3.0, Vec3::ZERO);
        // Cutter swept along a rotated axis (45° about X) → not parallel.
        let mut cutter = z_prism(rect_profile(1.0, 1.0), 3.0, Vec3::ZERO);
        cutter.local_xform = Mat4::from_rotation_x(std::f32::consts::FRAC_PI_4);
        assert!(matches!(subtract(&host, &cutter), PrismCsgOutcome::NotParametric));
    }

    #[test]
    fn oblique_extrusion_falls_back() {
        let host = z_prism(rect_profile(4.0, 4.0), 3.0, Vec3::ZERO);
        let mut cutter = z_prism(rect_profile(1.0, 1.0), 3.0, Vec3::ZERO);
        cutter.dir = Vec3::new(0.3, 0.0, 1.0).normalize(); // oblique
        assert!(matches!(subtract(&host, &cutter), PrismCsgOutcome::NotParametric));
    }

    #[test]
    fn multi_cutter_two_windows() {
        // Two disjoint 1×1 through-cuts in a 6×4×3 host.
        let host = z_prism(rect_profile(6.0, 4.0), 3.0, Vec3::ZERO);
        let c1 = z_prism(rect_profile(1.0, 1.0), 3.0, Vec3::new(-1.5, 0.0, 0.0));
        let c2 = z_prism(rect_profile(1.0, 1.0), 3.0, Vec3::new(1.5, 0.0, 0.0));
        let mesh = expect_cut(subtract_many(&host, &[c1, c2]));
        // (24 − 1 − 1) × 3 = 66.
        let vol = signed_volume(&mesh);
        assert!((vol - 66.0).abs() < 1e-3, "volume {vol}, expected 66");
    }

    #[test]
    fn offset_host_frame_still_correct() {
        // Host placed away from origin (within the rebased frame) — the
        // sweep-frame projection must stay translation-correct.
        let host = z_prism(rect_profile(4.0, 4.0), 3.0, Vec3::new(8.0, -5.0, 2.0));
        let cutter = z_prism(rect_profile(1.0, 1.0), 3.0, Vec3::new(8.0, -5.0, 2.0));
        let mesh = expect_cut(subtract(&host, &cutter));
        let vol = signed_volume(&mesh);
        assert!((vol - 45.0).abs() < 1e-3, "volume {vol}, expected 45");
    }

    #[test]
    fn rebase_params_offsets_translation() {
        let p = z_prism(rect_profile(1.0, 1.0), 3.0, Vec3::ZERO);
        let r = rebase_params(&p, [600_001.5, 10.0, 4.0], [600_000.0, 10.0, 0.0]);
        // Only the small anchor DIFFERENCE (1.5, 0, 4) lands in the matrix.
        assert!((r.local_xform.w_axis.x - 1.5).abs() < 1e-4);
        assert!((r.local_xform.w_axis.y - 0.0).abs() < 1e-4);
        assert!((r.local_xform.w_axis.z - 4.0).abs() < 1e-4);
    }

    #[test]
    fn cross_product_rebase_is_far_origin_safe() {
        // The F3 gate. Two products at UTM scale (~600 km east), 1.5 m
        // apart. Each prism lives in its OWN anchor-local frame (near
        // origin). Rebasing the cutter into the host's anchor frame must
        // keep the math near origin so f32 doesn't collapse — the UTM
        // magnitude only ever appears as a small anchor DIFFERENCE.
        let host_anchor = [600_000.0_f64, 10.0, 0.0];
        let cutter_anchor = [600_001.5_f64, 10.0, 0.0]; // 1.5 m east
        let host = z_prism(rect_profile(4.0, 4.0), 3.0, Vec3::ZERO);
        let cutter_local = z_prism(rect_profile(1.0, 1.0), 3.0, Vec3::ZERO);
        let cutter = rebase_params(&cutter_local, cutter_anchor, host_anchor);
        // Cutter now at +1.5 east in the host frame: footprint x∈[1,2],
        // inside the host's x∈[-2,2] → 1×1 through-cut → (16−1)×3 = 45.
        let mesh = expect_cut(subtract(&host, &cutter));
        let vol = signed_volume(&mesh);
        assert!(vol.is_finite(), "F3 collapse: non-finite volume");
        assert!((vol - 45.0).abs() < 1e-3, "far-origin rebase volume {vol}, expected 45");
    }

    #[test]
    fn extrude_params_converts_to_prism_params() {
        let ep = ExtrudeParams {
            profile: rect_profile(2.0, 2.0),
            dir: Vec3::Z,
            depth: 1.5,
            local_xform: Mat4::IDENTITY,
        };
        let pp: PrismParams = ep.into();
        assert_eq!(pp.profile.outer.len(), 4);
        assert!((pp.depth - 1.5).abs() < 1e-6);
    }
}
