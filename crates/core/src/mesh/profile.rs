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
use crate::lexer::{parse_field, split_top_level_args, Field};

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
    Some(positioned)
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
        _ => return Some(Polygon2D { outer, holes: vec![] }),
    };
    let mut holes: Vec<Vec<Vec2>> = Vec::new();
    for hole_field in split_top_level_args(body) {
        if let Field::Ref(hid) = parse_field(hole_field) {
            if let Some(hole) = curve_to_polyline(table, hid) {
                let mut h = hole;
                h.reverse(); // CW for inner
                holes.push(h);
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
        return point_list_2d(table, pts_id);
    }
    // IfcCompositeCurve, IfcTrimmedCurve etc. — skip for Phase 1A.
    None
}

fn point_list_2d(table: &EntityTable, id: u64) -> Option<Vec<Vec2>> {
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
    if pts.len() > 2 && pts.first() == pts.last() {
        pts.pop();
    }
    Some(pts)
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

fn apply_profile_position(
    table: &EntityTable,
    fields: &[&[u8]],
    poly: Polygon2D,
) -> Polygon2D {
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
    match parse_field(*fields.get(idx)?) {
        Field::Number(n) => Some(n),
        _ => None,
    }
}

fn ref_at(fields: &[&[u8]], idx: usize) -> Option<u64> {
    match parse_field(*fields.get(idx)?) {
        Field::Ref(id) => Some(id),
        _ => None,
    }
}
