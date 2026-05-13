//! `IfcFacetedBrep` / `IfcManifoldSolidBrep` → triangle mesh.
//!
//! Traversal: brep → `Outer` (`IfcClosedShell`) → `CfsFaces` (list of
//! `IfcFace`) → `Bounds` (list of `IfcFaceBound` / `IfcFaceOuterBound`)
//! → `Bound` (`IfcPolyLoop`) → `Polygon` (list of `IfcCartesianPoint`).
//!
//! Vertex deduplication: a single `IfcCartesianPoint` is typically
//! referenced by many faces. We cache step_id → vertex_index in the
//! output mesh so each unique point becomes one vertex.

use std::collections::HashMap;

use glam::Vec3;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;

/// Mesh an `IfcFacetedBrep` or `IfcManifoldSolidBrep`.
pub fn faceted_brep(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCFACETEDBREP")
        && !type_name.eq_ignore_ascii_case(b"IFCMANIFOLDSOLIDBREP")
    {
        return None;
    }
    let fields = split_top_level_args(args);
    // (Outer: IfcClosedShell)
    let outer_id = match parse_field(*fields.first()?) {
        Field::Ref(id) => id,
        _ => return None,
    };
    closed_shell(table, outer_id)
}

/// Mesh an `IfcClosedShell` (walked directly, not via a Brep wrapper).
pub fn closed_shell(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCLOSEDSHELL")
        && !type_name.eq_ignore_ascii_case(b"IFCOPENSHELL")
    {
        return None;
    }
    let fields = split_top_level_args(args);
    // (CfsFaces: LIST OF IfcFace)
    let body = match parse_field(*fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };

    let mut mesh = LocalMesh::new();
    // Cache: cartesian-point step_id → index in mesh.vertices
    let mut vertex_cache: HashMap<u64, u32> = HashMap::with_capacity(4096);

    for face_field in split_top_level_args(body) {
        let face_id = match parse_field(face_field) {
            Field::Ref(id) => id,
            _ => continue,
        };
        mesh_face(table, face_id, &mut mesh, &mut vertex_cache);
    }

    if mesh.indices.is_empty() {
        return None;
    }
    Some(mesh)
}

fn mesh_face(
    table: &EntityTable,
    face_id: u64,
    mesh: &mut LocalMesh,
    vertex_cache: &mut HashMap<u64, u32>,
) {
    let (type_name, args) = match table.get(face_id) {
        Some(x) => x,
        None => return,
    };
    if !type_name.eq_ignore_ascii_case(b"IFCFACE")
        && !type_name.eq_ignore_ascii_case(b"IFCFACESURFACE")
        && !type_name.eq_ignore_ascii_case(b"IFCADVANCEDFACE")
    {
        return;
    }
    let fields = split_top_level_args(args);
    // (Bounds: LIST OF IfcFaceBound)
    let body = match parse_field(*fields.first().unwrap_or(&&[][..])) {
        Field::List(b) => b,
        _ => return,
    };

    // For Phase 1B: take the outer bound (IfcFaceOuterBound preferred,
    // else the first bound). Holes from non-outer bounds are dropped —
    // the face will be over-filled, visually wrong on those rare cases
    // but topologically valid. Earcutr-with-3D-projection comes in 1C.
    let mut outer_loop: Option<u64> = None;
    let mut outer_orientation = true;
    let mut fallback: Option<(u64, bool)> = None;
    for bound_field in split_top_level_args(body) {
        let bound_id = match parse_field(bound_field) {
            Field::Ref(id) => id,
            _ => continue,
        };
        let (b_type, b_args) = match table.get(bound_id) {
            Some(x) => x,
            None => continue,
        };
        if !b_type.eq_ignore_ascii_case(b"IFCFACEBOUND")
            && !b_type.eq_ignore_ascii_case(b"IFCFACEOUTERBOUND")
        {
            continue;
        }
        let bf = split_top_level_args(b_args);
        // (Bound: IfcLoop, Orientation: BOOL)
        let loop_id = match parse_field(*bf.first().unwrap_or(&&[][..])) {
            Field::Ref(id) => id,
            _ => continue,
        };
        let orient = match parse_field(*bf.get(1).unwrap_or(&&[][..])) {
            // STEP booleans: `.T.` = true, `.F.` = false (enum form)
            Field::Enum(e) => e == b"T",
            _ => true,
        };
        if b_type.eq_ignore_ascii_case(b"IFCFACEOUTERBOUND") {
            outer_loop = Some(loop_id);
            outer_orientation = orient;
            break;
        }
        if fallback.is_none() {
            fallback = Some((loop_id, orient));
        }
    }
    let (loop_id, orient) = match outer_loop {
        Some(id) => (id, outer_orientation),
        None => match fallback {
            Some(x) => x,
            None => return,
        },
    };

    // Walk the loop's polygon.
    let mut verts: Vec<u32> = poly_loop_vertices(table, loop_id, mesh, vertex_cache);
    if verts.len() < 3 {
        return;
    }
    if !orient {
        verts.reverse();
    }

    // Fan triangulate.
    for i in 1..(verts.len() - 1) {
        mesh.indices.push(verts[0]);
        mesh.indices.push(verts[i]);
        mesh.indices.push(verts[i + 1]);
    }
}

fn poly_loop_vertices(
    table: &EntityTable,
    loop_id: u64,
    mesh: &mut LocalMesh,
    vertex_cache: &mut HashMap<u64, u32>,
) -> Vec<u32> {
    let (type_name, args) = match table.get(loop_id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    if !type_name.eq_ignore_ascii_case(b"IFCPOLYLOOP") {
        // IfcEdgeLoop etc. — Phase 1C.
        return Vec::new();
    }
    let fields = split_top_level_args(args);
    // (Polygon: LIST OF IfcCartesianPoint)
    let body = match parse_field(*fields.first().unwrap_or(&&[][..])) {
        Field::List(b) => b,
        _ => return Vec::new(),
    };
    let mut out: Vec<u32> = Vec::new();
    for pt_field in split_top_level_args(body) {
        let pt_id = match parse_field(pt_field) {
            Field::Ref(id) => id,
            _ => continue,
        };
        if let Some(&idx) = vertex_cache.get(&pt_id) {
            out.push(idx);
            continue;
        }
        let p = match cartesian_point(table, pt_id) {
            Some(p) => p,
            None => continue,
        };
        let idx = (mesh.vertices.len() / 3) as u32;
        mesh.vertices.push(p.x);
        mesh.vertices.push(p.y);
        mesh.vertices.push(p.z);
        vertex_cache.insert(pt_id, idx);
        out.push(idx);
    }
    out
}

fn cartesian_point(table: &EntityTable, id: u64) -> Option<Vec3> {
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
