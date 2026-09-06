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
    pts.iter().copied().fold(
        DVec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY),
        |a, p| a.min(p),
    )
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
        let (indices, inner_loops) = match indexed_polygonal_face(table, face_id) {
            Some(v) => v,
            None => continue,
        };
        // Remap via PnIndex if present (both are 1-based; the IFC spec
        // says PnIndex maps face-local 1-based indices to coord-list
        // 1-based indices).
        let remap = |raw: &[u32]| -> Vec<u32> {
            if let Some(pn) = &pn_index {
                raw.iter()
                    .filter_map(|&i| pn.get((i as usize).saturating_sub(1)).copied())
                    .map(|v| v.saturating_sub(1))
                    .collect()
            } else {
                raw.iter().map(|&i| i.saturating_sub(1)).collect()
            }
        };
        let mapped: Vec<u32> = remap(&indices[..]);
        if mapped.len() < 3 {
            continue;
        }
        let holes: Vec<Vec<u32>> = inner_loops
            .iter()
            .map(|l| remap(&l[..]))
            .filter(|l| l.len() >= 3)
            .collect();

        // GH #160: a face with declared voids is ear-clipped WITH its
        // holes (same projection + earcut path the brep face walker
        // uses) instead of fan-filled. Fan-filling a WithVoids face
        // over-reports its area and, through the closed shell, its
        // volume — silently, with the result still classified
        // `volume_reliable`.
        if !holes.is_empty() {
            let in_range = |l: &[u32]| l.iter().all(|&i| (i as usize) < coords.len());
            if in_range(&mapped[..])
                && holes.iter().all(|h| in_range(&h[..]))
                && crate::mesh::brep::triangulate_face_with_holes(&mut mesh, &mapped, &holes)
            {
                continue;
            }
            // Projection failed (degenerate face) or an index is out of
            // range — fall through to the fan below so the face is at
            // least present rather than dropped.
        }

        // Fan-triangulate the outer loop.
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

/// One face's index loops: the outer `CoordIndex` plus, for
/// `IfcIndexedPolygonalFaceWithVoids`, every `InnerCoordIndices` loop
/// (GH #160). Reading only the outer loop filled the voids — an
/// Archicad polygonal faceset's penetrations came back solid, with no
/// flag on the over-reported volume.
fn indexed_polygonal_face(table: &EntityTable, id: u64) -> Option<(Vec<u32>, Vec<Vec<u32>>)> {
    let (type_name, args) = table.get(id)?;
    // IfcIndexedPolygonalFace OR IfcIndexedPolygonalFaceWithVoids
    let with_voids = type_name.eq_ignore_ascii_case(b"IFCINDEXEDPOLYGONALFACEWITHVOIDS");
    if !type_name.eq_ignore_ascii_case(b"IFCINDEXEDPOLYGONALFACE") && !with_voids {
        return None;
    }
    let fields = split_top_level_args(args);
    // arg[0] = CoordIndex, arg[1] = InnerCoordIndices (WithVoids only)
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let outer: Vec<u32> = index_list(body);
    let mut inner: Vec<Vec<u32>> = Vec::new();
    if with_voids {
        if let Some(Field::List(loops)) = fields.get(1).copied().map(parse_field) {
            for lf in split_top_level_args(loops) {
                if let Field::List(one) = parse_field(lf) {
                    let l = index_list(one);
                    if l.len() >= 3 {
                        inner.push(l);
                    }
                }
            }
        }
    }
    Some((outer, inner))
}

/// Flat list of 1-based indices from a STEP list body.
fn index_list(body: &[u8]) -> Vec<u32> {
    split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Number(n) => Some(n as u32),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single 10×10 face with a centred 4×4 void, as
    /// `IfcIndexedPolygonalFaceWithVoids`. Archicad's shape for a
    /// penetration in a polygonal faceset.
    const FACE_WITH_VOID_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('void.ifc','2026-09-06T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(10.,0.,0.),(10.,10.,0.),(0.,10.,0.),(3.,3.,0.),(3.,7.,0.),(7.,7.,0.),(7.,3.,0.)));
#2=IFCINDEXEDPOLYGONALFACEWITHVOIDS((1,2,3,4),((5,6,7,8)));
#3=IFCPOLYGONALFACESET(#1,.F.,(#2),$);
#4=IFCINDEXEDPOLYGONALFACE((1,2,3,4));
#5=IFCPOLYGONALFACESET(#1,.F.,(#4),$);
ENDSEC;
END-ISO-10303-21;
"#;

    fn tri_area(mesh: &LocalMesh) -> f32 {
        let v = |i: u32| -> glam::Vec3 {
            let b = i as usize * 3;
            glam::Vec3::new(mesh.vertices[b], mesh.vertices[b + 1], mesh.vertices[b + 2])
        };
        mesh.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| 0.5 * (v(t[1]) - v(t[0])).cross(v(t[2]) - v(t[0])).length())
            .sum()
    }

    /// GH #160: `InnerCoordIndices` must be honoured. A 10×10 face with
    /// a 4×4 void has area 84 — a fan over the outer loop alone gives
    /// the 100 the pre-fix path reported, silently over-filling the
    /// penetration.
    #[test]
    fn polygonal_face_with_voids_excludes_hole_area() {
        let table = EntityTable::build(FACE_WITH_VOID_IFC.as_bytes());
        let mesh = polygonal_face_set(&table, 3).expect("faceset #3 meshes");
        let area = tri_area(&mesh);
        assert!(
            (area - 84.0).abs() < 1e-3,
            "expected the void excluded (area 84), got {area}"
        );
    }

    /// The plain `IfcIndexedPolygonalFace` path is untouched — still the
    /// cheap fan, still the full 100.
    #[test]
    fn polygonal_face_without_voids_is_unchanged() {
        let table = EntityTable::build(FACE_WITH_VOID_IFC.as_bytes());
        let mesh = polygonal_face_set(&table, 5).expect("faceset #5 meshes");
        assert!((tri_area(&mesh) - 100.0).abs() < 1e-3);
    }
}
