//! `IfcGeometricCurveSet` / `IfcGeometricSet` → wireframe-as-degenerate-triangles.
//!
//! `IfcGeometricCurveSet` is a subtype of `IfcGeometricSet` whose
//! `Elements` are curves (and points), commonly used for axis grids,
//! dimension witness lines, room-boundary polylines, and similar
//! non-solid annotation geometry. It has no surface and no volume —
//! only line segments and discrete points in 3D space.
//!
//! Per the reveal-all stance we surface this geometry instead of
//! dropping it. Until a first-class `MeshFragment::Wireframe` lands,
//! every line segment is emitted as a *degenerate* triangle
//! `(a, b, b)` with zero area. Downstream consumers see:
//!   - Correct vertex positions (the polyline is preserved);
//!   - Zero `surface_area` and zero `volume` contributions (correct);
//!   - One triangle per segment (slight count inflation, but the
//!     fragment is tagged `curve_set` so it filters cleanly).
//!
//! A future commit should promote this to a real wireframe fragment
//! with OBJ `l` directives and a glTF `LINES` primitive. Filed as a
//! follow-up — the present handler closes the reveal-all gap on
//! real Norwegian Revit/Magicad ARK + RIB output, where this is the
//! one type that still bucketed as `unhandled:*`.

use glam::Vec3;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;

/// Build a wireframe mesh for an `IfcGeometricCurveSet` (or the parent
/// `IfcGeometricSet`). Each curve element contributes a chain of
/// degenerate triangles tracing its polyline; standalone points add a
/// lone vertex.
pub fn geometric_curve_set(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCGEOMETRICCURVESET")
        && !type_name.eq_ignore_ascii_case(b"IFCGEOMETRICSET")
    {
        return None;
    }

    let fields = split_top_level_args(args);
    let body = match parse_field(*fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };

    let mut mesh = LocalMesh::new();
    for f in split_top_level_args(body) {
        if let Field::Ref(eid) = parse_field(f) {
            append_curve_element(table, eid, &mut mesh);
        }
    }

    if mesh.vertices.is_empty() {
        None
    } else {
        Some(mesh)
    }
}

fn append_curve_element(table: &EntityTable, id: u64, mesh: &mut LocalMesh) {
    let Some((type_name, _)) = table.get(id) else {
        return;
    };

    if type_name.eq_ignore_ascii_case(b"IFCPOLYLINE") {
        if let Some(pts) = polyline_3d_points(table, id) {
            append_polyline(mesh, &pts);
        }
        return;
    }
    if type_name.eq_ignore_ascii_case(b"IFCINDEXEDPOLYCURVE") {
        if let Some(pts) = indexed_poly_curve_3d_points(table, id) {
            append_polyline(mesh, &pts);
        }
        return;
    }
    if type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINT") {
        if let Some(p) = cartesian_point_3d(table, id) {
            mesh.vertices.push(p.x);
            mesh.vertices.push(p.y);
            mesh.vertices.push(p.z);
        }
    }
    // Other curve subtypes (IfcTrimmedCurve, IfcBSplineCurve,
    // IfcCompositeCurve, ...) are left as silent drops *within* the
    // curve set. The outer set still buckets as `curve_set`; these
    // specific sub-elements are a follow-up gap.
}

fn append_polyline(mesh: &mut LocalMesh, pts: &[Vec3]) {
    if pts.len() < 2 {
        if let Some(p) = pts.first() {
            mesh.vertices.push(p.x);
            mesh.vertices.push(p.y);
            mesh.vertices.push(p.z);
        }
        return;
    }
    let start = (mesh.vertices.len() / 3) as u32;
    for p in pts {
        mesh.vertices.push(p.x);
        mesh.vertices.push(p.y);
        mesh.vertices.push(p.z);
    }
    for i in 0..(pts.len() - 1) {
        let a = start + i as u32;
        let b = start + (i + 1) as u32;
        mesh.indices.push(a);
        mesh.indices.push(b);
        mesh.indices.push(b);
    }
}

fn polyline_3d_points(table: &EntityTable, id: u64) -> Option<Vec<Vec3>> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCPOLYLINE") {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(*fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut out = Vec::new();
    for f in split_top_level_args(body) {
        if let Field::Ref(pid) = parse_field(f) {
            if let Some(p) = cartesian_point_3d(table, pid) {
                out.push(p);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn indexed_poly_curve_3d_points(table: &EntityTable, id: u64) -> Option<Vec<Vec3>> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCINDEXEDPOLYCURVE") {
        return None;
    }
    let fields = split_top_level_args(args);
    let pts_id = match parse_field(*fields.first()?) {
        Field::Ref(pid) => pid,
        _ => return None,
    };
    cartesian_point_list_3d(table, pts_id)
}

fn cartesian_point_3d(table: &EntityTable, id: u64) -> Option<Vec3> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINT") {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(*fields.first()?) {
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
    Some(Vec3::new(
        *coords.first().unwrap_or(&0.0),
        *coords.get(1).unwrap_or(&0.0),
        *coords.get(2).unwrap_or(&0.0),
    ))
}

fn cartesian_point_list_3d(table: &EntityTable, id: u64) -> Option<Vec<Vec3>> {
    let (type_name, args) = table.get(id)?;
    let is_3d = type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINTLIST3D");
    let is_2d = type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINTLIST2D");
    if !is_3d && !is_2d {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(*fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut out = Vec::new();
    for f in split_top_level_args(body) {
        if let Field::List(inner) = parse_field(f) {
            let coords: Vec<f32> = split_top_level_args(inner)
                .into_iter()
                .filter_map(|g| match parse_field(g) {
                    Field::Number(n) => Some(n as f32),
                    _ => None,
                })
                .collect();
            if coords.len() >= 2 {
                out.push(Vec3::new(
                    coords[0],
                    coords[1],
                    *coords.get(2).unwrap_or(&0.0),
                ));
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}
