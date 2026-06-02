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
//! Sampling density matches `profile::CURVE_SAMPLES` (32 samples per full
//! circle), scaled by arc angle so a semicircle gets 16 chord segments.

use glam::{Vec2, Vec3};

use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::profile::CURVE_SAMPLES;

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
pub fn eval_segments_2d(pts: &[Vec2], segments_raw: &[u8]) -> Option<Vec<Vec2>> {
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
            let samples =
                arc_samples_2d(p1, mid, p3).unwrap_or_else(|| vec![p1, p3]);
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
    if out.len() > 2
        && (*out.first().unwrap() - *out.last().unwrap()).length_squared() < EPS_DEDUP
    {
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
pub fn eval_segments_3d(pts: &[Vec3], segments_raw: &[u8]) -> Option<Vec<Vec3>> {
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
            let samples =
                arc_samples_3d(p1, mid, p3).unwrap_or_else(|| vec![p1, p3]);
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
    if out.len() > 2
        && (*out.first().unwrap() - *out.last().unwrap()).length_squared() < EPS_DEDUP
    {
        out.pop();
    }
    Some(out)
}

const EPS_DEDUP: f32 = 1e-12;

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
    let start = if let (Some(&last), Some(&first)) = (out.last(), samples.first()) {
        if (last - first).length_squared() < EPS_DEDUP {
            1
        } else {
            0
        }
    } else {
        0
    };
    out.extend_from_slice(&samples[start..]);
}

fn append_dedup_3d(out: &mut Vec<Vec3>, samples: &[Vec3]) {
    let start = if let (Some(&last), Some(&first)) = (out.last(), samples.first()) {
        if (last - first).length_squared() < EPS_DEDUP {
            1
        } else {
            0
        }
    } else {
        0
    };
    out.extend_from_slice(&samples[start..]);
}

/// Unique circle through three 2D points. `None` for collinear inputs.
fn circumcircle_2d(p1: Vec2, p2: Vec2, p3: Vec2) -> Option<(Vec2, f32)> {
    let (ax, ay) = (p1.x, p1.y);
    let (bx, by) = (p2.x, p2.y);
    let (cx, cy) = (p3.x, p3.y);
    let d = 2.0 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
    if d.abs() < 1e-12 {
        return None;
    }
    let a2 = ax * ax + ay * ay;
    let b2 = bx * bx + by * by;
    let c2 = cx * cx + cy * cy;
    let ux = (a2 * (by - cy) + b2 * (cy - ay) + c2 * (ay - by)) / d;
    let uy = (a2 * (cx - bx) + b2 * (ax - cx) + c2 * (bx - ax)) / d;
    let center = Vec2::new(ux, uy);
    let r = (p1 - center).length();
    if !r.is_finite() || r <= 0.0 {
        return None;
    }
    Some((center, r))
}

/// Sample the unique arc from `p1` to `p3` passing through `mid` —
/// inclusive of both endpoints. Sampling density matches
/// `CURVE_SAMPLES` per full turn, with a minimum of two chord
/// segments per arc.
fn arc_samples_2d(p1: Vec2, mid: Vec2, p3: Vec2) -> Option<Vec<Vec2>> {
    let (center, radius) = circumcircle_2d(p1, mid, p3)?;
    let theta1 = (p1 - center).y.atan2((p1 - center).x);
    let theta_m = (mid - center).y.atan2((mid - center).x);
    let theta3 = (p3 - center).y.atan2((p3 - center).x);

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
    let step = std::f32::consts::TAU / CURVE_SAMPLES as f32;
    let n = ((delta / step).ceil() as usize).max(2);
    let mut out = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let signed = if ccw { delta * t } else { -delta * t };
        let a = theta1 + signed;
        out.push(center + Vec2::new(a.cos() * radius, a.sin() * radius));
    }
    Some(out)
}

/// 3D arc sampler — projects the three control points into the plane
/// they define, samples in 2D, lifts back.
fn arc_samples_3d(p1: Vec3, mid: Vec3, p3: Vec3) -> Option<Vec<Vec3>> {
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
    let samples_2d = arc_samples_2d(p1_2d, mid_2d, p3_2d)?;
    Some(
        samples_2d
            .into_iter()
            .map(|q| p1 + u * q.x + v * q.y)
            .collect(),
    )
}

fn wrap_pos(a: f32) -> f32 {
    let two_pi = std::f32::consts::TAU;
    let mut x = a % two_pi;
    if x < 0.0 {
        x += two_pi;
    }
    x
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
        let (name, body) =
            parse_typed_inline(b"  IFCLINEINDEX( (1, 2, 3, 4) )  ").unwrap();
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
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(-1.0, 0.0),
        )
        .unwrap();
        assert!((c - Vec2::ZERO).length() < 1e-5);
        assert!((r - 1.0).abs() < 1e-5);
    }

    #[test]
    fn circumcircle_collinear() {
        assert!(circumcircle_2d(
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(2.0, 0.0),
        )
        .is_none());
    }

    #[test]
    fn arc_semicircle_upper_half() {
        // p1=(1,0), mid=(0,1), p3=(-1,0). Upper semicircle CCW. Should
        // emit at least 16 chord segments (half of CURVE_SAMPLES=32).
        let pts = arc_samples_2d(
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(-1.0, 0.0),
        )
        .unwrap();
        assert!(pts.len() >= 17);
        // All samples on unit circle.
        for p in &pts {
            assert!((p.length() - 1.0).abs() < 1e-4);
        }
        // All samples in upper half-plane (with eps for endpoints).
        for p in &pts {
            assert!(p.y >= -1e-4);
        }
        // Endpoints match.
        assert!((pts.first().unwrap() - &Vec2::new(1.0, 0.0)).length() < 1e-4);
        assert!((pts.last().unwrap() - &Vec2::new(-1.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn arc_semicircle_lower_half_via_mid() {
        // Same endpoints, mid below → arc goes the other way.
        let pts = arc_samples_2d(
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, -1.0),
            Vec2::new(-1.0, 0.0),
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
        let polyline = eval_segments_2d(&pts, segs).unwrap();
        // Should land somewhere near 2 * 16 = 32 chord segments.
        // (Plus minus one for endpoint dedup.)
        assert!(
            polyline.len() >= 30 && polyline.len() <= 34,
            "got {} samples — expected ~32",
            polyline.len()
        );
        // All on unit circle.
        for p in &polyline {
            assert!(
                (p.length() - 1.0).abs() < 1e-3,
                "point {:?} not on unit circle",
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
        let polyline = eval_segments_2d(&pts, segs).unwrap();
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
        let segs =
            b"IFCLINEINDEX((1,2)),IFCARCINDEX((2,3,4)),IFCLINEINDEX((4,5,1))";
        let polyline = eval_segments_2d(&pts, segs).unwrap();
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
        assert!(eval_segments_2d(&[Vec2::ZERO], b"").is_none());
    }

    #[test]
    fn unknown_segment_type_returns_none() {
        let pts = vec![Vec2::ZERO, Vec2::X, Vec2::Y];
        assert!(eval_segments_2d(&pts, b"IFCBSPLINEINDEX((1,2,3))").is_none());
    }
}
