//! Broad-phase pairwise AABB overlap.
//!
//! Given a list of axis-aligned boxes (one per product, in world
//! coordinates), return every pair whose boxes overlap — optionally
//! expanded by a tolerance band on each side, so "clearance clashes"
//! (objects within N centimetres of each other, even if not touching)
//! fall out of the same primitive.
//!
//! Implementation note: this is the trivial O(N²) intersection sweep.
//! For Duplex (~300 instances) that's ~45 000 pair comparisons,
//! sub-millisecond. We'll add an interval-tree or hashed-grid
//! accelerator only when a real model proves it necessary (the BVH
//! inside each `TriMesh` is doing the heavy lifting in the narrow
//! phase, so this layer is rarely the bottleneck).

use parry3d::bounding_volume::{Aabb, BoundingVolume};
use parry3d::math::Point;

/// World-AABB of a product, paired with a stable index back into the
/// caller's element list. The caller decides what the index *means*
/// (instance row, mesh entry, GUID lookup key) — the broad phase only
/// emits pairs of indices.
#[derive(Debug, Clone, Copy)]
pub struct AabbF32 {
    pub id: u32,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl AabbF32 {
    fn to_parry(self) -> Aabb {
        Aabb::new(
            Point::new(self.min[0], self.min[1], self.min[2]),
            Point::new(self.max[0], self.max[1], self.max[2]),
        )
    }
}

/// Return every (i, j) pair (with i < j) whose AABBs overlap when
/// expanded by `tolerance` on each side. Pass `tolerance = 0.0` for a
/// strict touching-or-intersecting test; pass a positive value for
/// soft-clash / clearance candidate detection.
///
/// Pairs are returned in `(boxes[i].id, boxes[j].id)` form — the
/// caller's indices, not the position in the input slice. That keeps
/// the broad phase composable with the caller's element ordering (e.g.
/// the row order in `instances.parquet`).
///
/// Caller is responsible for any same-object dedup (passing an empty
/// list, or one containing duplicate ids, is allowed — duplicates
/// just produce self-pairs which can be filtered at the caller).
pub fn pairs_overlapping(boxes: &[AabbF32], tolerance: f32) -> Vec<(u32, u32)> {
    let n = boxes.len();
    if n < 2 {
        return Vec::new();
    }
    // Pre-expand each AABB once so the tolerance test is a normal
    // AABB-vs-AABB intersection inside the loop — cheaper than
    // recomputing on every comparison. `parry3d::bounding_volume::Aabb`
    // in 0.17 doesn't expose `loosened`, so do it by hand.
    let expanded: Vec<Aabb> = boxes
        .iter()
        .map(|b| {
            let parry = b.to_parry();
            Aabb::new(
                Point::new(
                    parry.mins.x - tolerance,
                    parry.mins.y - tolerance,
                    parry.mins.z - tolerance,
                ),
                Point::new(
                    parry.maxs.x + tolerance,
                    parry.maxs.y + tolerance,
                    parry.maxs.z + tolerance,
                ),
            )
        })
        .collect();

    let mut pairs: Vec<(u32, u32)> = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            if expanded[i].intersects(&expanded[j]) {
                pairs.push((boxes[i].id, boxes[j].id));
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bb(id: u32, min: [f32; 3], max: [f32; 3]) -> AabbF32 {
        AabbF32 { id, min, max }
    }

    #[test]
    fn empty_input_returns_no_pairs() {
        assert!(pairs_overlapping(&[], 0.0).is_empty());
    }

    #[test]
    fn single_box_returns_no_pairs() {
        let boxes = vec![bb(1, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0])];
        assert!(pairs_overlapping(&boxes, 0.0).is_empty());
    }

    #[test]
    fn disjoint_boxes_dont_pair() {
        let boxes = vec![
            bb(1, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
            bb(2, [10.0, 0.0, 0.0], [11.0, 1.0, 1.0]),
        ];
        assert!(pairs_overlapping(&boxes, 0.0).is_empty());
    }

    #[test]
    fn intersecting_boxes_pair() {
        // Cube 1: [0,0,0]..[2,2,2]. Cube 2: [1,1,1]..[3,3,3]. Overlap.
        let boxes = vec![
            bb(1, [0.0, 0.0, 0.0], [2.0, 2.0, 2.0]),
            bb(2, [1.0, 1.0, 1.0], [3.0, 3.0, 3.0]),
        ];
        let pairs = pairs_overlapping(&boxes, 0.0);
        assert_eq!(pairs, vec![(1, 2)]);
    }

    #[test]
    fn touching_boxes_pair_at_zero_tolerance() {
        // Cube 1: [0,0,0]..[1,1,1]. Cube 2: [1,0,0]..[2,1,1]. They share
        // the x=1 face — parry treats this as intersecting.
        let boxes = vec![
            bb(1, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
            bb(2, [1.0, 0.0, 0.0], [2.0, 1.0, 1.0]),
        ];
        let pairs = pairs_overlapping(&boxes, 0.0);
        assert_eq!(pairs, vec![(1, 2)]);
    }

    #[test]
    fn near_miss_pairs_only_with_tolerance() {
        // 10 cm gap between two cubes (1 m apart along X — gap from 1.0
        // to 1.1 m).
        let boxes = vec![
            bb(1, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
            bb(2, [1.1, 0.0, 0.0], [2.1, 1.0, 1.0]),
        ];
        // Zero tolerance — no pair.
        assert!(pairs_overlapping(&boxes, 0.0).is_empty());
        // 4 cm tolerance — still no pair (each side expands by 4 cm so
        // 8 cm of the 10 cm gap is closed, leaving a 2 cm gap).
        assert!(pairs_overlapping(&boxes, 0.04).is_empty());
        // 6 cm tolerance — pairs (each side expands by 6 cm so 12 cm
        // closes the 10 cm gap and leaves a 2 cm overlap). Picking 6 cm
        // rather than the exact-touch 5 cm avoids the binary-float
        // imprecision around `1.1 - 0.05`.
        assert_eq!(pairs_overlapping(&boxes, 0.06), vec![(1, 2)]);
        // 20 cm tolerance — clearly pairs.
        assert_eq!(pairs_overlapping(&boxes, 0.2), vec![(1, 2)]);
    }

    #[test]
    fn n_squared_includes_every_overlapping_pair() {
        // Three cubes all overlapping each other → 3 pairs.
        let boxes = vec![
            bb(1, [0.0, 0.0, 0.0], [2.0, 2.0, 2.0]),
            bb(2, [1.0, 1.0, 1.0], [3.0, 3.0, 3.0]),
            bb(3, [0.5, 0.5, 0.5], [2.5, 2.5, 2.5]),
        ];
        let pairs = pairs_overlapping(&boxes, 0.0);
        assert_eq!(pairs.len(), 3);
        assert!(pairs.contains(&(1, 2)));
        assert!(pairs.contains(&(1, 3)));
        assert!(pairs.contains(&(2, 3)));
    }
}
