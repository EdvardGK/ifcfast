//! 2D profile vertex generation.
//!
//! Each IFC profile def maps to a closed polygon (or polygon + holes) in
//! the profile plane. Per Agent C's port spec, every profile may carry
//! its own `Position` (`IfcAxis2Placement2D`) — applied to every output
//! vertex.
//!
//! Curve sampling: 32 segments for circles / ellipses. Good enough for
//! BIM rendering; trivially configurable per use case.

use glam::{Mat3, Vec2, Vec3};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, parse_ref_list, split_top_level_args, Field};

pub const CURVE_SAMPLES: usize = 32;

/// A 2D profile polygon — one outer loop + any number of inner holes.
#[derive(Debug, Clone, Default)]
pub struct Polygon2D {
    pub outer: Vec<Vec2>,
    pub holes: Vec<Vec<Vec2>>,
}

/// Resolve an `IfcProfileDef` reference to a closed `Polygon2D`.
/// Returns None for profile subtypes we can't handle yet.
pub fn extract(table: &EntityTable, id: u64) -> Option<Polygon2D> {
    let (type_name, args) = table.get(id)?;
    let fields = split_top_level_args(args);

    // Shared layout for all IfcProfileDef subtypes:
    //   arg[0] = ProfileType (.CURVE. / .AREA.)
    //   arg[1] = ProfileName (label or $)
    // The remaining args are subtype-specific.

    let polygon = if type_name.eq_ignore_ascii_case(b"IFCRECTANGLEPROFILEDEF") {
        // (Position, XDim, YDim) at arg[2..5]
        rectangle(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCROUNDEDRECTANGLEPROFILEDEF") {
        // Same as rectangle but with RoundingRadius — approximate as plain rect for now.
        rectangle(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCCIRCLEPROFILEDEF") {
        circle(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCCIRCLEHOLLOWPROFILEDEF") {
        circle_hollow(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCELLIPSEPROFILEDEF") {
        ellipse(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCISHAPEPROFILEDEF") {
        i_shape(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCLSHAPEPROFILEDEF") {
        l_shape(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCUSHAPEPROFILEDEF") {
        u_shape(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCTSHAPEPROFILEDEF") {
        t_shape(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCZSHAPEPROFILEDEF") {
        z_shape(&fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCARBITRARYCLOSEDPROFILEDEF") {
        // arg[2] = OuterCurve
        arbitrary_closed(table, &fields)?
    } else if type_name.eq_ignore_ascii_case(b"IFCARBITRARYPROFILEDEFWITHVOIDS") {
        // arg[2] = OuterCurve, arg[3] = InnerCurves (list of refs)
        arbitrary_with_voids(table, &fields)?
    } else {
        return None;
    };

    // Apply the profile's own Position transform if present (arg[2] is
    // Position for parametric profiles; for arbitrary it's the curve).
    let positioned = if type_name.starts_with(b"IFCARBITRARY") {
        polygon
    } else {
        apply_profile_position(table, &fields, polygon)
    };
    Some(normalize_winding(positioned))
}

/// Twice the shoelace signed area of a 2D loop, accumulated in f64 so
/// far-origin mm-scale coordinates don't lose the sign to f32 cancellation.
/// Positive = counter-clockwise.
fn signed_area_2(pts: &[Vec2]) -> f64 {
    let n = pts.len();
    let mut acc = 0.0_f64;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        acc += a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64;
    }
    acc
}

/// Enforce the `Polygon2D` winding invariant every consumer assumes:
/// **outer CCW, holes CW**. IFC does not mandate a winding for profile
/// curves and Revit routinely authors both `IfcPolyline` outers and voids
/// clockwise. Downstream that assumption is load-bearing twice over —
/// `extrude_polygon` derives cap normals from earcut output (which follows
/// the outer ring's winding) and winds wall quads per loop, so a
/// wrong-winding hole inverts its wall normals: the divergence-theorem
/// volume then *adds* the void instead of subtracting it and edge-pairing
/// flags the mesh `open_shell` (G55_ARK: all 208 windows, mesh volume
/// 8× the kernel, GH #62/#121 window residue). Normalising by signed area
/// at the single exit point makes every authored winding safe; loops that
/// already comply are untouched. Zero-area (degenerate) loops keep their
/// order — reversing them is meaningless either way.
fn normalize_winding(mut polygon: Polygon2D) -> Polygon2D {
    if signed_area_2(&polygon.outer) < 0.0 {
        polygon.outer.reverse();
    }
    for hole in &mut polygon.holes {
        if signed_area_2(hole) > 0.0 {
            hole.reverse();
        }
    }
    polygon
}

// ----------------------------------------------------------------------
// Parametric profiles
// ----------------------------------------------------------------------

fn rectangle(fields: &[&[u8]]) -> Option<Polygon2D> {
    // (ProfileType, ProfileName, Position, XDim, YDim)
    let x_dim = number_at(fields, 3)?;
    let y_dim = number_at(fields, 4)?;
    let hw = (x_dim * 0.5) as f32;
    let hd = (y_dim * 0.5) as f32;
    Some(Polygon2D {
        outer: vec![
            Vec2::new(-hw, -hd),
            Vec2::new(hw, -hd),
            Vec2::new(hw, hd),
            Vec2::new(-hw, hd),
        ],
        holes: Vec::new(),
    })
}

fn circle(fields: &[&[u8]]) -> Option<Polygon2D> {
    // (ProfileType, ProfileName, Position, Radius)
    let r = number_at(fields, 3)? as f32;
    Some(Polygon2D {
        outer: sample_ellipse(r, r, CURVE_SAMPLES),
        holes: Vec::new(),
    })
}

fn circle_hollow(fields: &[&[u8]]) -> Option<Polygon2D> {
    // (ProfileType, ProfileName, Position, Radius, WallThickness)
    let r_outer = number_at(fields, 3)? as f32;
    let t = number_at(fields, 4)? as f32;
    let r_inner = (r_outer - t).max(0.0);
    let outer = sample_ellipse(r_outer, r_outer, CURVE_SAMPLES);
    let mut hole = sample_ellipse(r_inner, r_inner, CURVE_SAMPLES);
    hole.reverse(); // CW for hole
    Some(Polygon2D {
        outer,
        holes: vec![hole],
    })
}

fn ellipse(fields: &[&[u8]]) -> Option<Polygon2D> {
    // (ProfileType, ProfileName, Position, SemiAxis1, SemiAxis2)
    let a = number_at(fields, 3)? as f32;
    let b = number_at(fields, 4)? as f32;
    Some(Polygon2D {
        outer: sample_ellipse(a, b, CURVE_SAMPLES),
        holes: Vec::new(),
    })
}

fn i_shape(fields: &[&[u8]]) -> Option<Polygon2D> {
    // (ProfileType, ProfileName, Position, OverallWidth, OverallDepth,
    //  WebThickness, FlangeThickness, FilletRadius, FlangeEdgeRadius, FlangeSlope)
    let bf = number_at(fields, 3)? as f32; // OverallWidth
    let d = number_at(fields, 4)? as f32; // OverallDepth
    let tw = number_at(fields, 5)? as f32; // WebThickness
    let tf = number_at(fields, 6)? as f32; // FlangeThickness
    let half_bf = bf * 0.5;
    let half_d = d * 0.5;
    let half_tw = tw * 0.5;
    let y_inner = half_d - tf;
    Some(Polygon2D {
        outer: vec![
            Vec2::new(-half_bf, -half_d),
            Vec2::new(half_bf, -half_d),
            Vec2::new(half_bf, -y_inner),
            Vec2::new(half_tw, -y_inner),
            Vec2::new(half_tw, y_inner),
            Vec2::new(half_bf, y_inner),
            Vec2::new(half_bf, half_d),
            Vec2::new(-half_bf, half_d),
            Vec2::new(-half_bf, y_inner),
            Vec2::new(-half_tw, y_inner),
            Vec2::new(-half_tw, -y_inner),
            Vec2::new(-half_bf, -y_inner),
        ],
        holes: Vec::new(),
    })
}

fn l_shape(fields: &[&[u8]]) -> Option<Polygon2D> {
    // (ProfileType, ProfileName, Position, Depth, Width, Thickness, ...)
    let d = number_at(fields, 3)? as f32;
    let w = number_at(fields, 4).unwrap_or_else(|| number_at(fields, 3).unwrap_or(0.0)) as f32;
    let t = number_at(fields, 5)? as f32;
    Some(Polygon2D {
        outer: vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(w, 0.0),
            Vec2::new(w, t),
            Vec2::new(t, t),
            Vec2::new(t, d),
            Vec2::new(0.0, d),
        ],
        holes: Vec::new(),
    })
}

fn u_shape(fields: &[&[u8]]) -> Option<Polygon2D> {
    // (Depth, FlangeWidth, WebThickness, FlangeThickness)
    let d = number_at(fields, 3)? as f32;
    let bf = number_at(fields, 4)? as f32;
    let tw = number_at(fields, 5)? as f32;
    let tf = number_at(fields, 6)? as f32;
    Some(Polygon2D {
        outer: vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(bf, 0.0),
            Vec2::new(bf, tf),
            Vec2::new(tw, tf),
            Vec2::new(tw, d - tf),
            Vec2::new(bf, d - tf),
            Vec2::new(bf, d),
            Vec2::new(0.0, d),
        ],
        holes: Vec::new(),
    })
}

fn t_shape(fields: &[&[u8]]) -> Option<Polygon2D> {
    // (Depth, FlangeWidth, WebThickness, FlangeThickness)
    // T centered on origin, flange at top.
    let d = number_at(fields, 3)? as f32;
    let bf = number_at(fields, 4)? as f32;
    let tw = number_at(fields, 5)? as f32;
    let tf = number_at(fields, 6)? as f32;
    let half_bf = bf * 0.5;
    let half_tw = tw * 0.5;
    Some(Polygon2D {
        outer: vec![
            Vec2::new(-half_tw, 0.0),
            Vec2::new(half_tw, 0.0),
            Vec2::new(half_tw, d - tf),
            Vec2::new(half_bf, d - tf),
            Vec2::new(half_bf, d),
            Vec2::new(-half_bf, d),
            Vec2::new(-half_bf, d - tf),
            Vec2::new(-half_tw, d - tf),
        ],
        holes: Vec::new(),
    })
}

fn z_shape(fields: &[&[u8]]) -> Option<Polygon2D> {
    // (Depth, FlangeWidth, WebThickness, FlangeThickness)
    let d = number_at(fields, 3)? as f32;
    let bf = number_at(fields, 4)? as f32;
    let tw = number_at(fields, 5)? as f32;
    let tf = number_at(fields, 6)? as f32;
    let half_tw = tw * 0.5;
    Some(Polygon2D {
        outer: vec![
            Vec2::new(-half_tw - bf, 0.0),
            Vec2::new(half_tw, 0.0),
            Vec2::new(half_tw, d - tf),
            Vec2::new(half_tw + bf, d - tf),
            Vec2::new(half_tw + bf, d),
            Vec2::new(-half_tw, d),
            Vec2::new(-half_tw, tf),
            Vec2::new(-half_tw - bf, tf),
        ],
        holes: Vec::new(),
    })
}

// ----------------------------------------------------------------------
// Arbitrary profiles — walk OuterCurve / InnerCurves
// ----------------------------------------------------------------------

fn arbitrary_closed(table: &EntityTable, fields: &[&[u8]]) -> Option<Polygon2D> {
    // (ProfileType, ProfileName, OuterCurve)
    let curve_id = ref_at(fields, 2)?;
    let outer = curve_to_polyline(table, curve_id)?;
    Some(Polygon2D {
        outer,
        holes: Vec::new(),
    })
}

fn arbitrary_with_voids(table: &EntityTable, fields: &[&[u8]]) -> Option<Polygon2D> {
    // (ProfileType, ProfileName, OuterCurve, InnerCurves)
    let curve_id = ref_at(fields, 2)?;
    let outer = curve_to_polyline(table, curve_id)?;
    let holes_field = fields.get(3).copied()?;
    let body = match parse_field(holes_field) {
        Field::List(b) => b,
        _ => {
            return Some(Polygon2D {
                outer,
                holes: vec![],
            })
        }
    };
    // Holes are pushed as authored — `normalize_winding` at the `extract`
    // exit forces them CW by signed area. (A blind `reverse()` here was
    // the GH #62 window bug: Revit authors voids CW already, so reversing
    // made them CCW and inverted every hole-wall normal downstream.)
    let mut holes: Vec<Vec<Vec2>> = Vec::new();
    for hole_field in split_top_level_args(body) {
        if let Field::Ref(hid) = parse_field(hole_field) {
            if let Some(hole) = curve_to_polyline(table, hid) {
                holes.push(hole);
            }
        }
    }
    Some(Polygon2D { outer, holes })
}

fn curve_to_polyline(table: &EntityTable, curve_id: u64) -> Option<Vec<Vec2>> {
    let (type_name, args) = table.get(curve_id)?;
    let fields = split_top_level_args(args);
    if type_name.eq_ignore_ascii_case(b"IFCPOLYLINE") {
        // (Points: LIST OF IfcCartesianPoint)
        let body = match parse_field(fields.first()?) {
            Field::List(b) => b,
            _ => return None,
        };
        let mut pts = Vec::new();
        for f in split_top_level_args(body) {
            if let Field::Ref(pid) = parse_field(f) {
                if let Some(p) = cartesian_point_2d(table, pid) {
                    pts.push(p);
                }
            }
        }
        // Drop the duplicate closing point if present.
        if pts.len() > 2 && pts.first() == pts.last() {
            pts.pop();
        }
        return Some(pts);
    }
    if type_name.eq_ignore_ascii_case(b"IFCINDEXEDPOLYCURVE") {
        // (Points: IfcCartesianPointList2D ref, Segments, SelfIntersect)
        let pts_id = match parse_field(fields.first()?) {
            Field::Ref(id) => id,
            _ => return None,
        };
        let raw_pts = point_list_2d_raw(table, pts_id)?;
        // Segments is optional — when present, evaluate IfcArcIndex /
        // IfcLineIndex against the raw point list. Without this every
        // Revit MEP pipe (4 points + 2 IfcArcIndex semicircles)
        // collapses to a 4-sided prism — GH #48.
        if let Some(seg_body) = list_body(fields.get(1).copied()) {
            if let Some(poly) = crate::mesh::indexed_curve::eval_segments_2d(&raw_pts, seg_body) {
                return Some(poly);
            }
        }
        // Segments = $ (or unrecognised): connect raw points in order,
        // dropping the optional trailing close vertex.
        let mut pts = raw_pts;
        if pts.len() > 2 && pts.first() == pts.last() {
            pts.pop();
        }
        return Some(pts);
    }
    if type_name.eq_ignore_ascii_case(b"IFCCOMPOSITECURVE") {
        // (Segments: LIST OF IfcCompositeCurveSegment, SelfIntersect)
        return composite_curve(table, &fields);
    }
    if type_name.eq_ignore_ascii_case(b"IFCCIRCLE") {
        // (Position: IfcAxis2Placement2D, Radius). Full circle — sample
        // into a CCW polyline. This is the common pipe / round-column
        // cross-section when authored as an IfcArbitraryProfileDef(WithVoids)
        // rather than the parametric IfcCircleProfileDef. Without this the
        // whole profile resolves to None and the product emits an empty
        // mesh — the annular-pipe empty-mesh failure mode.
        return circle_curve_2d(table, &fields);
    }
    if type_name.eq_ignore_ascii_case(b"IFCTRIMMEDCURVE") {
        // (BasisCurve, Trim1, Trim2, SenseAgreement, MasterRepresentation).
        // An arc segment inside an IfcCompositeCurve — the profile curve of
        // thin *curved* walls (Revit "15mm flis" etc.). Before GH #123 the
        // arc returned None and the composite walk `continue`d past it, so a
        // two-arc profile collapsed to its two straight end-cap lines: a
        // ~0-area sliver → open tube mesh → prism_fallback 8-9x over-count.
        // Circle, ellipse and line basis curves are all sampled; conic trim
        // parameters honour the model's PLANEANGLEUNIT (GH #139).
        return trimmed_curve_2d(table, &fields);
    }
    // A bare IfcEllipse / IfcLine as a *direct* composite-curve parent (not
    // wrapped in an IfcTrimmedCurve) is a full conic / infinite line — not a
    // closed profile boundary on its own; resolving to None is correct.
    None
}

/// Sample an `IfcTrimmedCurve` into a polyline arc, dispatching on the basis
/// curve type:
/// - `IfcCircle` — circular arc (`a == b == Radius`).
/// - `IfcEllipse` — elliptical arc (`a = SemiAxis1`, `b = SemiAxis2`).
/// - `IfcLine` — a straight segment between the two trim points (lengths
///   along the line, not angles).
///
/// Conic (circle / ellipse) trim parameters authored as `IfcParameterValue`
/// are angles scaled by the model's `PLANEANGLEUNIT`
/// ([`resolve_plane_angle_scale`]); CARTESIAN trims are points resolved via
/// `atan2` in the conic's local frame (unit-safe). Orientation follows
/// `SenseAgreement` (arg 3): `T` runs the arc in the natural
/// CCW / increasing-parameter direction from `Trim1` to `Trim2`, `F` the
/// reverse; the composite-curve caller applies its own per-segment
/// `SameSense` reversal on top. Endpoints are inclusive so the arc joins its
/// neighbours in the composite walk.
fn trimmed_curve_2d(table: &EntityTable, fields: &[&[u8]]) -> Option<Vec<Vec2>> {
    let basis_id = match parse_field(fields.first()?) {
        Field::Ref(id) => id,
        _ => return None,
    };
    let (basis_type, basis_args) = table.get(basis_id)?;
    let basis_fields = split_top_level_args(basis_args);
    if basis_type.eq_ignore_ascii_case(b"IFCCIRCLE") {
        // (Position, Radius)
        let radius = number_at(&basis_fields, 1)? as f32;
        if !(radius.is_finite() && radius > 0.0) {
            return None;
        }
        conic_arc(table, fields, &basis_fields, radius, radius)
    } else if basis_type.eq_ignore_ascii_case(b"IFCELLIPSE") {
        // (Position, SemiAxis1, SemiAxis2)
        let a = number_at(&basis_fields, 1)? as f32;
        let b = number_at(&basis_fields, 2)? as f32;
        if !(a.is_finite() && a > 0.0 && b.is_finite() && b > 0.0) {
            return None;
        }
        conic_arc(table, fields, &basis_fields, a, b)
    } else if basis_type.eq_ignore_ascii_case(b"IFCLINE") {
        // (Pnt, Dir: IfcVector)
        trimmed_line_2d(table, fields, &basis_fields)
    } else {
        None
    }
}

/// Sample a trimmed circle / ellipse arc. `a` / `b` are the local semi-axes
/// (equal for a circle); the trim parameters are angles in the conic's local
/// frame. Shared by the circle and ellipse basis paths of
/// [`trimmed_curve_2d`].
fn conic_arc(
    table: &EntityTable,
    fields: &[&[u8]],
    basis_fields: &[&[u8]],
    a: f32,
    b: f32,
) -> Option<Vec<Vec2>> {
    let (center, ref_dir) = match basis_fields.first().copied().map(parse_field) {
        Some(Field::Ref(pid)) => placement2d_origin_dir(table, pid),
        _ => (Vec2::ZERO, Vec2::X),
    };
    // Radians per authored PARAMETER trim unit. Resolved once per arc — only
    // trimmed conic profiles (a rare curved-wall case) pay for the unit walk;
    // the thousands of ordinary products never reach here.
    let pa = resolve_plane_angle_scale(table);
    let semi = Vec2::new(a, b);
    let a1 = trim_angle(table, fields.get(1).copied(), center, ref_dir, semi, pa)?;
    let a2 = trim_angle(table, fields.get(2).copied(), center, ref_dir, semi, pa)?;
    let sense = matches!(
        fields.get(3).copied().map(parse_field),
        Some(Field::Enum(b"T"))
    );
    let (start, end) = arc_span(a1, a2, sense)?;
    let sweep = (end - start).abs();
    let n = ((CURVE_SAMPLES as f32) * sweep / std::f32::consts::TAU)
        .ceil()
        .max(2.0) as usize;
    let (cos, sin) = (ref_dir.x, ref_dir.y);
    let pts = (0..=n)
        .map(|i| {
            let t = start + (end - start) * (i as f32 / n as f32);
            let lx = a * t.cos();
            let ly = b * t.sin();
            Vec2::new(
                center.x + cos * lx - sin * ly,
                center.y + sin * lx + cos * ly,
            )
        })
        .collect();
    Some(pts)
}

/// Sample an `IfcTrimmedCurve` on an `IfcLine` basis into a 2-vertex segment.
/// Line trims are lengths along the line (`P(u) = Pnt + u·Magnitude·dir`),
/// NOT angles — the `PLANEANGLEUNIT` scaling does not apply. PARAMETER trims
/// give `u` directly; CARTESIAN trims are points projected onto the line.
/// Straight composite segments are usually authored as `IfcPolyline`, so this
/// basis is uncommon — but handling it removes the last basis that could
/// collapse a profile to `None`.
fn trimmed_line_2d(
    table: &EntityTable,
    fields: &[&[u8]],
    basis_fields: &[&[u8]],
) -> Option<Vec<Vec2>> {
    let pnt = match basis_fields.first().copied().map(parse_field) {
        Some(Field::Ref(pid)) => cartesian_point_2d(table, pid)?,
        _ => return None,
    };
    let (dir, mag) = match basis_fields.get(1).copied().map(parse_field) {
        Some(Field::Ref(vid)) => vector_2d(table, vid)?,
        _ => return None,
    };
    let u1 = trim_line_param(table, fields.get(1).copied(), pnt, dir, mag)?;
    let u2 = trim_line_param(table, fields.get(2).copied(), pnt, dir, mag)?;
    let p1 = pnt + dir * (u1 * mag);
    let p2 = pnt + dir * (u2 * mag);
    // SenseAgreement `F` runs the segment Trim2 -> Trim1.
    let sense = matches!(
        fields.get(3).copied().map(parse_field),
        Some(Field::Enum(b"T"))
    );
    let (start, end) = if sense { (p1, p2) } else { (p2, p1) };
    if (start - end).length_squared() < 1e-12 {
        return None;
    }
    Some(vec![start, end])
}

/// Resolve one `IfcTrimmingSelect` (a 1-2 element SET) to an angle in the
/// conic's local frame (radians). Prefers a CARTESIAN point (unambiguous via
/// `atan2`, with the ellipse semi-axes divided out so the parameter `t` is
/// recovered — a no-op for a circle where `semi.x == semi.y`); otherwise
/// reads an `IfcParameterValue` and scales it by `pa` (radians per authored
/// plane-angle unit, from [`resolve_plane_angle_scale`]).
fn trim_angle(
    table: &EntityTable,
    field: Option<&[u8]>,
    center: Vec2,
    ref_dir: Vec2,
    semi: Vec2,
    pa: f32,
) -> Option<f32> {
    let body = match parse_field(field?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut param: Option<f64> = None;
    for sel in split_top_level_args(body) {
        match parse_field(sel) {
            Field::Ref(pid) => {
                if let Some(p) = cartesian_point_2d(table, pid) {
                    // Un-rotate the world point into the conic's local frame,
                    // then divide out the semi-axes so an ellipse recovers its
                    // parameter t (identity for a circle).
                    let d = p - center;
                    let (cos, sin) = (ref_dir.x, ref_dir.y);
                    let lx = cos * d.x + sin * d.y;
                    let ly = -sin * d.x + cos * d.y;
                    return Some((ly / semi.y).atan2(lx / semi.x));
                }
            }
            _ => {
                if param.is_none() {
                    param = parameter_value(sel);
                }
            }
        }
    }
    param.map(|v| (v as f32) * pa)
}

/// Resolve one `IfcTrimmingSelect` to a line parameter `u` (a dimensionless
/// multiple of the basis `IfcVector`). CARTESIAN trims project the point onto
/// the line; PARAMETER trims are used directly.
fn trim_line_param(
    table: &EntityTable,
    field: Option<&[u8]>,
    pnt: Vec2,
    dir: Vec2,
    mag: f32,
) -> Option<f32> {
    let body = match parse_field(field?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut param: Option<f64> = None;
    for sel in split_top_level_args(body) {
        match parse_field(sel) {
            Field::Ref(pid) => {
                if let Some(q) = cartesian_point_2d(table, pid) {
                    return Some((q - pnt).dot(dir) / mag);
                }
            }
            _ => {
                if param.is_none() {
                    param = parameter_value(sel);
                }
            }
        }
    }
    param.map(|v| v as f32)
}

/// Extract the scalar from an `IfcParameterValue(x)` / `IfcReal(x)` wrapper
/// (or a bare number).
fn parameter_value(raw: &[u8]) -> Option<f64> {
    if let Field::Number(n) = parse_field(raw) {
        return Some(n);
    }
    let open = raw.iter().position(|&b| b == b'(')?;
    let close = raw.iter().rposition(|&b| b == b')')?;
    if close <= open + 1 {
        return None;
    }
    std::str::from_utf8(&raw[open + 1..close])
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Radians per authored plane-angle unit, for scaling `IfcParameterValue`
/// conic trim parameters. Walks the `IfcUnitAssignment.Units` for a
/// PLANEANGLEUNIT:
/// - `IfcSIUnit(*,.PLANEANGLEUNIT.,$,.RADIAN.)` → `1.0` (already radians).
/// - `IfcConversionBasedUnit(_,.PLANEANGLEUNIT.,'DEGREE',#f)` where
///   `#f = IfcMeasureWithUnit(IfcPlaneAngleMeasure(v), _)` → `v`
///   (`0.017453… = π/180` for DEGREE, read straight off the measure).
///
/// Default when no PLANEANGLEUNIT resolves: **π/180 (degrees)** — a
/// deliberate deviation from IFC's spec default of RADIAN. Revit / ArchiCAD
/// author conic trims in degrees (a semicircle trims `180.0`) and routinely
/// omit the plane-angle declaration; defaulting to radians would turn every
/// undeclared `180` into a ~28-turn sweep. Files that *do* declare RADIAN get
/// `1.0` and parse correctly. Resolves strictly through the assignment's
/// Units, so a stray RADIAN `IfcSIUnit` not referenced by the assignment
/// (both G55 files carry one) is ignored.
fn resolve_plane_angle_scale(table: &EntityTable) -> f32 {
    const DEGREE: f32 = std::f32::consts::PI / 180.0;
    resolve_plane_angle_scale_opt(table).unwrap_or(DEGREE)
}

/// The declared PLANEANGLEUNIT scale, or `None` when the file declares
/// none. Split out from [`resolve_plane_angle_scale`] (GH #155) because
/// the degrees-default above is a *trim-parameter* heuristic: an
/// `IfcParameterValue` conic trim is ambiguous and authored in degrees
/// in practice. A true `IfcPlaneAngleMeasure` (e.g.
/// `IfcRevolvedAreaSolid.Angle`) is NOT ambiguous — the schema says it
/// is in the declared unit, radians when undeclared — so that caller
/// takes the `Option` and defaults to radians, which is also what
/// ifcopenshell does.
pub(crate) fn resolve_plane_angle_scale_opt(table: &EntityTable) -> Option<f32> {
    // The unit assignment can be the last entity in a multi-million-entry
    // file (G55_ARK: penultimate of ~2.8M), so use the table's memoized
    // lookup — one scan per model — rather than re-scanning per arc.
    let unit_refs = table
        .unit_assignment_id()
        .and_then(|id| table.get(id))
        .map(|(_, args)| {
            let fields = split_top_level_args(args);
            match fields.first().copied().map(parse_field) {
                Some(Field::List(b)) => parse_ref_list(b),
                _ => Vec::new(),
            }
        })
        .unwrap_or_default();
    for uref in unit_refs {
        let (utype, uargs) = match table.get(uref) {
            Some(x) => x,
            None => continue,
        };
        let uf = split_top_level_args(uargs);
        // UnitType is arg[1] on both IfcSIUnit and IfcConversionBasedUnit.
        let is_plane_angle = matches!(
            uf.get(1).copied().map(parse_field),
            Some(Field::Enum(b"PLANEANGLEUNIT"))
        );
        if !is_plane_angle {
            continue;
        }
        if utype.eq_ignore_ascii_case(b"IFCSIUNIT") {
            // (Dimensions, UnitType, Prefix, Name) — RADIAN is the only SI
            // plane-angle name.
            if matches!(
                uf.get(3).copied().map(parse_field),
                Some(Field::Enum(b"RADIAN"))
            ) {
                return Some(1.0);
            }
        } else if utype.eq_ignore_ascii_case(b"IFCCONVERSIONBASEDUNIT") {
            // (Dimensions, UnitType, Name, ConversionFactor) → follow the
            // factor to IfcMeasureWithUnit and read its radians-per-unit value.
            let factor_ref = match uf.get(3).copied().map(parse_field) {
                Some(Field::Ref(id)) => id,
                _ => continue,
            };
            if let Some((mtype, margs)) = table.get(factor_ref) {
                if mtype.eq_ignore_ascii_case(b"IFCMEASUREWITHUNIT") {
                    let mf = split_top_level_args(margs);
                    if let Some(v) = mf.first().copied().and_then(parameter_value) {
                        if v.is_finite() && v > 0.0 {
                            return Some(v as f32);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Directed angular span `[start, end]` from `a1` to `a2`. `sense == true`
/// sweeps CCW (increasing angle), `false` CW; a1 == a2 yields a full turn.
///
/// `None` for non-finite input (GH #160). This used to be a
/// `while e += TAU` walk, which never terminates once `|a1|` exceeds
/// ~1.3e8 (TAU is below the f32 ULP there, so `e` stops advancing) and
/// hangs outright on an infinite or NaN trim angle. The modular form
/// below is O(1) and total.
fn arc_span(a1: f32, a2: f32, sense: bool) -> Option<(f32, f32)> {
    use std::f32::consts::TAU;
    const EPS: f32 = 1e-6;
    if !a1.is_finite() || !a2.is_finite() {
        return None;
    }
    // Distance to walk from a1, in the swept direction, modulo a full
    // turn. A residue at (or within EPS of) zero means the trims
    // coincide → a full revolution, matching the old loop's behaviour.
    let mut delta = if sense {
        (a2 - a1).rem_euclid(TAU)
    } else {
        (a1 - a2).rem_euclid(TAU)
    };
    if !delta.is_finite() {
        return None;
    }
    if delta <= EPS {
        delta = TAU;
    }
    if sense {
        Some((a1, a1 + delta))
    } else {
        Some((a1, a1 - delta))
    }
}

/// Sample an `IfcCircle` into a closed CCW polyline of `CURVE_SAMPLES`
/// points, honouring its `IfcAxis2Placement2D` (centre + orientation).
fn circle_curve_2d(table: &EntityTable, fields: &[&[u8]]) -> Option<Vec<Vec2>> {
    let radius = number_at(fields, 1)? as f32;
    if !(radius.is_finite() && radius > 0.0) {
        return None;
    }
    let (center, ref_dir) = match fields.first().copied().map(parse_field) {
        Some(Field::Ref(pid)) => placement2d_origin_dir(table, pid),
        _ => (Vec2::ZERO, Vec2::X),
    };
    let (cos, sin) = (ref_dir.x, ref_dir.y);
    let pts = (0..CURVE_SAMPLES)
        .map(|i| {
            let a = (i as f32) * (std::f32::consts::TAU / CURVE_SAMPLES as f32);
            let lx = radius * a.cos();
            let ly = radius * a.sin();
            // Rotate the local circle by the placement's ref direction,
            // then translate to the centre.
            Vec2::new(
                center.x + cos * lx - sin * ly,
                center.y + sin * lx + cos * ly,
            )
        })
        .collect();
    Some(pts)
}

/// Extract `(location, ref_direction)` from an `IfcAxis2Placement2D`.
/// Defaults to origin / +X when absent or malformed.
fn placement2d_origin_dir(table: &EntityTable, pid: u64) -> (Vec2, Vec2) {
    let (type_name, args) = match table.get(pid) {
        Some(x) => x,
        None => return (Vec2::ZERO, Vec2::X),
    };
    if !type_name.eq_ignore_ascii_case(b"IFCAXIS2PLACEMENT2D") {
        return (Vec2::ZERO, Vec2::X);
    }
    let pf = split_top_level_args(args);
    let loc = pf
        .first()
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(p) => cartesian_point_2d(table, p),
            _ => None,
        })
        .unwrap_or(Vec2::ZERO);
    let dir = pf
        .get(1)
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(d) => direction_2d(table, d),
            _ => None,
        })
        .unwrap_or(Vec2::X);
    (loc, dir)
}

/// Concatenate the parent curves of an IfcCompositeCurve into a single
/// polyline. Each segment carries a SameSense flag — when false the
/// segment's parent curve is reversed before joining.
fn composite_curve(table: &EntityTable, fields: &[&[u8]]) -> Option<Vec<Vec2>> {
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut out: Vec<Vec2> = Vec::new();
    for seg_field in split_top_level_args(body) {
        // GH #154: EVERY failure below fails the WHOLE profile. The old
        // `continue` skipped the segment and then closed the loop across
        // the gap, producing a shape that is closed, plausible and wrong
        // — and passes the QTO tripwires as `volume_reliable`. Failing
        // here propagates `None` up through `extract` → the handler →
        // `mesh_item`'s `single(None, …)`, which emits an `Unhandled`
        // marker naming the item's IFC type. A visible hole in the
        // output beats a silently-bridged solid.
        let seg_id = match parse_field(seg_field) {
            Field::Ref(id) => id,
            _ => return None,
        };
        let (seg_type, seg_args) = table.get(seg_id)?;
        // IfcReparametrisedCompositeCurveSegment is the one subtype; it
        // adds ParamLength after the three inherited attributes, so the
        // arg positions used below are identical.
        if !seg_type.eq_ignore_ascii_case(b"IFCCOMPOSITECURVESEGMENT")
            && !seg_type.eq_ignore_ascii_case(b"IFCREPARAMETRISEDCOMPOSITECURVESEGMENT")
        {
            return None;
        }
        // IfcCompositeCurveSegment(Transition, SameSense, ParentCurve)
        let seg_fields = split_top_level_args(seg_args);
        let same_sense = matches!(
            seg_fields.get(1).copied().map(parse_field),
            Some(Field::Enum(b"T"))
        );
        let parent_id = match seg_fields.get(2).copied().map(parse_field) {
            Some(Field::Ref(id)) => id,
            _ => return None,
        };
        // `curve_to_polyline` returns `None` for every curve subtype it
        // cannot tessellate (B-spline, IfcOffsetCurve, unsampled conics,
        // …) — the general case GH #123 left open.
        let mut pts = curve_to_polyline(table, parent_id)?;
        if !same_sense {
            pts.reverse();
        }
        // Stitch: if last point of `out` matches first point of `pts`,
        // drop the duplicate before joining.
        if let (Some(last), Some(first)) = (out.last().copied(), pts.first().copied()) {
            if (last - first).length_squared() < 1e-12 {
                pts.remove(0);
            }
        }
        out.extend(pts);
    }
    if out.len() > 2 && out.first() == out.last() {
        out.pop();
    }
    if out.len() < 3 {
        return None;
    }
    Some(out)
}

/// Raw 2D point list from an `IfcCartesianPointList2D` entity — no
/// trailing-close dedup, since callers may need the raw indices
/// (`IfcIndexedPolyCurve` segments are 1-based and any vertex may be
/// referenced).
fn point_list_2d_raw(table: &EntityTable, id: u64) -> Option<Vec<Vec2>> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINTLIST2D") {
        return None;
    }
    let fields = split_top_level_args(args);
    // arg[0] = CoordList — list of 2-element coord lists.
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut pts = Vec::new();
    for sub in split_top_level_args(body) {
        if let Field::List(inner) = parse_field(sub) {
            let coords: Vec<f32> = split_top_level_args(inner)
                .into_iter()
                .filter_map(|f| match parse_field(f) {
                    Field::Number(n) => Some(n as f32),
                    _ => None,
                })
                .collect();
            if coords.len() >= 2 {
                pts.push(Vec2::new(coords[0], coords[1]));
            }
        }
    }
    Some(pts)
}

/// Extract the inner list bytes from an optional `Field::List` field,
/// returning `None` for null/star/missing fields. Used to thread the
/// `IfcIndexedPolyCurve.Segments` raw bytes into the segment evaluator.
fn list_body(field: Option<&[u8]>) -> Option<&[u8]> {
    match parse_field(field?) {
        Field::List(b) => Some(b),
        _ => None,
    }
}

fn cartesian_point_2d(table: &EntityTable, id: u64) -> Option<Vec2> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINT") {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let coords: Vec<f32> = split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Number(n) => Some(n as f32),
            _ => None,
        })
        .collect();
    let x = *coords.first().unwrap_or(&0.0);
    let y = *coords.get(1).unwrap_or(&0.0);
    Some(Vec2::new(x, y))
}

// ----------------------------------------------------------------------
// Profile-local Position transform (2D)
// ----------------------------------------------------------------------

fn apply_profile_position(table: &EntityTable, fields: &[&[u8]], poly: Polygon2D) -> Polygon2D {
    // Parametric profiles put Position at arg[2].
    let pos_id = match fields.get(2).copied().map(parse_field) {
        Some(Field::Ref(id)) => id,
        _ => return poly,
    };
    let (type_name, args) = match table.get(pos_id) {
        Some(x) => x,
        None => return poly,
    };
    if !type_name.eq_ignore_ascii_case(b"IFCAXIS2PLACEMENT2D") {
        return poly;
    }
    let pf = split_top_level_args(args);
    // arg[0] = Location (IfcCartesianPoint 2D)
    // arg[1] = RefDirection (optional IfcDirection 2D, default (1,0))
    let loc = pf
        .first()
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(pid) => cartesian_point_2d(table, pid),
            _ => None,
        })
        .unwrap_or(Vec2::ZERO);
    let ref_dir = pf
        .get(1)
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(did) => direction_2d(table, did),
            _ => None,
        })
        .unwrap_or(Vec2::X);
    let cos = ref_dir.x;
    let sin = ref_dir.y;
    let rot = Mat3::from_cols(
        Vec3::new(cos, sin, 0.0),
        Vec3::new(-sin, cos, 0.0),
        Vec3::new(loc.x, loc.y, 1.0),
    );
    let map = |p: Vec2| -> Vec2 {
        let r = rot * Vec3::new(p.x, p.y, 1.0);
        Vec2::new(r.x, r.y)
    };
    Polygon2D {
        outer: poly.outer.into_iter().map(map).collect(),
        holes: poly
            .holes
            .into_iter()
            .map(|h| h.into_iter().map(map).collect())
            .collect(),
    }
}

fn direction_2d(table: &EntityTable, id: u64) -> Option<Vec2> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCDIRECTION") {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let ratios: Vec<f32> = split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Number(n) => Some(n as f32),
            _ => None,
        })
        .collect();
    let x = *ratios.first().unwrap_or(&1.0);
    let y = *ratios.get(1).unwrap_or(&0.0);
    Some(Vec2::new(x, y).normalize_or_zero())
}

/// Resolve an `IfcVector(Orientation: IfcDirection, Magnitude)` to a
/// normalized 2D direction and its magnitude. Used by the `IfcLine` basis of
/// [`trimmed_line_2d`].
fn vector_2d(table: &EntityTable, id: u64) -> Option<(Vec2, f32)> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCVECTOR") {
        return None;
    }
    let fields = split_top_level_args(args);
    let dir = match fields.first().copied().map(parse_field) {
        Some(Field::Ref(did)) => direction_2d(table, did)?,
        _ => return None,
    };
    let mag = number_at(&fields, 1)? as f32;
    if !(mag.is_finite() && mag > 0.0) {
        return None;
    }
    // direction_2d already normalizes; a zero direction collapses to zero and
    // would make the line degenerate — reject it.
    if dir.length_squared() < 1e-12 {
        return None;
    }
    Some((dir, mag))
}

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn sample_ellipse(a: f32, b: f32, n: usize) -> Vec<Vec2> {
    (0..n)
        .map(|i| {
            let t = (i as f32) * (std::f32::consts::TAU / n as f32);
            Vec2::new(a * t.cos(), b * t.sin())
        })
        .collect()
}

fn number_at(fields: &[&[u8]], idx: usize) -> Option<f64> {
    match parse_field(fields.get(idx)?) {
        Field::Number(n) => Some(n),
        _ => None,
    }
}

fn ref_at(fields: &[&[u8]], idx: usize) -> Option<u64> {
    match parse_field(fields.get(idx)?) {
        Field::Ref(id) => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod curve_tests {
    use super::*;

    /// Two composite curves over the same points: `#30` contains a
    /// segment whose parent is a B-spline (`curve_to_polyline` cannot
    /// tessellate it), `#31` is all polylines.
    const COMPOSITE_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('cc.ifc','2026-09-06T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCCARTESIANPOINT((0.,0.));
#2=IFCCARTESIANPOINT((10.,0.));
#3=IFCCARTESIANPOINT((10.,10.));
#4=IFCCARTESIANPOINT((0.,10.));
#10=IFCPOLYLINE((#1,#2));
#11=IFCBSPLINECURVEWITHKNOTS(3,(#2,#3,#4,#1),.UNSPECIFIED.,.F.,.F.,(4,4),(0.,1.),.UNSPECIFIED.);
#12=IFCPOLYLINE((#2,#3,#4));
#20=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#10);
#21=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#11);
#22=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#12);
#30=IFCCOMPOSITECURVE((#20,#21),.F.);
#31=IFCCOMPOSITECURVE((#20,#22),.F.);
ENDSEC;
END-ISO-10303-21;
"#;

    /// GH #154: a segment the kernel cannot tessellate fails the WHOLE
    /// profile. The old `continue` closed the loop across the gap and
    /// produced a plausible — and wrong — closed shape that passed the
    /// QTO reliability tripwires.
    #[test]
    fn untessellatable_segment_fails_the_whole_composite_curve() {
        let table = EntityTable::build(COMPOSITE_IFC.as_bytes());
        assert!(
            curve_to_polyline(&table, 30).is_none(),
            "a B-spline segment must fail the composite curve, not be bridged"
        );
    }

    /// The all-polyline composite still resolves — the fail-loud change
    /// does not reject valid input.
    #[test]
    fn all_polyline_composite_curve_still_resolves() {
        let table = EntityTable::build(COMPOSITE_IFC.as_bytes());
        let pts = curve_to_polyline(&table, 31).expect("polyline composite resolves");
        assert_eq!(pts.len(), 4, "got {pts:?}");
    }

    /// GH #160: `arc_span` is modular and total.
    #[test]
    fn arc_span_is_modular_and_total() {
        use std::f32::consts::{PI, TAU};
        let (s, e) = arc_span(0.0, PI, true).unwrap();
        assert!((s - 0.0).abs() < 1e-5 && (e - PI).abs() < 1e-5);
        // Coincident trims → a full turn, in both senses.
        let (_, e) = arc_span(1.0, 1.0, true).unwrap();
        assert!((e - (1.0 + TAU)).abs() < 1e-4);
        let (_, e) = arc_span(1.0, 1.0, false).unwrap();
        assert!((e - (1.0 - TAU)).abs() < 1e-4);
        // CCW to a *smaller* angle wraps forward by a full turn.
        let (_, e) = arc_span(0.0, -PI, true).unwrap();
        assert!((e - PI).abs() < 1e-4, "got {e}");
        // CW to a *larger* angle wraps backward.
        let (_, e) = arc_span(0.0, PI, false).unwrap();
        assert!((e + PI).abs() < 1e-4, "got {e}");
        // Non-finite input is rejected, never looped on.
        assert!(arc_span(f32::INFINITY, 0.0, true).is_none());
        assert!(arc_span(0.0, f32::NAN, true).is_none());
        // A huge angle terminates (the old `while e += TAU` never did:
        // TAU is below the f32 ULP at this magnitude).
        assert!(arc_span(2.0e8, 2.0e8, true).is_some());
    }
}
