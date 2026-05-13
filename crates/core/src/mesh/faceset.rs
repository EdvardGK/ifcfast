//! `IfcPolygonalFaceSet` + `IfcTriangulatedFaceSet` → triangle mesh.
//!
//! Easiest of all the geometry types: vertices already exist as a flat
//! `IfcCartesianPointList3D`, faces are 1-based index lists into them.
//! For polygons with >3 vertices we fan-triangulate (Archicad and Revit
//! both emit convex faces almost exclusively; non-convex faces would
//! need earcutr with a projection to 2D, which we'll add if it becomes
//! a problem).

use glam::Vec3;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;

/// Mesh an `IfcPolygonalFaceSet` (Archicad's primary export format).
pub fn polygonal_face_set(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCPOLYGONALFACESET") {
        return None;
    }
    let fields = split_top_level_args(args);
    // IfcPolygonalFaceSet(Coordinates, Closed, Faces, PnIndex)
    let coords_id = match parse_field(*fields.first()?) {
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
    let faces_body = match parse_field(*fields.get(2)?) {
        Field::List(b) => b,
        _ => return None,
    };

    let mut mesh = LocalMesh::new();
    // Push all coords as vertices once.
    for p in &coords {
        mesh.vertices.push(p.x);
        mesh.vertices.push(p.y);
        mesh.vertices.push(p.z);
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
    let coords_id = match parse_field(*fields.first()?) {
        Field::Ref(id) => id,
        _ => return None,
    };
    let coords = cartesian_point_list_3d(table, coords_id)?;
    if coords.is_empty() {
        return None;
    }

    let coord_index_body = match parse_field(*fields.get(3)?) {
        Field::List(b) => b,
        _ => return None,
    };

    let mut mesh = LocalMesh::new();
    for p in &coords {
        mesh.vertices.push(p.x);
        mesh.vertices.push(p.y);
        mesh.vertices.push(p.z);
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

fn cartesian_point_list_3d(table: &EntityTable, id: u64) -> Option<Vec<Vec3>> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINTLIST3D") {
        return None;
    }
    let fields = split_top_level_args(args);
    // arg[0] = CoordList — list of (x, y, z) triples.
    let body = match parse_field(*fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut pts: Vec<Vec3> = Vec::new();
    for sub in split_top_level_args(body) {
        let inner = match parse_field(sub) {
            Field::List(b) => b,
            _ => continue,
        };
        let coords: Vec<f32> = split_top_level_args(inner)
            .into_iter()
            .filter_map(|f| match parse_field(f) {
                Field::Number(n) => Some(n as f32),
                _ => None,
            })
            .collect();
        if coords.len() >= 3 {
            pts.push(Vec3::new(coords[0], coords[1], coords[2]));
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
    let body = match parse_field(*fields.first()?) {
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
