//! `IfcPolygonalFaceSet` + `IfcTriangulatedFaceSet` → triangle mesh.
//!
//! Easiest of all the geometry types: vertices already exist as a flat
//! `IfcCartesianPointList3D`, faces are 1-based index lists into them.
//! For polygons with >3 vertices we fan-triangulate (Archicad and Revit
//! both emit convex faces almost exclusively; non-convex faces would
//! need earcutr with a projection to 2D, which we'll add if it becomes
//! a problem).

use glam::DVec3;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;

/// Compute the f64 bbox-min of a CartesianPointList. The kernel
/// subtracts this from every vertex before downcasting to f32 so that a
/// representation whose coords have huge world values baked into them
/// (transformed/georeferenced MEP) still meshes precisely. The offset
/// rides on `LocalMesh.rep_origin` and is re-applied through an f64
/// anchor by the bake loop. For typical authoring (small local coords)
/// this is `[0, 0, 0]` and behaviour is unchanged.
fn bbox_min(pts: &[DVec3]) -> DVec3 {
    pts.iter()
        .copied()
        .fold(DVec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY), |a, p| a.min(p))
}

/// Mesh an `IfcPolygonalFaceSet` (Archicad's primary export format).
pub fn polygonal_face_set(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCPOLYGONALFACESET") {
        return None;
    }
    let fields = split_top_level_args(args);
    // IfcPolygonalFaceSet(Coordinates, Closed, Faces, PnIndex)
    let coords_id = match parse_field(fields.first()?) {
        Field::Ref(id) => id,
        _ => return None,
    };
    let coords = cartesian_point_list_3d(table, coords_id)?;
    if coords.is_empty() {
        return None;
    }

    // PnIndex is an optional 1-based remap layer for the coord indices.
    let pn_index: Option<Vec<u32>> = fields.get(3).copied().and_then(|f| match parse_field(f) {
        Field::List(body) => Some(
            split_top_level_args(body)
                .into_iter()
                .filter_map(|x| match parse_field(x) {
                    Field::Number(n) => Some(n as u32),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    });

    // Faces list.
    let faces_body = match parse_field(fields.get(2)?) {
        Field::List(b) => b,
        _ => return None,
    };

    let mut mesh = LocalMesh::new();
    // Rebase by bbox-min so the f32 vertex buffer stays near origin
    // even when the file embeds huge world coords directly into the
    // CartesianPointList. The bake loop re-applies `rep_origin` via an
    // f64 anchor.
    let origin = bbox_min(&coords);
    mesh.rep_origin = [origin.x, origin.y, origin.z];
    for p in &coords {
        let d = *p - origin;
        mesh.vertices.push(d.x as f32);
        mesh.vertices.push(d.y as f32);
        mesh.vertices.push(d.z as f32);
    }

    let face_refs = split_top_level_args(faces_body);
    for face_field in face_refs {
        let face_id = match parse_field(face_field) {
            Field::Ref(id) => id,
            _ => continue,
        };
        let indices = match indexed_polygonal_face(table, face_id) {
            Some(v) => v,
            None => continue,
        };
        // Remap via PnIndex if present (both are 1-based; the IFC spec
        // says PnIndex maps face-local 1-based indices to coord-list
        // 1-based indices).
        let mapped: Vec<u32> = if let Some(pn) = &pn_index {
            indices
                .iter()
                .filter_map(|&i| pn.get((i as usize).saturating_sub(1)).copied())
                .map(|v| v.saturating_sub(1))
                .collect()
        } else {
            indices.iter().map(|&i| i.saturating_sub(1)).collect()
        };
        // Fan-triangulate.
        if mapped.len() < 3 {
            continue;
        }
        for i in 1..(mapped.len() - 1) {
            // Validate indices fit the coords table.
            let a = mapped[0];
            let b = mapped[i];
            let c = mapped[i + 1];
            if (a as usize) >= coords.len()
                || (b as usize) >= coords.len()
                || (c as usize) >= coords.len()
            {
                continue;
            }
            mesh.indices.push(a);
            mesh.indices.push(b);
            mesh.indices.push(c);
        }
    }

    if mesh.indices.is_empty() {
        return None;
    }
    Some(mesh)
}

/// Mesh an `IfcTriangulatedFaceSet` (already triangulated).
pub fn triangulated_face_set(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCTRIANGULATEDFACESET") {
        return None;
    }
    let fields = split_top_level_args(args);
    // IfcTriangulatedFaceSet(Coordinates, Normals, Closed, CoordIndex, PnIndex)
    let coords_id = match parse_field(fields.first()?) {
        Field::Ref(id) => id,
        _ => return None,
    };
    let coords = cartesian_point_list_3d(table, coords_id)?;
    if coords.is_empty() {
        return None;
    }

    let coord_index_body = match parse_field(fields.get(3)?) {
        Field::List(b) => b,
        _ => return None,
    };

    let mut mesh = LocalMesh::new();
    // Same f64 bbox-min rebase as polygonal_face_set — see comment there.
    let origin = bbox_min(&coords);
    mesh.rep_origin = [origin.x, origin.y, origin.z];
    for p in &coords {
        let d = *p - origin;
        mesh.vertices.push(d.x as f32);
        mesh.vertices.push(d.y as f32);
        mesh.vertices.push(d.z as f32);
    }

    // CoordIndex is a list of [i, j, k] triples (1-based).
    for tri_field in split_top_level_args(coord_index_body) {
        let body = match parse_field(tri_field) {
            Field::List(b) => b,
            _ => continue,
        };
        let idx: Vec<u32> = split_top_level_args(body)
            .into_iter()
            .filter_map(|f| match parse_field(f) {
                Field::Number(n) => Some(n as u32),
                _ => None,
            })
            .collect();
        if idx.len() < 3 {
            continue;
        }
        let a = idx[0].saturating_sub(1);
        let b = idx[1].saturating_sub(1);
        let c = idx[2].saturating_sub(1);
        if (a as usize) >= coords.len()
            || (b as usize) >= coords.len()
            || (c as usize) >= coords.len()
        {
            continue;
        }
        mesh.indices.push(a);
        mesh.indices.push(b);
        mesh.indices.push(c);
    }

    if mesh.indices.is_empty() {
        return None;
    }
    Some(mesh)
}

fn cartesian_point_list_3d(table: &EntityTable, id: u64) -> Option<Vec<DVec3>> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINTLIST3D") {
        return None;
    }
    let fields = split_top_level_args(args);
    // arg[0] = CoordList — list of (x, y, z) triples. Parsed in f64 so
    // a representation whose coords have huge world values baked into
    // them (transformed/georeferenced MEP) doesn't collapse here, before
    // bbox-min rebase can rescue it.
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut pts: Vec<DVec3> = Vec::new();
    for sub in split_top_level_args(body) {
        let inner = match parse_field(sub) {
            Field::List(b) => b,
            _ => continue,
        };
        let coords: Vec<f64> = split_top_level_args(inner)
            .into_iter()
            .filter_map(|f| match parse_field(f) {
                Field::Number(n) => Some(n),
                _ => None,
            })
            .collect();
        if coords.len() >= 3 {
            pts.push(DVec3::new(coords[0], coords[1], coords[2]));
        }
    }
    Some(pts)
}

fn indexed_polygonal_face(table: &EntityTable, id: u64) -> Option<Vec<u32>> {
    let (type_name, args) = table.get(id)?;
    // IfcIndexedPolygonalFace OR IfcIndexedPolygonalFaceWithVoids
    if !type_name.eq_ignore_ascii_case(b"IFCINDEXEDPOLYGONALFACE")
        && !type_name.eq_ignore_ascii_case(b"IFCINDEXEDPOLYGONALFACEWITHVOIDS")
    {
        return None;
    }
    let fields = split_top_level_args(args);
    // arg[0] = CoordIndex
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    Some(
        split_top_level_args(body)
            .into_iter()
            .filter_map(|f| match parse_field(f) {
                Field::Number(n) => Some(n as u32),
                _ => None,
            })
            .collect(),
    )
}
