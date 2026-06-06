//! Thin facade over i_overlay's 2D Boolean, for the pure-Rust prism /
//! polygonal-bounded-halfspace CSG paths (GH #58 W6 + W9).
//!
//! **Why a facade** (audit risk-mitigation #2). i_overlay is pre-1.0 in
//! spirit — its API churns across majors. v7's float entry point is the
//! `SingleFloatOverlay` extension trait, *not* the integer
//! `Overlay::with_subj_and_clip` constructor an earlier design sketch
//! assumed. Quarantining the dependency behind this one module makes a
//! version bump, a feature change, or a swap to another 2D-Boolean
//! engine a single-file edit. Nothing outside this module names an
//! i_overlay type. See the `i_overlay-api` reference note.
//!
//! **Conventions enforced here** so callers don't have to learn
//! i_overlay's:
//! * Coordinates are `f64`, expected mesh_anchor-local (near origin,
//!   ±~10 m). i_overlay converts float → an adaptive integer grid
//!   internally (its robustness core); feeding f64 gives that adapter
//!   maximal precision, and `overlay_as::<i64>` uses the finer i64 grid
//!   appropriate at building scale with millimetre features. Callers do
//!   NOT pre-scale — that would duplicate / fight the internal adapter.
//! * Fill rule is `NonZero` with outer-CCW / hole-CW winding. NonZero
//!   unions overlapping same-orientation contours correctly (multiple
//!   openings whose footprints overlap), which `EvenOdd` would XOR. It
//!   *requires* correct winding, so [`difference`] normalises every
//!   ring before the call rather than trusting the caller.

use glam::Vec2;

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;

use crate::mesh::profile::Polygon2D;

/// A 2D point in the facade's f64 working space.
pub type Point = [f64; 2];
/// One closed ring (no repeated closing vertex), matching i_overlay's
/// `Contour`.
pub type Contour = Vec<Point>;
/// A planar region: `[0]` = outer ring, `[1..]` = holes. Matches
/// i_overlay's `Shape`. Winding is normalised by [`difference`].
pub type Shape = Vec<Contour>;

/// Subtract the union of `cutters` from `subject`, returning the result
/// as zero or more shapes (each an outer ring plus its holes).
///
/// * Empty result vec ⇒ the subject was fully consumed.
/// * One shape ⇒ the common door / window case (a wall profile with a
///   rectangular hole, or a clipped outline).
/// * Multiple shapes ⇒ the cutter split the subject into disconnected
///   pieces (rare but valid).
///
/// All cutter contours are handed to i_overlay as a single clip region;
/// under `NonZero` that region is their union, so no separate union
/// pass is needed and overlapping cutters fold correctly. Winding is
/// normalised here (outer CCW, holes CW) so `NonZero` is robust
/// regardless of how the caller wound its rings.
pub fn difference(subject: &Shape, cutters: &[Shape]) -> Vec<Shape> {
    let subj = normalized(subject);

    let mut clip: Shape = Vec::new();
    for c in cutters {
        for (i, ring) in c.iter().enumerate() {
            if ring.len() >= 3 {
                clip.push(oriented(ring, i == 0));
            }
        }
    }
    if clip.is_empty() {
        // Nothing to subtract — echo the (normalised) subject through.
        return vec![subj];
    }

    subj.overlay_as::<i64>(&clip, OverlayRule::Difference, FillRule::NonZero)
}

/// Signed area of a ring via the shoelace formula. Positive ⇒ CCW,
/// negative ⇒ CW (in a right-handed XY frame). Degenerate rings
/// (< 3 points) return 0.
pub fn signed_area(ring: &[Point]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..n {
        let [x0, y0] = ring[i];
        let [x1, y1] = ring[(i + 1) % n];
        a += x0 * y1 - x1 * y0;
    }
    a * 0.5
}

/// Return `ring` wound CCW if `want_ccw`, else CW. A degenerate ring is
/// returned unchanged.
fn oriented(ring: &[Point], want_ccw: bool) -> Contour {
    let area = signed_area(ring);
    if area == 0.0 {
        return ring.to_vec();
    }
    let is_ccw = area > 0.0;
    if is_ccw == want_ccw {
        ring.to_vec()
    } else {
        ring.iter().rev().copied().collect()
    }
}

/// Normalise a shape's winding: outer ring CCW, holes CW.
fn normalized(shape: &Shape) -> Shape {
    shape
        .iter()
        .enumerate()
        .map(|(i, ring)| oriented(ring, i == 0))
        .collect()
}

/// Build a facade [`Shape`] from a [`Polygon2D`] (f32 → f64), preserving
/// the outer-then-holes order. Input winding is irrelevant — [`difference`]
/// normalises it.
pub fn shape_from_polygon2d(p: &Polygon2D) -> Shape {
    let mut s: Shape = Vec::with_capacity(1 + p.holes.len());
    s.push(p.outer.iter().map(|v| [v.x as f64, v.y as f64]).collect());
    for h in &p.holes {
        s.push(h.iter().map(|v| [v.x as f64, v.y as f64]).collect());
    }
    s
}

/// Convert one facade [`Shape`] back to a [`Polygon2D`] (f64 → f32),
/// taking ring `[0]` as the outer and the rest as holes.
pub fn polygon2d_from_shape(s: &Shape) -> Polygon2D {
    let outer = s
        .first()
        .map(|c| c.iter().map(|p| Vec2::new(p[0] as f32, p[1] as f32)).collect())
        .unwrap_or_default();
    let holes = s
        .iter()
        .skip(1)
        .map(|c| c.iter().map(|p| Vec2::new(p[0] as f32, p[1] as f32)).collect())
        .collect();
    Polygon2D { outer, holes }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis-aligned rectangle, wound CCW.
    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Contour {
        vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    }

    /// Net area of a result shape: outer minus holes (magnitudes).
    fn net_area(shape: &Shape) -> f64 {
        let outer = shape.first().map(|c| signed_area(c).abs()).unwrap_or(0.0);
        let holes: f64 = shape.iter().skip(1).map(|c| signed_area(c).abs()).sum();
        outer - holes
    }

    #[test]
    fn centered_cutter_produces_one_hole() {
        let subj: Shape = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let cut: Shape = vec![rect(3.0, 3.0, 7.0, 7.0)];
        let out = difference(&subj, &[cut]);
        assert_eq!(out.len(), 1, "one connected shape");
        assert_eq!(out[0].len(), 2, "outer + one hole");
        assert!((net_area(&out[0]) - 84.0).abs() < 1e-6, "area = {}", net_area(&out[0]));
    }

    #[test]
    fn subject_with_hole_round_trips() {
        // Residual check from the i_overlay research: shape-with-holes
        // input is accepted by SingleFloatOverlay. Subject is a 10×10
        // square with a 2×2 hole; a disjoint cutter leaves it unchanged
        // (one shape, the original hole preserved).
        let subj: Shape = vec![rect(0.0, 0.0, 10.0, 10.0), rect(4.0, 4.0, 6.0, 6.0)];
        let cut: Shape = vec![rect(20.0, 20.0, 21.0, 21.0)];
        let out = difference(&subj, &[cut]);
        assert_eq!(out.len(), 1, "disjoint cutter → subject unchanged");
        assert_eq!(out[0].len(), 2, "the subject's own hole must survive");
        assert!((net_area(&out[0]) - 96.0).abs() < 1e-6, "area = {}", net_area(&out[0]));
    }

    #[test]
    fn overlapping_cutters_union_via_nonzero() {
        // Two overlapping cutters: NonZero unions them, so the carved
        // region is their union (28), not double-counted or XOR'd.
        let subj: Shape = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let c1: Shape = vec![rect(2.0, 2.0, 6.0, 6.0)];
        let c2: Shape = vec![rect(4.0, 4.0, 8.0, 8.0)];
        let out = difference(&subj, &[c1, c2]);
        assert_eq!(out.len(), 1);
        // union(c1,c2) = 16 + 16 − 4 = 28 ; remaining = 100 − 28 = 72.
        assert!((net_area(&out[0]) - 72.0).abs() < 1e-6, "area = {}", net_area(&out[0]));
    }

    #[test]
    fn cutter_consuming_subject_returns_empty() {
        let subj: Shape = vec![rect(2.0, 2.0, 8.0, 8.0)];
        let cut: Shape = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let out = difference(&subj, &[cut]);
        assert!(out.is_empty(), "fully consumed → no shapes, got {out:?}");
    }

    #[test]
    fn reversed_winding_is_normalised() {
        // Subject wound CW (negative area) must still behave as a solid
        // region under NonZero after normalisation.
        let mut cw = rect(0.0, 0.0, 10.0, 10.0);
        cw.reverse();
        let subj: Shape = vec![cw];
        let cut: Shape = vec![rect(3.0, 3.0, 7.0, 7.0)];
        let out = difference(&subj, &[cut]);
        assert_eq!(out.len(), 1);
        assert!((net_area(&out[0]) - 84.0).abs() < 1e-6, "area = {}", net_area(&out[0]));
    }

    #[test]
    fn polygon2d_conversion_round_trips() {
        let p = Polygon2D {
            outer: vec![Vec2::new(0.0, 0.0), Vec2::new(4.0, 0.0), Vec2::new(4.0, 4.0)],
            holes: vec![vec![Vec2::new(1.0, 1.0), Vec2::new(2.0, 1.0), Vec2::new(2.0, 2.0)]],
        };
        let s = shape_from_polygon2d(&p);
        assert_eq!(s.len(), 2);
        let back = polygon2d_from_shape(&s);
        assert_eq!(back.outer.len(), 3);
        assert_eq!(back.holes.len(), 1);
        assert!((back.outer[1].x - 4.0).abs() < 1e-6);
    }
}
