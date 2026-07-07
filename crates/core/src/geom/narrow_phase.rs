//! Narrow-phase mesh-mesh queries.
//!
//! Once the broad phase has reduced "everything against everything" to
//! a candidate pair list, the narrow phase answers the real questions:
//!
//! * **`intersects`** — does this pair of solids actually overlap, as
//!   opposed to just bounding-box overlap? (False positives in the
//!   broad phase get filtered out here. A column and a beam can share
//!   AABBs without their tessellated bodies actually touching.)
//! * **`min_distance`** — the minimum distance between the two meshes.
//!   Zero means they intersect; positive means clearance. This is what
//!   "soft clash" / tolerance-band queries consume to classify
//!   `clearance` vs `clash`.
//!
//! All queries assume the meshes are already in world coordinates —
//! the kernel passes `Isometry::identity()` to parry. ifcfast's mesh
//! extractor already bakes world coords into `ProductMesh.vertices`
//! when run in `BakeFrame::World`, so this is the natural form.

use parry3d::math::Isometry;
use parry3d::query;
use parry3d::shape::TriMesh;

/// Errors from parry queries. parry occasionally returns
/// `Unsupported` for shape pairs it doesn't know how to handle —
/// shouldn't fire for TriMesh-vs-TriMesh, but we surface it instead
/// of unwrapping in case a future kernel revision changes that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NarrowPhaseError {
    /// parry's `intersection_test` / `distance` returned `Unsupported`.
    /// Should not happen for TriMesh inputs, but possible if a future
    /// caller hands us a different `Shape` impl.
    Unsupported,
}

impl std::fmt::Display for NarrowPhaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("parry3d does not support this shape pair"),
        }
    }
}

impl std::error::Error for NarrowPhaseError {}

/// True iff the two world-coord meshes actually intersect. Surface
/// touching (zero-distance contact) counts as intersecting — same
/// stance as the broad phase's "touching boxes pair at zero
/// tolerance".
pub fn intersects(a: &TriMesh, b: &TriMesh) -> Result<bool, NarrowPhaseError> {
    let pos = Isometry::identity();
    query::intersection_test(&pos, a, &pos, b).map_err(|_| NarrowPhaseError::Unsupported)
}

/// Minimum Euclidean distance between the two meshes, in metres
/// (assuming the meshes are in metres — same convention as the rest of
/// the substrate). Returns `0.0` when the meshes intersect.
///
/// Useful for soft-clash classification: a returned distance of `0`
/// means hard clash, a small positive distance means "within
/// clearance", a larger one means "near but not concerning".
pub fn min_distance(a: &TriMesh, b: &TriMesh) -> Result<f32, NarrowPhaseError> {
    let pos = Isometry::identity();
    query::distance(&pos, a, &pos, b).map_err(|_| NarrowPhaseError::Unsupported)
}

/// Band probe: `Some(d)` with some mesh-mesh distance `d <= cap` when
/// the meshes lie within `cap` of each other, else `None`. The
/// best-first traversal is seeded with `cap` as its initial pruning
/// bound instead of infinity, so subtrees whose AABB lower bound
/// already exceeds the band are never descended — in clearance sweeps
/// most candidate pairs lie beyond the band, and each of those rejects
/// near the BVH root instead of paying an exhaustive exact-distance
/// traversal (GH #143).
///
/// REJECT-ONLY: do not report the returned value. Seeding changes the
/// traversal schedule, and parry's composite distance is only
/// schedule-deterministic — near-tied leaf pairs resolve differently
/// (measured ~1e-6 m flips on G55; `query::distance(a,b)` vs `(b,a)`
/// differ the same way, and node lower bounds computed in world-scale
/// f32 can round above a leaf's true value). Callers that emit a
/// distance must confirm with [`min_distance`] and re-test the band —
/// and pad `cap` by more than the bound-arithmetic rounding slack so
/// a `None` here can never contradict the exact query's band verdict.
pub fn min_distance_within(a: &TriMesh, b: &TriMesh, cap: f32) -> Option<f32> {
    use parry3d::query::details::CompositeShapeAgainstAnyDistanceVisitor;
    use parry3d::query::DefaultQueryDispatcher;

    let pos = Isometry::identity();
    let mut visitor =
        CompositeShapeAgainstAnyDistanceVisitor::new(&DefaultQueryDispatcher, &pos, a, b);
    // Seed with `cap.next_up()`, not `cap`: the traversal records a
    // leaf only when its distance is STRICTLY below the running bound,
    // and callers band-test with `d <= cap` — a pair at exactly `cap`
    // must not be lost.
    a.qbvh()
        .traverse_best_first_node(&mut visitor, 0, cap.next_up())
        .map(|(_, (_, d))| d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::mesh::build_trimesh;

    /// Shadow of `geom::mesh::tests::unit_cube_at` so this module can
    /// build its own fixtures without making the helper pub.
    fn unit_cube_at(origin: [f32; 3]) -> (Vec<f32>, Vec<u32>) {
        let [ox, oy, oz] = origin;
        let v: Vec<f32> = vec![
            ox, oy, oz,
            ox + 1.0, oy, oz,
            ox + 1.0, oy + 1.0, oz,
            ox, oy + 1.0, oz,
            ox, oy, oz + 1.0,
            ox + 1.0, oy, oz + 1.0,
            ox + 1.0, oy + 1.0, oz + 1.0,
            ox, oy + 1.0, oz + 1.0,
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

    fn cube(origin: [f32; 3]) -> TriMesh {
        let (v, i) = unit_cube_at(origin);
        build_trimesh(&v, &i).expect("cube")
    }

    #[test]
    fn coincident_cubes_intersect() {
        let a = cube([0.0, 0.0, 0.0]);
        let b = cube([0.0, 0.0, 0.0]);
        assert!(intersects(&a, &b).unwrap());
        assert_eq!(min_distance(&a, &b).unwrap(), 0.0);
    }

    #[test]
    fn overlapping_cubes_intersect() {
        // [0,0,0]..[1,1,1] vs [0.5,0.5,0.5]..[1.5,1.5,1.5] — clearly overlap.
        let a = cube([0.0, 0.0, 0.0]);
        let b = cube([0.5, 0.5, 0.5]);
        assert!(intersects(&a, &b).unwrap());
        assert_eq!(min_distance(&a, &b).unwrap(), 0.0);
    }

    #[test]
    fn touching_cubes_intersect_at_zero_distance() {
        // Share the x=1 face.
        let a = cube([0.0, 0.0, 0.0]);
        let b = cube([1.0, 0.0, 0.0]);
        assert!(intersects(&a, &b).unwrap());
        // Exact touch — parry reports 0 distance.
        let d = min_distance(&a, &b).unwrap();
        assert!(d.abs() < 1e-4, "expected ~0 distance for face-touching cubes, got {d}");
    }

    #[test]
    fn separated_cubes_do_not_intersect() {
        let a = cube([0.0, 0.0, 0.0]);
        let b = cube([2.0, 0.0, 0.0]); // 1 m clearance along X
        assert!(!intersects(&a, &b).unwrap());
        let d = min_distance(&a, &b).unwrap();
        assert!((d - 1.0).abs() < 1e-4, "expected ~1.0 m clearance, got {d}");
    }

    #[test]
    fn capped_distance_within_band_matches_exact() {
        let a = cube([0.0, 0.0, 0.0]);
        let b = cube([2.0, 0.0, 0.0]); // ~1 m clearance
        let exact = min_distance(&a, &b).unwrap();
        // Comfortably inside the band. On well-conditioned fixtures
        // like these cubes the probe and the exact query agree
        // exactly; on real meshes the probe's VALUE may differ at the
        // last ulps (see the fn docs) — only Some/None is contractual.
        assert_eq!(min_distance_within(&a, &b, 1.5), Some(exact));
    }

    #[test]
    fn capped_distance_beyond_band_rejects() {
        let a = cube([0.0, 0.0, 0.0]);
        let b = cube([2.0, 0.0, 0.0]); // ~1 m clearance
        assert_eq!(min_distance_within(&a, &b, 0.5), None);
    }

    #[test]
    fn capped_distance_at_exact_band_edge_is_kept() {
        // Callers band-test with `d <= cap`, so a pair at exactly the
        // cap must survive — this pins the `cap.next_up()` seeding.
        let a = cube([0.0, 0.0, 0.0]);
        let b = cube([2.0, 0.0, 0.0]);
        let exact = min_distance(&a, &b).unwrap();
        assert_eq!(min_distance_within(&a, &b, exact), Some(exact));
    }

    #[test]
    fn capped_distance_intersecting_is_zero() {
        let a = cube([0.0, 0.0, 0.0]);
        let b = cube([0.5, 0.5, 0.5]);
        assert_eq!(min_distance_within(&a, &b, 0.1), Some(0.0));
    }

    #[test]
    fn distance_is_axis_aligned_minimum() {
        // Diagonal separation: 1 m along X, 1 m along Y — Euclidean
        // distance is sqrt(2) m, but the boxes themselves touch via
        // diagonal corners only. Use cubes [0,0,0]..[1,1,1] and
        // [2,2,0]..[3,3,1] → corners at (1,1,?) and (2,2,?), distance
        // sqrt(2).
        let a = cube([0.0, 0.0, 0.0]);
        let b = cube([2.0, 2.0, 0.0]);
        let d = min_distance(&a, &b).unwrap();
        let expected = (2.0_f32).sqrt();
        assert!(
            (d - expected).abs() < 1e-3,
            "expected ~{expected} m diagonal distance, got {d}"
        );
    }
}
