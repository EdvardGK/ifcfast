//! Profile-polygon validity at extraction (GH #220).
//!
//! `volume_reliable` is a tripwire that must predict divergence from an
//! authoritative kernel. A profile polygon that self-intersects (or has
//! collapsed, or carries a hole outside its outer loop) reaches earcut and
//! yields a cap triangulation that over- or under-covers the region the
//! author meant — and the resulting extrusion still fits comfortably inside
//! the element-level bounds the QTO router checks (#60 prism, AABB). On
//! KNM_ARK before #218 two walls with ~1 mm crescent-sliver items whose
//! *sampled* outline self-intersected over-counted +14 % / +21 % with
//! `volume_reliable = True`.
//!
//! [`check`] inspects the final `Polygon2D` (after sampling, positioning
//! and winding normalisation — exactly what the extruder triangulates) and
//! names the first defect it finds. The mesh is never changed (reveal-all);
//! the defect rides on [`crate::mesh::extrusion::LocalMesh::profile_invalid`]
//! up to the product and its QTO row is routed out of trusted sums.
//!
//! Cost: one sort-and-sweep over all loop segments (O(n log n + k)); a
//! rectangle is four segments. Measured on G55_ARK / KNM_ARK in the GH #220
//! report.

use glam::Vec2;

use crate::mesh::profile::Polygon2D;

/// Why a profile polygon is not a valid region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileDefect {
    /// Two non-adjacent edges (of the outer loop, a hole, or between
    /// loops) properly cross each other.
    SelfIntersecting,
    /// A loop has fewer than three distinct vertices, or its mean width
    /// (`2·area / perimeter`) is below the distance tolerance — the region
    /// has collapsed to a line.
    Degenerate,
    /// A hole lies outside the outer loop (it subtracts nothing a kernel
    /// would subtract; earcut's result is undefined).
    HoleOutside,
}

impl ProfileDefect {
    /// Stable token surfaced on QTO rows (`profile_invalid` column).
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileDefect::SelfIntersecting => "self_intersecting",
            ProfileDefect::Degenerate => "degenerate",
            ProfileDefect::HoleOutside => "hole_outside",
        }
    }
}

/// Relative distance tolerance (fraction of the outer loop's bbox
/// diagonal) for "this vertex is really on the other side of that edge".
const REL_TOL: f64 = 1e-6;
/// f32 coordinates carry ~1.2e-7 relative error; a crossing shallower than
/// a few ulps of the largest coordinate is noise, not geometry.
const ULP_TOL: f64 = 5e-7;

struct Seg {
    a: (f64, f64),
    b: (f64, f64),
    loop_id: usize,
    idx: usize,
    loop_len: usize,
    xmin: f64,
    xmax: f64,
    ymin: f64,
    ymax: f64,
}

/// Distinct consecutive vertices of a loop (drops repeats and an explicit
/// closing duplicate of the first vertex), in f64.
fn clean_loop(pts: &[Vec2]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(pts.len());
    for p in pts {
        let q = (p.x as f64, p.y as f64);
        if out.last() != Some(&q) {
            out.push(q);
        }
    }
    while out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    out
}

fn area2(l: &[(f64, f64)]) -> f64 {
    let n = l.len();
    let mut acc = 0.0;
    for i in 0..n {
        let (ax, ay) = l[i];
        let (bx, by) = l[(i + 1) % n];
        acc += ax * by - bx * ay;
    }
    acc
}

fn perimeter(l: &[(f64, f64)]) -> f64 {
    let n = l.len();
    (0..n)
        .map(|i| {
            let (ax, ay) = l[i];
            let (bx, by) = l[(i + 1) % n];
            (bx - ax).hypot(by - ay)
        })
        .sum()
}

#[inline]
fn orient(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

/// Signed distance-scaled side test: `c` is strictly on one side of line
/// `ab` only when its distance exceeds `tol`.
#[inline]
fn side(a: (f64, f64), b: (f64, f64), c: (f64, f64), tol: f64) -> i8 {
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    let o = orient(a, b, c);
    if o > tol * len {
        1
    } else if o < -tol * len {
        -1
    } else {
        0
    }
}

/// Proper crossing: each segment's endpoints lie strictly on opposite
/// sides of the other's line. Touching, T-junctions and collinear overlap
/// are not crossings.
fn crosses(s: &Seg, t: &Seg, tol: f64) -> bool {
    let d1 = side(s.a, s.b, t.a, tol);
    let d2 = side(s.a, s.b, t.b, tol);
    if d1 == 0 || d2 == 0 || d1 == d2 {
        return false;
    }
    let d3 = side(t.a, t.b, s.a, tol);
    let d4 = side(t.a, t.b, s.b, tol);
    d3 != 0 && d4 != 0 && d3 != d4
}

fn adjacent(s: &Seg, t: &Seg) -> bool {
    if s.loop_id != t.loop_id {
        return false;
    }
    let (i, j, n) = (s.idx, t.idx, s.loop_len);
    i.abs_diff(j) == 1 || (i == 0 && j == n - 1) || (j == 0 && i == n - 1)
}

/// Point-in-polygon with a boundary band: `Some(true)` strictly inside,
/// `Some(false)` strictly outside, `None` within `tol` of an edge.
fn point_in_loop(p: (f64, f64), l: &[(f64, f64)], tol: f64) -> Option<bool> {
    let n = l.len();
    let mut inside = false;
    for i in 0..n {
        let a = l[i];
        let b = l[(i + 1) % n];
        // Distance from p to segment ab.
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (qx, qy) = (a.0 + t * dx, a.1 + t * dy);
        if (p.0 - qx).hypot(p.1 - qy) <= tol {
            return None;
        }
        if (a.1 > p.1) != (b.1 > p.1) {
            let x = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
            if p.0 < x {
                inside = !inside;
            }
        }
    }
    Some(inside)
}

/// Validate a profile polygon. `None` = valid (or not decidable as
/// invalid); `Some(defect)` names the first defect found, checked in the
/// order: < 3 distinct vertices (degenerate) → crossing edges
/// (self-intersecting) → collapsed loop (degenerate) → hole outside.
pub fn check(p: &Polygon2D) -> Option<ProfileDefect> {
    let outer = clean_loop(&p.outer);
    if outer.len() < 3 {
        return Some(ProfileDefect::Degenerate);
    }
    let (mut xmin, mut ymin, mut xmax, mut ymax) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in &outer {
        xmin = xmin.min(x);
        xmax = xmax.max(x);
        ymin = ymin.min(y);
        ymax = ymax.max(y);
    }
    if !(xmin.is_finite() && ymin.is_finite() && xmax.is_finite() && ymax.is_finite()) {
        return None;
    }
    let diag = (xmax - xmin).hypot(ymax - ymin);
    let max_abs = xmin.abs().max(xmax.abs()).max(ymin.abs()).max(ymax.abs());
    let tol = (REL_TOL * diag).max(ULP_TOL * max_abs);
    if diag <= tol {
        return Some(ProfileDefect::Degenerate);
    }

    let holes: Vec<Vec<(f64, f64)>> = p.holes.iter().map(|h| clean_loop(h)).collect();
    let mut loops: Vec<&[(f64, f64)]> = Vec::with_capacity(1 + holes.len());
    loops.push(&outer);
    for h in &holes {
        loops.push(h);
    }

    if loops.iter().any(|l| l.len() < 3) {
        return Some(ProfileDefect::Degenerate);
    }

    // Self-intersection: sort-and-sweep on x over every loop's segments.
    let total: usize = loops.iter().map(|l| l.len()).sum();
    if total > 3 {
        let mut segs: Vec<Seg> = Vec::with_capacity(total);
        for (li, l) in loops.iter().enumerate() {
            let n = l.len();
            for i in 0..n {
                let a = l[i];
                let b = l[(i + 1) % n];
                segs.push(Seg {
                    a,
                    b,
                    loop_id: li,
                    idx: i,
                    loop_len: n,
                    xmin: a.0.min(b.0),
                    xmax: a.0.max(b.0),
                    ymin: a.1.min(b.1),
                    ymax: a.1.max(b.1),
                });
            }
        }
        segs.sort_by(|s, t| s.xmin.total_cmp(&t.xmin));
        for i in 0..segs.len() {
            let s = &segs[i];
            for t in &segs[i + 1..] {
                if t.xmin > s.xmax {
                    break;
                }
                if t.ymin > s.ymax || t.ymax < s.ymin || adjacent(s, t) {
                    continue;
                }
                if crosses(s, t, tol) {
                    return Some(ProfileDefect::SelfIntersecting);
                }
            }
        }
    }

    // Degenerate: mean width (2·area / perimeter) below tolerance — the
    // loop has collapsed onto a line. Checked after crossings: a bow-tie's
    // lobes cancel to zero signed area but it is a self-intersection.
    for l in &loops {
        if area2(l).abs() / perimeter(l) <= tol {
            return Some(ProfileDefect::Degenerate);
        }
    }

    // Hole outside: no crossings remain, so each hole is wholly inside or
    // wholly outside the outer loop (modulo touching) — the first vertex
    // clear of the boundary decides.
    for h in &holes {
        for &v in h.iter() {
            match point_in_loop(v, &outer, tol) {
                Some(true) => break,
                Some(false) => return Some(ProfileDefect::HoleOutside),
                None => continue,
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poly(outer: &[(f32, f32)], holes: &[&[(f32, f32)]]) -> Polygon2D {
        Polygon2D {
            outer: outer.iter().map(|&(x, y)| Vec2::new(x, y)).collect(),
            holes: holes
                .iter()
                .map(|h| h.iter().map(|&(x, y)| Vec2::new(x, y)).collect())
                .collect(),
        }
    }

    #[test]
    fn rectangle_is_valid() {
        let p = poly(&[(0., 0.), (10., 0.), (10., 5.), (0., 5.)], &[]);
        assert_eq!(check(&p), None);
    }

    #[test]
    fn bow_tie_self_intersects() {
        let p = poly(&[(0., 0.), (10., 10.), (10., 0.), (0., 10.)], &[]);
        assert_eq!(check(&p), Some(ProfileDefect::SelfIntersecting));
    }

    #[test]
    fn valid_concave_is_valid() {
        // L / U shapes and a deep notch: concave but simple.
        let l = poly(
            &[(0., 0.), (10., 0.), (10., 2.), (2., 2.), (2., 10.), (0., 10.)],
            &[],
        );
        assert_eq!(check(&l), None);
        let notch = poly(
            &[(0., 0.), (10., 0.), (10., 10.), (5., 0.5), (0., 10.)],
            &[],
        );
        assert_eq!(check(&notch), None);
    }

    #[test]
    fn closing_duplicate_and_repeats_are_ignored() {
        let p = poly(
            &[(0., 0.), (10., 0.), (10., 0.), (10., 5.), (0., 5.), (0., 0.)],
            &[],
        );
        assert_eq!(check(&p), None);
    }

    #[test]
    fn touching_vertex_is_not_a_crossing() {
        // Keyhole-style: a vertex touches another edge but nothing crosses.
        let p = poly(
            &[(0., 0.), (10., 0.), (10., 10.), (5., 0.), (0., 10.)],
            &[],
        );
        assert_eq!(check(&p), None);
    }

    #[test]
    fn hole_inside_is_valid_and_outside_is_flagged() {
        let outer = [(0., 0.), (10., 0.), (10., 10.), (0., 10.)];
        let inside: &[(f32, f32)] = &[(2., 2.), (2., 4.), (4., 4.), (4., 2.)];
        assert_eq!(check(&poly(&outer, &[inside])), None);
        let outside: &[(f32, f32)] = &[(12., 2.), (12., 4.), (14., 4.), (14., 2.)];
        assert_eq!(
            check(&poly(&outer, &[outside])),
            Some(ProfileDefect::HoleOutside)
        );
        // A hole crossing the outer boundary is a self-intersection.
        let straddle: &[(f32, f32)] = &[(8., 2.), (8., 4.), (12., 4.), (12., 2.)];
        assert_eq!(
            check(&poly(&outer, &[straddle])),
            Some(ProfileDefect::SelfIntersecting)
        );
    }

    #[test]
    fn collinear_and_short_loops_are_degenerate() {
        let line = poly(&[(0., 0.), (5., 0.), (10., 0.)], &[]);
        assert_eq!(check(&line), Some(ProfileDefect::Degenerate));
        let two = poly(&[(0., 0.), (10., 0.)], &[]);
        assert_eq!(check(&two), Some(ProfileDefect::Degenerate));
        let one_point = poly(&[(1., 1.), (1., 1.), (1., 1.)], &[]);
        assert_eq!(check(&one_point), Some(ProfileDefect::Degenerate));
    }

    #[test]
    fn thin_but_real_sliver_is_valid() {
        // 1 mm × 28 m band (KNM scale, mm units): thin, not degenerate.
        let p = poly(
            &[(0., 0.), (28_000., 0.), (28_000., 1.), (0., 1.)],
            &[],
        );
        assert_eq!(check(&p), None);
    }

    #[test]
    fn thin_crossing_sliver_is_flagged() {
        // The #220 shape class: a ~1 mm band whose long edges cross.
        let p = poly(
            &[(0., 0.), (28_000., 1.), (28_000., 0.), (0., 1.)],
            &[],
        );
        assert_eq!(check(&p), Some(ProfileDefect::SelfIntersecting));
    }
}
