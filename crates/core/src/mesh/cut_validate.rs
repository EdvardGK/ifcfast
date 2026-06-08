//! Cross-cutting validation + tolerance policy for the cut-openings
//! pipeline ([GH #58] / W3 — see
//! `docs/plans/2026-06-05_cut-openings-manifold-replacement.md`).
//!
//! Two concerns live here, both shared across the half-space clip and
//! the manifold subtract paths so the policy is single-sourced:
//!
//! 1. **Clip on-plane round-off guard** ([F2] / [GH #65]). The half-space
//!    clipper treats a vertex within `eps` of the cutting plane as "on
//!    the plane → outside". This `eps` is a *numerical* round-off guard
//!    in the mesh's SOURCE units — NOT a physical building tolerance.
//!    [`on_plane_eps`] returns the validated `1e-3` source-unit guard
//!    (metre + millimetre baselines since v0.4.32), tightening only for
//!    large-unit (km-scale) files so the band can never exceed a
//!    physical millimetre.
//!
//!    W3 ([GH #58]) briefly reframed this as a *physical* 1 mm
//!    (`BASE_M / unit_scale`). That is 1.0 source units in a millimetre
//!    file — coarse enough that near-plane faces on a multi-clipped wall
//!    are classified "outside" and dropped WITHOUT a replacement cap,
//!    leaving an open shell whose volume over-integrates. That re-opened
//!    the #39 half-space over-report on every mm-unit model (GH #65:
//!    Sannergata ARK_E, +6 %…+136 %). Metre files were byte-identical
//!    under W3 (`unit_scale == 1.0`), which hid the regression from the
//!    metre-scale proptest. The guard is back to source units; metre /
//!    mm / foot all resolve to `1e-3`, identical to the pre-W3 clip.
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

/// Resolve the half-space clip's "on-plane" round-off guard, in the
/// model's SOURCE units, from its `unit_scale` (the source→metres
/// factor: mm→0.001, m→1.0, ft→0.3048).
///
/// This is a **numerical** guard, not a physical building tolerance: a
/// vertex within `eps` of the cutting plane is snapped to it. It must
/// stay well below feature size (wall thickness) or near-plane faces are
/// dropped without a replacement cap, leaving an open shell whose volume
/// over-integrates ([GH #65]). The validated value is `1e-3` source
/// units (metre + mm baselines since v0.4.32):
///
/// | unit  | `unit_scale` | returned eps (source units) | physical  |
/// |-------|--------------|-----------------------------|-----------|
/// | metre | 1.0          | 1e-3                        | 1 mm      |
/// | mm    | 0.001        | 1e-3                        | 0.001 mm  |
/// | foot  | 0.3048       | 1e-3                        | 0.0003 m  |
/// | km    | 1000.0       | 1e-6                        | 1 mm      |
///
/// For large-unit files (`unit_scale > 1`) the physical-millimetre cap
/// `BASE_M / unit_scale` is smaller than the `1e-3` source guard and
/// wins, so the band never exceeds a physical millimetre — still far
/// below feature size at km scale. A non-finite or non-positive
/// `unit_scale` (corrupt header) falls back to the source guard rather
/// than producing NaN/∞ — fail safe, not silent-wrong.
///
/// A coordinate-magnitude-relative guard (eps ∝ local extent · f32 ulp)
/// is the principled long-term form; the source-unit constant is the
/// validated, byte-identical-to-v0.4.32 choice and is tracked for a
/// follow-up.
pub fn on_plane_eps(unit_scale: f32) -> f32 {
    // Source-unit numerical guard, validated on metre + mm files.
    const NUMERICAL_GUARD_SRC: f32 = 1.0e-3;
    // Physical 1 mm expressed in source units; only binds (is smaller)
    // for large-unit files, where it keeps the band sub-millimetre.
    let physical_mm_in_src = if unit_scale.is_finite() && unit_scale > 1.0e-12 {
        ON_PLANE_EPS_BASE_M / unit_scale
    } else {
        NUMERICAL_GUARD_SRC
    };
    NUMERICAL_GUARD_SRC.min(physical_mm_in_src)
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
    fn eps_is_numerical_guard_in_source_units() {
        // GH #65: the clip on-plane eps is a NUMERICAL round-off guard in
        // SOURCE units (capped at 1e-3), NOT a physical 1 mm. A physical
        // 1 mm (W3) is 1.0 source units in a millimetre file — coarse
        // enough to drop near-plane faces without a replacement cap and
        // re-open the #39 half-space over-report. Every common unit
        // resolves to the validated 1e-3 source guard (byte-identical to
        // the pre-W3 clip on metre + mm + foot).
        for us in [1.0_f32, 0.001, 0.3048, 0.01] {
            let eps = on_plane_eps(us);
            assert!(
                (eps - 1.0e-3).abs() < 1e-9,
                "eps for unit_scale {us} = {eps} (want 1e-3 source units)"
            );
        }

        // Large-unit (km-scale) files tighten below the source guard so
        // the band can never exceed a physical millimetre.
        let km = on_plane_eps(1000.0);
        assert!(km < 1.0e-3 && km > 0.0, "km eps = {km} (want sub-1e-3)");
        assert!((km - 1.0e-6).abs() < 1e-12, "km eps = {km} (want 1e-6 = 1 mm)");
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
