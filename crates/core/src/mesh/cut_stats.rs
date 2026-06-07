//! Pure-data outcome + counter types for the cut-openings pipeline.
//!
//! These types are **not** csg-gated. They live here, separate from the
//! [`crate::mesh::cut_openings`] module (which IS `#[cfg(feature =
//! "csg")]`), so that mesh-only / prism-only builds can still carry and
//! marshal the per-pass counters. Before this split (GH #61),
//! `CutOpeningsStats` lived inside `cut_openings`; the PyO3 layer
//! (`#[cfg(feature = "python")]`) and the three mesh-gated entry points
//! (`mesh_qto`, `extract_meshes`, `write_gltf`) referenced it
//! unconditionally, so `--features python,mesh` without `csg` failed to
//! compile with `error[E0433]: cannot find cut_openings in mesh`.
//!
//! Keeping them here means `crate::mesh::cut_stats::CutOpeningsStats`
//! resolves in every `mesh` build; the csg-gated `cut_openings` module
//! re-exports them so existing `cut_openings::{Outcome, ...}` paths
//! (and the csg-gated integration tests) keep working unchanged.
//!
//! Design contract carried over from the W2 taxonomy: flat counter
//! fields (not a `HashMap`) keep the FFI dict shape stable and cheap,
//! and `UnsupportedReason` is `#[non_exhaustive]` so adding variants is
//! not a breaking change for external matchers.

/// Outcome counters from a cut pass. Aggregate across products in
/// the caller's loop for a top-line "n products had their openings
/// cut, m fell back to reveal-all" report.
///
/// The `unsupported_*` fields ([W2] — see
/// `docs/plans/2026-06-05_cut-openings-manifold-replacement.md`) are
/// per-reason counters for `Outcome::Unsupported(UnsupportedReason)`
/// emissions. Today most of them are zero — the detection paths land
/// over W3 (validation gate) and W11 (brep pre-flight); the slots are
/// surfaced now so downstream agents and the PyO3 stats dict carry the
/// vocabulary the moment any variant starts being emitted. Flat fields
/// (not a HashMap) keep the FFI cheap and the parquet column shape
/// stable across versions.
#[derive(Debug, Default, Clone, Copy)]
pub struct CutOpeningsStats {
    /// Products where at least one `boolean_second_operand` segment
    /// existed AND the cut succeeded — the output mesh is the net
    /// solid.
    pub cut: usize,
    /// Products that had no cutter segments — the mesh is unchanged,
    /// returned through verbatim.
    pub passthrough: usize,
    /// Products that had cutter segments but the CSG operation
    /// failed in an unrecognised way (catch-all). Preserved as the
    /// reveal-all fallback signal; typed variants live in the
    /// `unsupported_*` fields and represent recognised failures.
    pub fallback: usize,
    // ---- Outcome::Unsupported(UnsupportedReason) counters ----
    pub unsupported_non_manifold_input: usize,
    pub unsupported_self_intersecting_cutter: usize,
    pub unsupported_coplanar_face_degeneracy: usize,
    pub unsupported_kernel_internal_error: usize,
    pub unsupported_curved_surface_approximated: usize,
    pub unsupported_intersection_not_implemented: usize,
    pub unsupported_union_with_overlap: usize,
    pub unsupported_non_planar_base_surface: usize,
    pub unsupported_unhandled_cutter_entity: usize,
    pub unsupported_malformed_host: usize,
    pub unsupported_bsp_depth_exceeded: usize,
    pub unsupported_tight_polygonal_boundary_ignored: usize,
    pub unsupported_degenerate_cutter: usize,
    pub unsupported_host_consumed: usize,
}

/// Outcome of one cut attempt on a single product.
///
/// `Fallback` stays as the catch-all "we could not cut this and have
/// no diagnostic" bucket; the original mesh is left in place
/// (reveal-all is the safe behaviour). `Unsupported(reason)` is the
/// typed-diagnostic counterpart — same reveal-all effect on the mesh,
/// but the reason carries a categorical signal callers and substrate
/// consumers can route on. See [`UnsupportedReason`] for the
/// vocabulary and `docs/plans/2026-06-05_cut-openings-manifold-replacement.md`
/// for the staged rollout that fills each reason with detection code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Cut,
    Passthrough,
    Fallback,
    Unsupported(UnsupportedReason),
}

/// Why a cut could not be performed and the host was left as-is.
///
/// Most variants are not emitted yet — they exist as the vocabulary
/// that downstream W3 (validation gate) / W11 (brep pre-flight) /
/// W4 (operator-aware IfcBooleanResult) will fill in. Marked
/// `#[non_exhaustive]` so adding future variants is not a breaking
/// change for external matchers.
///
/// Each variant maps to a flat `unsupported_*` counter on
/// [`CutOpeningsStats`]; the dispatch lives in [`Outcome::accumulate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[allow(dead_code)]
pub enum UnsupportedReason {
    /// Host or cutter mesh has open edges / non-manifold topology
    /// (e.g. an open shell from `IfcFaceBasedSurfaceModel`). Wired by
    /// W3 + W11.
    NonManifoldInput,
    /// Cutter mesh self-intersects (Revit occasionally emits these).
    /// Wired by W11.
    SelfIntersectingCutter,
    /// Host and cutter share a face exactly (sub-tolerance), the
    /// CSG kernel cannot decide which side to keep. Wired by W3.
    CoplanarFaceDegeneracy,
    /// The underlying CSG kernel returned an error that does not map
    /// to any other variant (manifold-csg `Err(_)`, future csgrs
    /// internal panic caught and converted). Wired immediately after
    /// validators are in place; today these stay [`Outcome::Fallback`].
    KernelInternalError,
    /// `IfcAdvancedBrep` with NURBS-bounded faces was approximated
    /// with a planar facet — the cut would be wrong on the curved
    /// surface. Wired by W17.
    CurvedSurfaceApproximated,
    /// `IfcBooleanResult` with operator `.INTERSECTION.` — the spec
    /// requires actual intersection geometry; we don't compute it.
    /// Wired by W4.
    IntersectionNotImplemented,
    /// `IfcBooleanResult` with operator `.UNION.` between overlapping
    /// solids — we don't compute the union, would over-report volume
    /// by 2× if treated as reveal-all. Wired by W4.
    UnionWithOverlap,
    /// `IfcHalfSpaceSolid.BaseSurface` is not an `IfcPlane`
    /// (cylindrical / spherical / B-spline). Wired by W6 hardening.
    NonPlanarBaseSurface,
    /// Cutter is an IFC entity the mesher does not handle
    /// (`IfcSweptDiskSolid`, `IfcSurfaceCurveSweptAreaSolid`, etc.).
    /// The `&'static str` carries the IFC entity name for telemetry.
    /// Wired by W8.
    UnhandledCutterEntity(&'static str),
    /// Host representation is malformed (e.g. `IfcGeometricCurveSet`
    /// as a wall body). Wired by W3.
    MalformedHost,
    /// The composite-boolean recursion exceeded the dispatcher cap
    /// (`MAX_BOOLEAN_DEPTH`). Wired by W3.
    BspDepthExceeded,
    /// `IfcPolygonalBoundedHalfSpace` had a tight polygonal boundary
    /// that the current infinite-plane clipper ignored — the cut is
    /// over-aggressive. Wired by W6.
    TightPolygonalBoundaryIgnored,
    /// Cutter has zero or near-zero volume (degenerate profile or
    /// zero-extrude length). Wired by W11.
    DegenerateCutter,
    /// The cut consumed the host entirely (legitimate, but worth
    /// surfacing for downstream QTO — the host is gone from the
    /// output). Wired by W3.
    HostConsumed,
}

impl Outcome {
    pub fn accumulate(self, stats: &mut CutOpeningsStats) {
        use UnsupportedReason::*;
        match self {
            Self::Cut => stats.cut += 1,
            Self::Passthrough => stats.passthrough += 1,
            Self::Fallback => stats.fallback += 1,
            Self::Unsupported(reason) => match reason {
                NonManifoldInput => stats.unsupported_non_manifold_input += 1,
                SelfIntersectingCutter => stats.unsupported_self_intersecting_cutter += 1,
                CoplanarFaceDegeneracy => stats.unsupported_coplanar_face_degeneracy += 1,
                KernelInternalError => stats.unsupported_kernel_internal_error += 1,
                CurvedSurfaceApproximated => stats.unsupported_curved_surface_approximated += 1,
                IntersectionNotImplemented => stats.unsupported_intersection_not_implemented += 1,
                UnionWithOverlap => stats.unsupported_union_with_overlap += 1,
                NonPlanarBaseSurface => stats.unsupported_non_planar_base_surface += 1,
                UnhandledCutterEntity(_) => stats.unsupported_unhandled_cutter_entity += 1,
                MalformedHost => stats.unsupported_malformed_host += 1,
                BspDepthExceeded => stats.unsupported_bsp_depth_exceeded += 1,
                TightPolygonalBoundaryIgnored => {
                    stats.unsupported_tight_polygonal_boundary_ignored += 1
                }
                DegenerateCutter => stats.unsupported_degenerate_cutter += 1,
                HostConsumed => stats.unsupported_host_consumed += 1,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_accumulator_threads_outcomes() {
        let mut stats = CutOpeningsStats::default();
        Outcome::Cut.accumulate(&mut stats);
        Outcome::Cut.accumulate(&mut stats);
        Outcome::Passthrough.accumulate(&mut stats);
        Outcome::Fallback.accumulate(&mut stats);
        Outcome::Unsupported(UnsupportedReason::UnionWithOverlap).accumulate(&mut stats);
        Outcome::Unsupported(UnsupportedReason::TightPolygonalBoundaryIgnored)
            .accumulate(&mut stats);
        assert_eq!(stats.cut, 2);
        assert_eq!(stats.passthrough, 1);
        assert_eq!(stats.fallback, 1);
        assert_eq!(stats.unsupported_union_with_overlap, 1);
        assert_eq!(stats.unsupported_tight_polygonal_boundary_ignored, 1);
    }
}
