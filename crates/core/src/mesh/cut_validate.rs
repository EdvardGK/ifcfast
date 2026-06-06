//! Cross-cutting validation + tolerance policy for the cut-openings
//! pipeline ([GH #58] / W3 — see
//! `docs/plans/2026-06-05_cut-openings-manifold-replacement.md`).
//!
//! Two concerns live here, both shared across the half-space clip and
//! the manifold subtract paths so the policy is single-sourced:
//!
//! 1. **Unit-aware geometric tolerance** ([F2]). The half-space clipper
//!    treats a vertex within `eps` of the cutting plane as "on the
//!    plane". The raw constant
//!    [`halfspace_clip::ON_PLANE_EPS_BASE_M`] is a *physical* 1 mm,
//!    expressed for a model authored in metres. A model in millimetres
//!    has vertices ~1000× larger, one in feet ~3.3× smaller — so the
//!    SAME numeric epsilon means three different physical tolerances.
//!    [`on_plane_eps`] divides the physical target by `unit_scale`
//!    (source→metres) so the tolerance stays a consistent 1 mm in any
//!    unit system. For a metre model `unit_scale == 1.0` and the value
//!    is unchanged — the established Sannergata baseline behaviour is
//!    preserved exactly; only mm / imperial models change, and that is
//!    the bug fix.
//!
//! 2. **Input topology classification** ([NonManifoldInput]). When a
//!    manifold subtract fails, [`is_manifold_mesh`] lets the caller
//!    attribute the failure to non-manifold input (open shells,
//!    odd-paired edges — the Revit "bad opening solid" reality) and
//!    surface `Outcome::Unsupported(NonManifoldInput)` instead of an
//!    opaque `Fallback`. It is used as a *post-failure classifier*, not
//!    a pre-gate: the check is conservative (it can false-negative on a
//!    visually-closed mesh whose vertices aren't index-welded), so
//!    using it to *skip* a cut could suppress a real opening. Running
//!    it only after the kernel has already rejected the input makes the
//!    false-negative harmless — the kernel, not this check, decides
//!    whether the cut happens.

use crate::mesh::halfspace_clip::ON_PLANE_EPS_BASE_M;

/// Resolve the half-space "on-plane" epsilon in the model's source
/// units from its `unit_scale` (the source→metres factor:
/// mm→0.001, m→1.0, ft→0.3048).
///
/// The returned value is [`ON_PLANE_EPS_BASE_M`] (a physical 1 mm)
/// expressed in source units, so the snap tolerance is physically
/// constant regardless of how the file declares its length unit:
///
/// | unit  | `unit_scale` | returned eps (source units) | physical |
/// |-------|--------------|-----------------------------|----------|
/// | metre | 1.0          | 1e-3                        | 1 mm     |
/// | mm    | 0.001        | 1.0                         | 1 mm     |
/// | foot  | 0.3048       | 3.28e-3                     | 1 mm     |
///
/// A non-finite or non-positive `unit_scale` (corrupt header) falls
/// back to the base value rather than producing NaN/∞ — fail safe,
/// not silent-wrong.
pub fn on_plane_eps(unit_scale: f32) -> f32 {
    if unit_scale.is_finite() && unit_scale > 1.0e-12 {
        ON_PLANE_EPS_BASE_M / unit_scale
    } else {
        ON_PLANE_EPS_BASE_M
    }
}

/// `true` if `indices` describes a closed 2-manifold (every undirected
/// edge shared by exactly two triangles, with opposite orientation).
///
/// Thin wrapper over [`crate::mesh::qto::is_closed_manifold`] named for
/// its role in the cut pipeline: classifying *why* a manifold subtract
/// failed. See the module docs for why this is a post-failure
/// classifier and not a pre-gate.
pub fn is_manifold_mesh(indices: &[u32]) -> bool {
    crate::mesh::qto::is_closed_manifold(indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eps_is_physically_constant_across_units() {
        // 1 mm physical in every unit system, expressed in source units.
        let metre = on_plane_eps(1.0);
        let mm = on_plane_eps(0.001);
        let foot = on_plane_eps(0.3048);

        // metre model: unchanged from the historical hard-coded value.
        assert!((metre - 1.0e-3).abs() < 1e-12, "metre eps = {metre}");
        // mm model: 1 mm = 1.0 source unit.
        assert!((mm - 1.0).abs() < 1e-9, "mm eps = {mm}");
        // foot model: 1 mm ≈ 0.00328 ft.
        assert!((foot - 1.0e-3 / 0.3048).abs() < 1e-9, "foot eps = {foot}");

        // The physical tolerance (eps * unit_scale, in metres) is the
        // same 1 mm everywhere — that is the whole point of F2.
        for (eps, us) in [(metre, 1.0_f32), (mm, 0.001), (foot, 0.3048)] {
            let physical_m = eps * us;
            assert!(
                (physical_m - 1.0e-3).abs() < 1e-9,
                "physical tolerance drifted: {physical_m} m for unit_scale {us}"
            );
        }
    }

    #[test]
    fn eps_falls_back_on_garbage_unit_scale() {
        assert_eq!(on_plane_eps(0.0), ON_PLANE_EPS_BASE_M);
        assert_eq!(on_plane_eps(-1.0), ON_PLANE_EPS_BASE_M);
        assert_eq!(on_plane_eps(f32::NAN), ON_PLANE_EPS_BASE_M);
        assert_eq!(on_plane_eps(f32::INFINITY), ON_PLANE_EPS_BASE_M);
    }

    #[test]
    fn manifold_check_matches_qto() {
        // Closed cube (index-welded) is manifold; an open strip is not.
        let cube_idx: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 2, 3, 7, 2, 7, 6, 1, 2, 6, 1, 6,
            5, 0, 4, 7, 0, 7, 3,
        ];
        assert!(is_manifold_mesh(&cube_idx));
        // Single triangle — three edges each used once → not closed.
        assert!(!is_manifold_mesh(&[0, 1, 2]));
    }
}
