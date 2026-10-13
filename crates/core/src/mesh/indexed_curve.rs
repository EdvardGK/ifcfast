//! `IfcIndexedPolyCurve` segment evaluator.
//!
//! `IfcIndexedPolyCurve` stores a point list plus an optional list of
//! `IfcSegmentIndexSelect` segments. Each segment is one of:
//!
//! - `IfcLineIndex((i1, i2, …))` — connect the indexed points with
//!   straight lines.
//! - `IfcArcIndex((i_start, i_mid, i_end))` — the unique circular arc
//!   through three points, with `i_mid` lying on the arc between the
//!   endpoints.
//!
//! Indices are 1-based. Adjacent segments share an endpoint; the shared
//! vertex is emitted only once. When `Segments = $`, the polyline is the
//! point list in original order.
//!
//! Without arc evaluation, Revit MEP pipes (cross-sections authored as
//! 4-point lists + 2 `IfcArcIndex` semicircles) collapse to square
//! prisms — see GH #48.
//!
//! Sampling density is `profile::arc_samples_per_turn` — the 0.5 mm
//! sagitta rule of GH #170 without the 32-per-turn cap (GH #218) — scaled
//! by arc angle, 2..=`MAX_ARC_CHORDS` chords per arc: a metre-radius
//! semicircle gets 50 chords, a 16.5° arc at R = 98 m gets 45. The arc's
//! endpoints are emitted verbatim (they are the neighbouring segments'
//! points too).

use glam::{DVec2, Vec2, Vec3};

use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::profile::{arc_area_scale_pinned, arc_chord_count, arc_samples_per_turn};

/// Parse a typed inline value such as `IFCARCINDEX((1,2,3))` — returns
/// `(name, body)` where `body` is the bytes between the outer `(` and
/// `)` (so e.g. `b"(1,2,3)"` for an ArcIndex). Returns `None` if the
/// pattern doesn't match.
fn parse_typed_inline(raw: &[u8]) -> Option<(&[u8], &[u8])> {
    let raw = trim_ws(raw);
    let open = raw.iter().position(|&b| b == b'(')?;
    if raw.last() != Some(&b')') {
        return None;
    }
    let name = &raw[..open];
    if name.is_empty() {
        return None;
    }
    let body = &raw[open + 1..raw.len() - 1];
    Some((name, body))
}

fn trim_ws(mut s: &[u8]) -> &[u8] {
    while let Some((&b, rest)) = s.split_first() {
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
            s = rest;
        } else {
            break;
        }
    }
    while let Some((&b, rest)) = s.split_last() {
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// Evaluate `IfcIndexedPolyCurve.Segments` against a 2D point list.
///
/// `segments_raw` is the raw bytes of the Segments list field (the body
/// between the outer parens of the field, i.e. the inner of the LIST OF
/// IfcSegmentIndexSelect). Returns `None` if any segment fails to parse;
/// callers can fall back to "connect points in order" for that case.
pub fn eval_segments_2d(pts: &[Vec2], segments_raw: &[u8], unit_scale: f32) -> Option<Vec<Vec2>> {
    let seg_fields = split_top_level_args(segments_raw);
    if seg_fields.is_empty() {
        return None;
    }
    let mut out: Vec<Vec2> = Vec::new();
    for seg_field in seg_fields {
        let (name, body_with_parens) = parse_typed_inline(seg_field)?;
        let indices = parse_index_list(body_with_parens)?;
        if name.eq_ignore_ascii_case(b"IFCARCINDEX") {
            if indices.len() != 3 {
                return None;
            }
            let p1 = *pts.get(indices[0].checked_sub(1)?)?;
            let mid = *pts.get(indices[1].checked_sub(1)?)?;
            let p3 = *pts.get(indices[2].checked_sub(1)?)?;
            let samples = arc_samples_2d(p1, mid, p3, unit_scale).unwrap_or_else(|| vec![p1, p3]);
            append_dedup_2d(&mut out, &samples);
        } else if name.eq_ignore_ascii_case(b"IFCLINEINDEX") {
            if indices.len() < 2 {
                return None;
            }
            let line_pts: Option<Vec<Vec2>> = indices
                .iter()
                .map(|&i| pts.get(i.checked_sub(1)?).copied())
                .collect();
            append_dedup_2d(&mut out, &line_pts?);
        } else {
            return None;
        }
    }
    if out.len() > 2 && coincident_2d(*out.first().unwrap(), *out.last().unwrap()) {
        out.pop();
    }
    // Force CCW orientation — earcut + the extrusion pipeline assume
    // the outer ring is CCW. Revit MEP pipes author the arcs in CW
    // order (starting at (-R, 0), sweeping through (0, +R) → (+R, 0)
    // → (0, -R) → back), which without correction triangulates the
    // wrong region and shrinks the extruded volume by ~3×.
    if signed_area_2d(&out) < 0.0 {
        out.reverse();
    }
    Some(out)
}

fn signed_area_2d(pts: &[Vec2]) -> f32 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut a = 0.0_f32;
    for i in 0..n {
        let j = (i + 1) % n;
        a += pts[i].x * pts[j].y - pts[j].x * pts[i].y;
    }
    0.5 * a
}

/// 3D variant of [`eval_segments_2d`] — same semantics, evaluated in
/// the plane through the three control points of each arc.
pub fn eval_segments_3d(pts: &[Vec3], segments_raw: &[u8], unit_scale: f32) -> Option<Vec<Vec3>> {
    let seg_fields = split_top_level_args(segments_raw);
    if seg_fields.is_empty() {
        return None;
    }
    let mut out: Vec<Vec3> = Vec::new();
    for seg_field in seg_fields {
        let (name, body_with_parens) = parse_typed_inline(seg_field)?;
        let indices = parse_index_list(body_with_parens)?;
        if name.eq_ignore_ascii_case(b"IFCARCINDEX") {
            if indices.len() != 3 {
                return None;
            }
            let p1 = *pts.get(indices[0].checked_sub(1)?)?;
            let mid = *pts.get(indices[1].checked_sub(1)?)?;
            let p3 = *pts.get(indices[2].checked_sub(1)?)?;
            let samples = arc_samples_3d(p1, mid, p3, unit_scale).unwrap_or_else(|| vec![p1, p3]);
            append_dedup_3d(&mut out, &samples);
        } else if name.eq_ignore_ascii_case(b"IFCLINEINDEX") {
            if indices.len() < 2 {
                return None;
            }
            let line_pts: Option<Vec<Vec3>> = indices
                .iter()
                .map(|&i| pts.get(i.checked_sub(1)?).copied())
                .collect();
            append_dedup_3d(&mut out, &line_pts?);
        } else {
            return None;
        }
    }
    if out.len() > 2 && coincident_3d(*out.first().unwrap(), *out.last().unwrap()) {
        out.pop();
    }
    Some(out)
}

/// Relative coincidence tolerance for consecutive / closing samples
/// (GH #173). Arc samples are `center + r·(cos θ, sin θ)` in f32, so the
/// end of one arc and the start of the next — or the end of the last arc
/// and the loop's first sample — land on the same authored point only to
/// within a few ulps of the coordinate magnitude (measured on G55_RIV pipe
/// profiles: ~1e-5 mm at r = 55 mm, i.e. ~2e-7 relative). The old
/// absolute 1e-6-unit test missed that, so every two-arc hollow profile
/// kept a near-duplicate closing point: a zero-width side-wall quad at
/// the seam that the cap triangulation did not share, which left the
/// extrusion's edges unbalanced on raw indices. 1e-6 relative (~8 ulps)
/// with a 1e-6-unit absolute floor (the old tolerance) merges trig noise
/// and nothing a tessellation could mean.
const DEDUP_REL: f32 = 1e-6;

fn coincident_2d(a: Vec2, b: Vec2) -> bool {
    let scale = a.abs().max_element().max(b.abs().max_element()).max(1.0);
    (a - b).length_squared() <= (DEDUP_REL * scale) * (DEDUP_REL * scale)
}

fn coincident_3d(a: Vec3, b: Vec3) -> bool {
    let scale = a.abs().max_element().max(b.abs().max_element()).max(1.0);
    (a - b).length_squared() <= (DEDUP_REL * scale) * (DEDUP_REL * scale)
}

fn parse_index_list(body_with_parens: &[u8]) -> Option<Vec<usize>> {
    let inner = match parse_field(body_with_parens) {
        Field::List(b) => b,
        _ => return None,
    };
    let out: Vec<usize> = split_top_level_args(inner)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Number(n) if n >= 0.0 => Some(n as usize),
            _ => None,
        })
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn append_dedup_2d(out: &mut Vec<Vec2>, samples: &[Vec2]) {
    let start = match (out.last(), samples.first()) {
        (Some(&last), Some(&first)) if coincident_2d(last, first) => 1,
        _ => 0,
    };
    out.extend_from_slice(&samples[start..]);
}

fn append_dedup_3d(out: &mut Vec<Vec3>, samples: &[Vec3]) {
    let start = match (out.last(), samples.first()) {
        (Some(&last), Some(&first)) if coincident_3d(last, first) => 1,
        _ => 0,
    };
    out.extend_from_slice(&samples[start..]);
}

/// Unique circle through three 2D points, in f64 relative to `p1` (GH
/// #218): Revit profile coordinates reach 3.6e5 mm, where the f32
/// `x² + y²` terms of the textbook formula carry an ulp of ~8 000 mm² and
/// put a 9 m arc's centre millimetres off (5.3 mm bbox error on a KNM
/// opening). `None` for collinear inputs.
fn circumcircle_2d(p1: DVec2, p2: DVec2, p3: DVec2) -> Option<(DVec2, f64)> {
    let (b, c) = (p2 - p1, p3 - p1);
    let d = 2.0 * b.perp_dot(c);
    if d.abs() < 1e-12 {
        return None;
    }
    let (b2, c2) = (b.length_squared(), c.length_squared());
    let u = DVec2::new(c.y * b2 - b.y * c2, b.x * c2 - c.x * b2) / d;
    let r = u.length();
    if !r.is_finite() || r <= 0.0 {
        return None;
    }
    Some((p1 + u, r))
}

/// Sample the unique arc from `p1` to `p3` passing through `mid` —
/// inclusive of both endpoints, which are `p1` / `p3` verbatim. Sampling
/// density is [`arc_samples_per_turn`] for the arc's radius (`unit_scale`
/// = metres per file unit), 2..=`MAX_ARC_CHORDS` chords per arc.
fn arc_samples_2d(p1: Vec2, mid: Vec2, p3: Vec2, unit_scale: f32) -> Option<Vec<Vec2>> {
    arc_samples_2d_impl(p1, mid, p3, unit_scale, true)
}

/// `preserve_area`: scale the interior samples' radius by
/// [`arc_area_scale_pinned`] so the polygonal sector equals the circular one — right for the arcs of a
/// closed 2D profile (the QTO volume stays analytic for any chord
/// count), wrong for a 3D directrix where the path itself must stay on
/// the arc.
fn arc_samples_2d_impl(
    p1: Vec2,
    mid: Vec2,
    p3: Vec2,
    unit_scale: f32,
    preserve_area: bool,
) -> Option<Vec<Vec2>> {
    // f64 geometry, f32 only for the chord-count / area-scale rounding path
    // and the emitted points.
    let (center, radius) = circumcircle_2d(p1.as_dvec2(), mid.as_dvec2(), p3.as_dvec2())?;
    let angle = |p: Vec2| {
        let v = p.as_dvec2() - center;
        v.y.atan2(v.x)
    };
    let (theta1, theta_m, theta3) = (angle(p1), angle(mid), angle(p3));

    let ccw_to_mid = wrap_pos(theta_m - theta1);
    let ccw_to_end = wrap_pos(theta3 - theta1);

    // If mid lies on the CCW sweep from start to end (within float wiggle),
    // walk CCW; otherwise the arc goes CW.
    let (delta, ccw) = if ccw_to_mid <= ccw_to_end + 1e-5 {
        (ccw_to_end, true)
    } else {
        (wrap_pos(theta1 - theta3), false)
    };
    if delta <= 0.0 {
        return Some(vec![p1, p3]);
    }
    // Sagitta-honouring chord count, uncapped for arcs (GH #218): the old
    // 32-per-turn cap gave a 16.5° arc at R = 98 m two chords (253 mm
    // sagitta). The ratio is rounded in f32 through `chord_count`'s guard.
    let delta32 = delta as f32;
    let step = std::f32::consts::TAU / arc_samples_per_turn(radius as f32, unit_scale);
    let n = arc_chord_count(delta32 / step);
    // Interior samples only carry the sector-area radius; the endpoints are
    // the authored points (shared with the neighbouring segments), emitted
    // verbatim — scaling them jogged them (k − 1)·R outward (GH #218).
    let interior_radius = if preserve_area {
        radius * arc_area_scale_pinned(delta32, n) as f64
    } else {
        radius
    };
    let mut out = Vec::with_capacity(n + 1);
    out.push(p1);
    for i in 1..n {
        let t = i as f64 / n as f64;
        let a = theta1 + if ccw { delta * t } else { -delta * t };
        out.push((center + DVec2::new(a.cos(), a.sin()) * interior_radius).as_vec2());
    }
    out.push(p3);
    Some(out)
}

/// 3D arc sampler — projects the three control points into the plane
/// they define, samples in 2D, lifts back.
fn arc_samples_3d(p1: Vec3, mid: Vec3, p3: Vec3, unit_scale: f32) -> Option<Vec<Vec3>> {
    let v_mid = mid - p1;
    let v_end = p3 - p1;
    let normal = v_mid.cross(v_end);
    if normal.length_squared() < 1e-20 {
        return None;
    }
    let u = v_mid.try_normalize()?;
    let w = normal.try_normalize()?;
    let v = w.cross(u);
    let p1_2d = Vec2::ZERO;
    let mid_2d = Vec2::new(v_mid.dot(u), v_mid.dot(v));
    let p3_2d = Vec2::new(v_end.dot(u), v_end.dot(v));
    let samples_2d = arc_samples_2d_impl(p1_2d, mid_2d, p3_2d, unit_scale, false)?;
    Some(
        samples_2d
            .into_iter()
            .map(|q| p1 + u * q.x + v * q.y)
            .collect(),
    )
}

fn wrap_pos(a: f64) -> f64 {
    a.rem_euclid(std::f64::consts::TAU)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_typed_inline_arcindex() {
        let (name, body) = parse_typed_inline(b"IFCARCINDEX((1,2,3))").unwrap();
        assert_eq!(name, b"IFCARCINDEX");
        assert_eq!(body, b"(1,2,3)");
    }

    #[test]
    fn parse_typed_inline_lineindex_with_whitespace() {
        let (name, body) = parse_typed_inline(b"  IFCLINEINDEX( (1, 2, 3, 4) )  ").unwrap();
        assert_eq!(name, b"IFCLINEINDEX");
        // body keeps inner whitespace; parse_field handles it.
        assert!(body.starts_with(b" "));
    }

    #[test]
    fn parse_typed_inline_rejects_bare_list() {
        assert!(parse_typed_inline(b"(1,2,3)").is_none());
    }

    #[test]
    fn circumcircle_unit_circle() {
        let (c, r) = circumcircle_2d(
            DVec2::new(1.0, 0.0),
            DVec2::new(0.0, 1.0),
            DVec2::new(-1.0, 0.0),
        )
        .unwrap();
        assert!((c - DVec2::ZERO).length() < 1e-12);
        assert!((r - 1.0).abs() < 1e-12);
    }

    #[test]
    fn circumcircle_collinear() {
        assert!(circumcircle_2d(
            DVec2::new(0.0, 0.0),
            DVec2::new(1.0, 0.0),
            DVec2::new(2.0, 0.0),
        )
        .is_none());
    }

    #[test]
    fn arc_semicircle_upper_half() {
        // p1=(1,0), mid=(0,1), p3=(-1,0). Upper semicircle CCW, 1 m radius:
        // the 0.5 mm sagitta needs ~99 chords per turn → 50 here.
        let (p1, p3) = (Vec2::new(1.0, 0.0), Vec2::new(-1.0, 0.0));
        let pts = arc_samples_2d(p1, Vec2::new(0.0, 1.0), p3, 1.0).unwrap();
        assert_eq!(pts.len(), 51);
        // Endpoints are the authored points, bit-for-bit (GH #218).
        assert_eq!(pts.first().copied(), Some(p1));
        assert_eq!(pts.last().copied(), Some(p3));
        // Interior samples on the sector-area-preserving circle (GH #170):
        // radius k = arc_area_scale_pinned(π, n).
        let k = crate::mesh::profile::arc_area_scale_pinned(std::f32::consts::PI, 50);
        assert!(k > 1.0 && k < 1.001, "k = {k}");
        for p in &pts[1..pts.len() - 1] {
            assert!((p.length() - k).abs() < 1e-5, "{p:?} vs r={k}");
            assert!(p.y > 0.0);
        }
    }

    #[test]
    fn arc_semicircle_lower_half_via_mid() {
        // Same endpoints, mid below → arc goes the other way.
        let pts = arc_samples_2d(
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, -1.0),
            Vec2::new(-1.0, 0.0),
            1.0,
        )
        .unwrap();
        for p in &pts {
            assert!(p.y <= 1e-4);
        }
    }

    #[test]
    fn pipe_circle_via_two_arcindex() {
        // The pipe pattern from GH #48: 4-point list, two ArcIndex
        // semicircles forming a closed circle.
        let pts = vec![
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(-1.0, 0.0),
            Vec2::new(0.0, -1.0),
        ];
        // Segments = (IFCARCINDEX((1,2,3)), IFCARCINDEX((3,4,1)))
        let segs = b"IFCARCINDEX((1,2,3)),IFCARCINDEX((3,4,1))";
        let polyline = eval_segments_2d(&pts, segs, 1.0).unwrap();
        // 2 × 50 chords for a 1 m radius (0.5 mm sagitta), ± endpoint dedup.
        assert!(
            (99..=102).contains(&polyline.len()),
            "got {} samples — expected ~100",
            polyline.len()
        );
        // Authored points on the unit circle, interior samples on the
        // area-preserving radius (the polygon area is exactly π, see
        // `two_arc_pipe_profile_is_area_exact_for_any_chord_count`).
        for p in &polyline {
            assert!(
                (p.length() - 1.0).abs() < 1e-3 && p.length() >= 1.0 - 1e-5,
                "point {:?} not on the (area-preserving) unit circle",
                p
            );
        }
    }

    #[test]
    fn line_index_emits_indexed_points() {
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 5.0),
            Vec2::new(0.0, 5.0),
        ];
        let segs = b"IFCLINEINDEX((1,2,3,4,1))";
        let polyline = eval_segments_2d(&pts, segs, 1.0).unwrap();
        // Closed: trailing dedup drops the repeat back to point 1.
        assert_eq!(polyline.len(), 4);
        assert_eq!(polyline[0], Vec2::new(0.0, 0.0));
        assert_eq!(polyline[2], Vec2::new(10.0, 5.0));
    }

    #[test]
    fn mixed_line_and_arc() {
        // Rectangle with a rounded corner: line 1→2, arc 2→3 via mid,
        // line 3→4, back to 1.
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            // Arc center at (10, 1), going from (10,0) up CCW to (9,1)
            Vec2::new(10.0 - 0.293, 0.293), // mid on quarter-circle r=1
            Vec2::new(9.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        let segs = b"IFCLINEINDEX((1,2)),IFCARCINDEX((2,3,4)),IFCLINEINDEX((4,5,1))";
        let polyline = eval_segments_2d(&pts, segs, 1.0).unwrap();
        // Two corners + arc samples + start/end ≈ 1 (start) + 1 (arc end)
        // + 16/4=4 arc interior + 1 (corner) + 1 (close) = ~8 points.
        assert!(polyline.len() >= 6);
    }

    #[test]
    fn arc_3d_in_xy_plane() {
        let pts = arc_samples_3d(
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            1.0,
        )
        .unwrap();
        assert!(pts.len() >= 17);
        for p in &pts {
            assert!(p.z.abs() < 1e-4);
            assert!((Vec2::new(p.x, p.y).length() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn empty_segments_returns_none() {
        assert!(eval_segments_2d(&[Vec2::ZERO], b"", 1.0).is_none());
    }

    #[test]
    fn two_arc_pipe_profile_is_area_exact_for_any_chord_count() {
        // Revit IFC4 pipe: 4 points + two IfcArcIndex semicircles, r=7.5 mm.
        let r = 7.5f32;
        let pts = vec![
            Vec2::new(r, 0.0),
            Vec2::new(0.0, r),
            Vec2::new(-r, 0.0),
            Vec2::new(0.0, -r),
        ];
        let segs = b"IFCARCINDEX((1,2,3)),IFCARCINDEX((3,4,1))";
        for unit_scale in [0.001f32, 1.0] {
            let poly = eval_segments_2d(&pts, segs, unit_scale).unwrap();
            let area = signed_area_2d(&poly).abs();
            let want = std::f32::consts::PI * r * r;
            assert!(
                ((area - want) / want).abs() < 1e-4,
                "scale {unit_scale}: {area} vs {want} ({} pts)",
                poly.len()
            );
        }
        // mm file → ~9 chords per turn; metres file (7.5 m pipe) → 32.
        assert!(eval_segments_2d(&pts, segs, 0.001).unwrap().len() <= 12);
        assert!(eval_segments_2d(&pts, segs, 1.0).unwrap().len() >= 30);
    }

    #[test]
    fn exact_semicircle_is_8_chords_in_every_orientation() {
        // r = 25 mm in a millimetre file -> 16 chords per full turn by the
        // sagitta rule, so an exact semicircle is 8 chords / 9 samples. A
        // bare `ceil` on the f32 angle ratio gives 9 chords wherever libm's
        // `atan2` returns PI + 1 ulp (Apple), which made the far-origin
        // duct gate platform-dependent (GH #188).
        let r = 25.0f32;
        assert_eq!(crate::mesh::profile::circle_samples(r, 0.001), 16);
        let (east, west) = (Vec2::new(r, 0.0), Vec2::new(-r, 0.0));
        let (north, south) = (Vec2::new(0.0, r), Vec2::new(0.0, -r));
        for (p1, mid, p3) in [
            (east, north, west), // CCW over the top
            (west, north, east), // CW over the top
            (east, south, west), // CW under the bottom
            (west, south, east), // CCW under the bottom
        ] {
            let pts = arc_samples_2d(p1, mid, p3, 0.001).unwrap();
            assert_eq!(pts.len(), 9, "semicircle {p1:?} -> {mid:?} -> {p3:?}");
        }
        // Both halves of a circle built from two IfcArcIndex semicircles
        // must agree, or one side of a duct is finer than the other.
        let top = arc_samples_2d(east, north, west, 0.001).unwrap();
        let bottom = arc_samples_2d(west, south, east, 0.001).unwrap();
        assert_eq!(top.len(), bottom.len());
    }

    /// Largest deviation of the sampled polyline from the true arc
    /// (centre `c`, radius `r`): every vertex's radial offset and every
    /// chord midpoint's inset, in f64.
    fn max_arc_deviation(pts: &[Vec2], c: glam::DVec2, r: f64) -> f64 {
        let d = |p: glam::DVec2| ((p - c).length() - r).abs();
        let mut worst = 0.0f64;
        for w in pts.windows(2) {
            let (a, b) = (w[0].as_dvec2(), w[1].as_dvec2());
            worst = worst.max(d(a)).max(d((a + b) * 0.5));
        }
        worst
    }

    fn on_circle(c: glam::DVec2, r: f64, deg: f64) -> Vec2 {
        let a = deg.to_radians();
        (c + glam::DVec2::new(a.cos(), a.sin()) * r).as_vec2()
    }

    #[test]
    fn huge_radius_arc_honours_the_sagitta_and_keeps_its_endpoints() {
        // GH #218, KNM_ARK covering 14$lUH…: a 16.5° arc at R = 98 m in a
        // millimetre file. The 32-per-turn cap gave 2 chords (253 mm
        // sagitta) and pushed both endpoints (k − 1)·R = 169 mm outward.
        let (c, r) = (glam::DVec2::new(1200.0, -300.0), 98_000.0f64);
        let (p1, mid, p3) = (
            on_circle(c, r, 80.0),
            on_circle(c, r, 88.25),
            on_circle(c, r, 96.5),
        );
        for (a, m, b) in [(p1, mid, p3), (p3, mid, p1)] {
            let pts = arc_samples_2d(a, m, b, 0.001).unwrap();
            assert_eq!(pts.first().copied(), Some(a));
            assert_eq!(pts.last().copied(), Some(b));
            let n = pts.len() - 1;
            assert!((40..=50).contains(&n), "{n} chords");
            // 0.5 mm tolerance + f32 noise of 1e5-mm coordinates.
            let dev = max_arc_deviation(&pts, c, r);
            assert!(dev < 0.55, "deviation {dev} mm");
        }
        // A 3D directrix arc gets the same count, endpoints verbatim.
        let lift = |p: Vec2| Vec3::new(p.x, p.y, 7.0);
        let pts3 = arc_samples_3d(lift(p1), lift(mid), lift(p3), 0.001).unwrap();
        assert_eq!(pts3.first().copied(), Some(lift(p1)));
        assert!((pts3.last().unwrap() - lift(p3)).length() < 0.02);
        assert!((40..=50).contains(&(pts3.len() - 1)));
    }

    #[test]
    fn arc_far_from_the_profile_origin_stays_on_its_circle() {
        // GH #218, KNM_ARK opening 1_Qi_zRCcz5PCLiRPmY$_P: Revit profile
        // points at ~3.6e5 mm. The f32 circumcircle put a 9.2 m arc's
        // centre millimetres off and the bbox 5.3 mm inside the curve.
        let (c, r) = (glam::DVec2::new(353_756.0, 320_640.0), 9_220.0f64);
        let (p1, mid, p3) = (
            on_circle(c, r, -13.0),
            on_circle(c, r, 0.0),
            on_circle(c, r, 12.4),
        );
        let pts = arc_samples_2d(p1, mid, p3, 0.001).unwrap();
        // 0.5 mm tolerance + f32 ulp (0.03 mm) of the emitted points.
        let dev = max_arc_deviation(&pts, c, r);
        assert!(dev < 0.55, "deviation {dev} mm");
        let xmax = pts.iter().map(|p| p.x as f64).fold(f64::MIN, f64::max);
        assert!(
            (xmax - (c.x + r)).abs() < 0.55,
            "xmax {xmax} vs {}",
            c.x + r
        );
    }

    #[test]
    fn arc_chords_are_bounded_per_arc() {
        // A near-full turn at 10 km radius would need ~14 000 chords.
        let c = glam::DVec2::ZERO;
        let r = 10_000.0f64;
        let pts = arc_samples_2d(
            on_circle(c, r, 0.0),
            on_circle(c, r, 170.0),
            on_circle(c, r, 340.0),
            1.0,
        )
        .unwrap();
        assert_eq!(pts.len() - 1, crate::mesh::profile::MAX_ARC_CHORDS);
    }

    fn segments_cross(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> bool {
        let (a, b, c, d) = (a.as_dvec2(), b.as_dvec2(), c.as_dvec2(), d.as_dvec2());
        let o = |p: glam::DVec2, q: glam::DVec2, r: glam::DVec2| (q - p).perp_dot(r - p);
        let (d1, d2) = (o(c, d, a), o(c, d, b));
        let (d3, d4) = (o(a, b, c), o(a, b, d));
        (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0) && d1 != 0.0 && d3 != 0.0
    }

    #[test]
    fn one_millimetre_crescent_between_two_huge_arcs_stays_simple() {
        // GH #218, KNM_ARK walls 0DzHyetFz01QAC_Uwv$SXu / …SZL: a sliver
        // bounded by two R ≈ 98 m arcs sharing their endpoints, 1 mm wide
        // at the middle. Jogged endpoints + 2-chord arcs made the polygon
        // self-intersect and the extrusion over-count by 14–21 %.
        let r = 98_000.0f64;
        let half = 8.25f64.to_radians();
        let (a, b) = (
            glam::DVec2::new(r * half.cos(), -r * half.sin()),
            glam::DVec2::new(r * half.cos(), r * half.sin()),
        );
        // Shift the profile next to the origin, as the local profile frame does.
        let o = glam::DVec2::new(r * half.cos(), 0.0);
        let pts = vec![
            (a - o).as_vec2(),
            (glam::DVec2::new(r, 0.0) - o).as_vec2(),
            (b - o).as_vec2(),
            (glam::DVec2::new(r - 1.0, 0.0) - o).as_vec2(),
        ];
        let segs = b"IFCARCINDEX((1,2,3)),IFCARCINDEX((3,4,1))";
        let poly = eval_segments_2d(&pts, segs, 0.001).unwrap();
        let mut ring = poly.clone();
        if ring.first() == ring.last() {
            ring.pop();
        }
        let n = ring.len();
        for i in 0..n {
            for j in i + 2..n {
                if i == 0 && j == n - 1 {
                    continue;
                }
                let (p, q) = (ring[i], ring[(i + 1) % n]);
                let (u, v) = (ring[j], ring[(j + 1) % n]);
                assert!(!segments_cross(p, q, u, v), "edges {i} and {j} cross");
            }
        }
        // Analytic crescent: outer circular segment minus inner one, both
        // on chord AB (inner circle through A, B and (r − 1, 0)).
        let chord = (b - a).length();
        let seg = |rad: f64| {
            let phi = 2.0 * (chord / (2.0 * rad)).asin();
            rad * rad / 2.0 * (phi - phi.sin())
        };
        let h = 0.5 * chord;
        let s_in = r - 1.0 - r * half.cos(); // inner sagitta
        let r_in = (h * h + s_in * s_in) / (2.0 * s_in);
        let want = seg(r) - seg(r_in);
        // f64 shoelace: f32 loses the 1 mm sliver against 14 m coordinates.
        let got = (0..n)
            .map(|i| ring[i].as_dvec2().perp_dot(ring[(i + 1) % n].as_dvec2()))
            .sum::<f64>()
            .abs()
            / 2.0;
        assert!(((got - want) / want).abs() < 0.01, "{got} vs {want} mm²");
    }

    #[test]
    fn unknown_segment_type_returns_none() {
        let pts = vec![Vec2::ZERO, Vec2::X, Vec2::Y];
        assert!(eval_segments_2d(&pts, b"IFCBSPLINEINDEX((1,2,3))", 1.0).is_none());
    }
}
