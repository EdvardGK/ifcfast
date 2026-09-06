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

use glam::{DVec3, Vec3};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;

/// Mesh an `IfcFacetedBrep` / `IfcManifoldSolidBrep` / `IfcAdvancedBrep`.
///
/// All three share the same first attribute (`Outer: IfcClosedShell`)
/// and the underlying face / loop / point traversal. `IfcAdvancedBrep`
/// uses curved surfaces (`IfcAdvancedFace` + `IfcBSplineSurface` etc.)
/// for its faces — at this stage we tessellate by treating the face's
/// outer poly-loop as-is, which is a planar approximation. The fragment
/// caller tags the source as `"advanced_brep_approx"` so the consumer
/// knows curvature was discarded; real curved-surface tessellation lives
/// in a future pass.
pub fn faceted_brep(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCFACETEDBREP")
        && !type_name.eq_ignore_ascii_case(b"IFCMANIFOLDSOLIDBREP")
        && !type_name.eq_ignore_ascii_case(b"IFCADVANCEDBREP")
    {
        return None;
    }
    let fields = split_top_level_args(args);
    // (Outer: IfcClosedShell)
    let outer_id = match parse_field(fields.first()?) {
        Field::Ref(id) => id,
        _ => return None,
    };
    closed_shell(table, outer_id)
}

/// Mesh an `IfcClosedShell` / `IfcOpenShell` / `IfcConnectedFaceSet`
/// (walked directly, not via a Brep wrapper). All three share the same
/// shape — a `CfsFaces: LIST OF IfcFace` at attribute 0 — and shells are
/// just specialised connected face-sets in the schema. Accepting all
/// three here is what lets IfcFaceBasedSurfaceModel work, since its
/// `FbsmFaces` list contains `IfcConnectedFaceSet`s, not shells.
pub fn closed_shell(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCLOSEDSHELL")
        && !type_name.eq_ignore_ascii_case(b"IFCOPENSHELL")
        && !type_name.eq_ignore_ascii_case(b"IFCCONNECTEDFACESET")
    {
        return None;
    }
    let fields = split_top_level_args(args);
    // (CfsFaces: LIST OF IfcFace)
    let body = match parse_field(fields.first()?) {
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

/// Mesh an `IfcFaceBasedSurfaceModel`. Walks each `IfcConnectedFaceSet`
/// in `FbsmFaces` and unions the triangles.
pub fn face_based_surface_model(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCFACEBASEDSURFACEMODEL") {
        return None;
    }
    let fields = split_top_level_args(args);
    // FbsmFaces: SET OF IfcConnectedFaceSet
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut combined = LocalMesh::new();
    for f in split_top_level_args(body) {
        let face_set_id = match parse_field(f) {
            Field::Ref(id) => id,
            _ => continue,
        };
        if let Some(m) = closed_shell(table, face_set_id) {
            let base = (combined.vertices.len() / 3) as u32;
            // GH #153: every shell is rebased against its own first
            // point, so the raw vertex buffers are in different local
            // frames. Adopt the first shell's `rep_origin` for the
            // combined mesh and shift each later shell by the f64
            // difference — small (all shells of one product are
            // neighbours), so the f32 add is exact enough.
            if combined.vertices.is_empty() {
                combined.rep_origin = m.rep_origin;
            }
            let d = [
                (m.rep_origin[0] - combined.rep_origin[0]) as f32,
                (m.rep_origin[1] - combined.rep_origin[1]) as f32,
                (m.rep_origin[2] - combined.rep_origin[2]) as f32,
            ];
            for c in m.vertices.as_chunks::<3>().0 {
                combined.vertices.push(c[0] + d[0]);
                combined.vertices.push(c[1] + d[1]);
                combined.vertices.push(c[2] + d[2]);
            }
            for &idx in &m.indices {
                combined.indices.push(base + idx);
            }
        }
    }
    if combined.indices.is_empty() {
        return None;
    }
    Some(combined)
}

/// Mesh an `IfcShellBasedSurfaceModel` — same shape as FBSM but the
/// `SbsmBoundary` list holds `IfcShell` (Open|Closed).
pub fn shell_based_surface_model(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCSHELLBASEDSURFACEMODEL") {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut combined = LocalMesh::new();
    for f in split_top_level_args(body) {
        let shell_id = match parse_field(f) {
            Field::Ref(id) => id,
            _ => continue,
        };
        if let Some(m) = closed_shell(table, shell_id) {
            let base = (combined.vertices.len() / 3) as u32;
            // GH #153: every shell is rebased against its own first
            // point, so the raw vertex buffers are in different local
            // frames. Adopt the first shell's `rep_origin` for the
            // combined mesh and shift each later shell by the f64
            // difference — small (all shells of one product are
            // neighbours), so the f32 add is exact enough.
            if combined.vertices.is_empty() {
                combined.rep_origin = m.rep_origin;
            }
            let d = [
                (m.rep_origin[0] - combined.rep_origin[0]) as f32,
                (m.rep_origin[1] - combined.rep_origin[1]) as f32,
                (m.rep_origin[2] - combined.rep_origin[2]) as f32,
            ];
            for c in m.vertices.as_chunks::<3>().0 {
                combined.vertices.push(c[0] + d[0]);
                combined.vertices.push(c[1] + d[1]);
                combined.vertices.push(c[2] + d[2]);
            }
            for &idx in &m.indices {
                combined.indices.push(base + idx);
            }
        }
    }
    if combined.indices.is_empty() {
        return None;
    }
    Some(combined)
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
    let body = match parse_field(fields.first().unwrap_or(&&[][..])) {
        Field::List(b) => b,
        _ => return,
    };

    // Collect every bound on this face. `IfcFaceOuterBound` is the outer
    // contour; plain `IfcFaceBound`s are inner holes (window / door
    // reveals punched into a wall face, etc.). Earlier this code dropped
    // the inner bounds and fan-triangulated the outer loop only — that
    // over-fills the holes and over-reports solid volume by exactly the
    // hole area (GH #53: Sannergata ARK_E walls were +6 % … +122 % on
    // hole-bearing `IfcFacetedBrep` faces). We now honour inner bounds:
    // project the face to 2D and ear-clip with holes.
    let mut outer_loop: Option<(u64, bool)> = None;
    let mut inner_loops: Vec<(u64, bool)> = Vec::new();
    let mut first_bound: Option<(u64, bool)> = None;
    for bound_field in split_top_level_args(body) {
        let bound_id = match parse_field(bound_field) {
            Field::Ref(id) => id,
            _ => continue,
        };
        let (b_type, b_args) = match table.get(bound_id) {
            Some(x) => x,
            None => continue,
        };
        let is_outer = b_type.eq_ignore_ascii_case(b"IFCFACEOUTERBOUND");
        if !b_type.eq_ignore_ascii_case(b"IFCFACEBOUND") && !is_outer {
            continue;
        }
        let bf = split_top_level_args(b_args);
        // (Bound: IfcLoop, Orientation: BOOL)
        let loop_id = match parse_field(bf.first().unwrap_or(&&[][..])) {
            Field::Ref(id) => id,
            _ => continue,
        };
        let orient = match parse_field(bf.get(1).unwrap_or(&&[][..])) {
            // STEP booleans: `.T.` = true, `.F.` = false (enum form)
            Field::Enum(e) => e == b"T",
            _ => true,
        };
        if first_bound.is_none() {
            first_bound = Some((loop_id, orient));
        }
        if is_outer && outer_loop.is_none() {
            outer_loop = Some((loop_id, orient));
        } else {
            inner_loops.push((loop_id, orient));
        }
    }

    // Pick the outer contour. If no bound was explicitly tagged
    // `IfcFaceOuterBound`, the first bound is the outer one and there are
    // no holes to honour (`inner_loops` will hold the remaining bounds,
    // but without a declared outer we cannot reliably tell holes from a
    // multi-contour face, so we keep the old outer-only behaviour).
    let ((outer_loop_id, outer_orient), have_explicit_outer) = match outer_loop {
        Some(x) => (x, true),
        None => match first_bound {
            // first_bound was also pushed into inner_loops above when no
            // outer tag existed; drop it from the hole set.
            Some(x) => {
                if !inner_loops.is_empty() {
                    inner_loops.remove(0);
                }
                (x, false)
            }
            None => return,
        },
    };

    // Gather the outer loop's mesh vertex indices.
    let mut outer_verts: Vec<u32> = poly_loop_vertices(table, outer_loop_id, mesh, vertex_cache);
    if outer_verts.len() < 3 {
        return;
    }
    if !outer_orient {
        outer_verts.reverse();
    }

    // No declared holes (or no explicit outer tag) → keep the cheap fan
    // triangulation. Fan is exact for the convex / simple-polygon faces
    // that dominate breps and avoids the projection cost.
    if !have_explicit_outer || inner_loops.is_empty() {
        fan_triangulate(&outer_verts, mesh);
        return;
    }

    // Hole-bearing face: gather each inner loop's vertices, then
    // ear-clip the whole face (outer + holes) in 2D.
    let mut hole_vert_lists: Vec<Vec<u32>> = Vec::with_capacity(inner_loops.len());
    for (loop_id, orient) in &inner_loops {
        let mut hv = poly_loop_vertices(table, *loop_id, mesh, vertex_cache);
        if hv.len() < 3 {
            continue;
        }
        // earcutr wants holes wound opposite the outer contour; the IFC
        // `Orientation` flag already encodes the loop's sense relative to
        // the face, so apply it the same way we do for the outer loop and
        // let earcutr's signed-area logic place the hole.
        if !orient {
            hv.reverse();
        }
        hole_vert_lists.push(hv);
    }

    if hole_vert_lists.is_empty() {
        // All declared inner bounds were degenerate — fall back to fan.
        fan_triangulate(&outer_verts, mesh);
        return;
    }

    if triangulate_face_with_holes(mesh, &outer_verts, &hole_vert_lists) {
        return;
    }

    // Projection / ear-clip failed (degenerate face) — fan the outer loop
    // so the face is at least filled rather than dropped.
    fan_triangulate(&outer_verts, mesh);
}

/// Fan-triangulate a single closed loop of mesh vertex indices into
/// `mesh.indices`. Exact for convex polygons; the historical brep path.
fn fan_triangulate(verts: &[u32], mesh: &mut LocalMesh) {
    if verts.len() < 3 {
        return;
    }
    for i in 1..(verts.len() - 1) {
        mesh.indices.push(verts[0]);
        mesh.indices.push(verts[i]);
        mesh.indices.push(verts[i + 1]);
    }
}

/// Ear-clip a planar face (one outer loop + N hole loops, all given as
/// indices into `mesh.vertices`) with holes honoured, appending the
/// resulting triangles to `mesh.indices`. Returns `false` if the face is
/// too degenerate to project (zero-area outer loop), so the caller can
/// fall back to a fan.
///
/// The loops are 3D but coplanar (an `IfcFace` is planar by definition);
/// we compute the face plane via Newell's method over the outer loop,
/// build an orthonormal in-plane basis, project every loop vertex to 2D,
/// and run `earcutr` with the holes. earcutr's output triangle indices
/// address the concatenated loop order (outer then each hole), which we
/// map back to the original `mesh.vertices` indices. Winding is restored
/// to match the outer loop's CCW-in-plane sense so the emitted triangles
/// keep the face's outward normal.
pub(crate) fn triangulate_face_with_holes(
    mesh: &mut LocalMesh,
    outer: &[u32],
    holes: &[Vec<u32>],
) -> bool {
    // Fetch a mesh vertex by index.
    let vtx = |idx: u32| -> Vec3 {
        let b = idx as usize * 3;
        Vec3::new(mesh.vertices[b], mesh.vertices[b + 1], mesh.vertices[b + 2])
    };

    // Newell's normal over the outer loop (robust for non-planar-ish and
    // any vertex ordering).
    let mut normal = Vec3::ZERO;
    for i in 0..outer.len() {
        let a = vtx(outer[i]);
        let b = vtx(outer[(i + 1) % outer.len()]);
        normal.x += (a.y - b.y) * (a.z + b.z);
        normal.y += (a.z - b.z) * (a.x + b.x);
        normal.z += (a.x - b.x) * (a.y + b.y);
    }
    if normal.length_squared() < 1e-20 {
        return false;
    }
    let n = normal.normalize();

    // In-plane orthonormal basis (u, v) with u × v aligned to n, so the
    // projection preserves the outer loop's winding sense.
    let helper = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let u = (helper - n * helper.dot(n)).normalize();
    let v = n.cross(u);

    // Concatenate outer + holes into flat 2D coords and a hole-start list,
    // remembering each projected vertex's original mesh index.
    let total = outer.len() + holes.iter().map(|h| h.len()).sum::<usize>();
    let mut coords: Vec<f64> = Vec::with_capacity(total * 2);
    let mut orig: Vec<u32> = Vec::with_capacity(total);
    let push_loop = |loop_idx: &[u32], coords: &mut Vec<f64>, orig: &mut Vec<u32>| {
        for &mi in loop_idx {
            let p = vtx(mi);
            coords.push(p.dot(u) as f64);
            coords.push(p.dot(v) as f64);
            orig.push(mi);
        }
    };
    push_loop(outer, &mut coords, &mut orig);
    let mut hole_starts: Vec<usize> = Vec::with_capacity(holes.len());
    let mut acc = outer.len();
    for h in holes {
        hole_starts.push(acc);
        push_loop(h, &mut coords, &mut orig);
        acc += h.len();
    }

    let tris = earcutr::earcut(&coords, &hole_starts, 2).unwrap_or_default();
    if tris.is_empty() {
        return false;
    }
    // earcutr returns CCW triangles in the (u, v) plane; since (u, v, n)
    // is right-handed, that CCW sense already matches the face normal n.
    for t in tris.as_chunks::<3>().0 {
        mesh.indices.push(orig[t[0]]);
        mesh.indices.push(orig[t[1]]);
        mesh.indices.push(orig[t[2]]);
    }
    true
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
    let body = match parse_field(fields.first().unwrap_or(&&[][..])) {
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
        // Far-origin rebase (GH #153) — the same contract `faceset.rs`
        // applies to `IfcCartesianPointList3D`: parse in f64, subtract a
        // representation-local origin, and only THEN downcast to f32.
        // IFC2x3 breps routinely bake world coords straight into the
        // `IfcCartesianPoint`s; at 6e8 the f32 ULP is ~32 mm, so packing
        // the raw coordinate quantises the whole shell before the bake
        // loop ever sees it. The offset rides on `LocalMesh.rep_origin`
        // and the bake loop re-applies it through an f64 anchor
        // (`mesh/mod.rs`: `effective_f64.transform_point3(rep_origin)`),
        // so world placement is unchanged — only the precision improves.
        //
        // Origin = the FIRST point of the shell (not the bbox-min the
        // point-list path can afford): the brep walk streams points face
        // by face and never holds them all, and any point of the shell
        // is equally valid as the rebase datum — the residual coords are
        // bounded by the shell's own extent either way.
        if mesh.vertices.is_empty() {
            mesh.rep_origin = [p.x, p.y, p.z];
        }
        let d = p - DVec3::new(mesh.rep_origin[0], mesh.rep_origin[1], mesh.rep_origin[2]);
        let idx = (mesh.vertices.len() / 3) as u32;
        mesh.vertices.push(d.x as f32);
        mesh.vertices.push(d.y as f32);
        mesh.vertices.push(d.z as f32);
        vertex_cache.insert(pt_id, idx);
        out.push(idx);
    }
    out
}

/// An `IfcCartesianPoint` in **f64**. Parsed at full precision so the
/// caller can rebase (GH #153) before the f32 downcast — an f32 parse
/// here would already have quantised a world-coordinate brep.
fn cartesian_point(table: &EntityTable, id: u64) -> Option<DVec3> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINT") {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let coords: Vec<f64> = split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Number(n) => Some(n),
            _ => None,
        })
        .collect();
    Some(DVec3::new(
        *coords.first().unwrap_or(&0.0),
        *coords.get(1).unwrap_or(&0.0),
        *coords.get(2).unwrap_or(&0.0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GH #153 fixture: an IFC2x3-style `IfcFacetedBrep` with world
    /// coordinates (6e8) baked straight into its `IfcCartesianPoint`s.
    /// A tetrahedron with 1000-unit legs anchored at (6e8, 6e8, 0).
    /// At 6e8 the f32 ULP is ~64 units, so packing the raw coordinate
    /// quantises the 1000-unit legs into ~64-unit steps; the rebase
    /// keeps them exact.
    const FAR_ORIGIN_BREP_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('far.ifc','2026-09-06T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1=IFCCARTESIANPOINT((600000000.,600000000.,0.));
#2=IFCCARTESIANPOINT((600001000.,600000000.,0.));
#3=IFCCARTESIANPOINT((600000000.,600001000.,0.));
#4=IFCCARTESIANPOINT((600000000.,600000000.,1000.));
#10=IFCPOLYLOOP((#1,#3,#2));
#11=IFCFACEOUTERBOUND(#10,.T.);
#12=IFCFACE((#11));
#20=IFCPOLYLOOP((#1,#2,#4));
#21=IFCFACEOUTERBOUND(#20,.T.);
#22=IFCFACE((#21));
#30=IFCPOLYLOOP((#2,#3,#4));
#31=IFCFACEOUTERBOUND(#30,.T.);
#32=IFCFACE((#31));
#40=IFCPOLYLOOP((#3,#1,#4));
#41=IFCFACEOUTERBOUND(#40,.T.);
#42=IFCFACE((#41));
#50=IFCCLOSEDSHELL((#12,#22,#32,#42));
#60=IFCFACETEDBREP(#50);
#70=IFCSHELLBASEDSURFACEMODEL((#50));
ENDSEC;
END-ISO-10303-21;
"#;

    /// Per-axis (max - min) of the f32 vertex buffer.
    fn vertex_spread(mesh: &LocalMesh) -> [f32; 3] {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for c in mesh.vertices.as_chunks::<3>().0 {
            for (a, v) in c.iter().enumerate() {
                lo[a] = lo[a].min(*v);
                hi[a] = hi[a].max(*v);
            }
        }
        [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]
    }

    /// The rebase contract (GH #153): with world coords baked into the
    /// points, the f32 buffer must still resolve the 1000-unit legs to
    /// better than 1e-3 relative, and `rep_origin` must carry the f64
    /// offset the bake loop re-applies. Without the rebase the spread
    /// quantises to ~64-unit steps (f32 ULP at 6e8) and this fails.
    #[test]
    fn faceted_brep_far_origin_preserves_vertex_spread() {
        let table = EntityTable::build(FAR_ORIGIN_BREP_IFC.as_bytes());
        let mesh = faceted_brep(&table, 60).expect("brep #60 meshes");

        // The offset the bake loop re-applies is the shell's first point.
        assert!((mesh.rep_origin[0] - 600000000.0).abs() < 1e-6);
        assert!((mesh.rep_origin[1] - 600000000.0).abs() < 1e-6);
        assert!(mesh.rep_origin[2].abs() < 1e-6);

        let spread = vertex_spread(&mesh);
        for (axis, got) in spread.iter().enumerate() {
            assert!(
                (got - 1000.0).abs() < 1.0,
                "axis {axis}: expected a 1000-unit spread within 1e-3 \
                 relative, got {got} (f32 collapse at 6e8?)",
            );
        }

        // And the world reconstruction (rep_origin + vertex) still lands
        // on the authored coordinates.
        let mut max_err = 0.0_f64;
        for c in mesh.vertices.as_chunks::<3>().0 {
            let world = [
                mesh.rep_origin[0] + c[0] as f64,
                mesh.rep_origin[1] + c[1] as f64,
                mesh.rep_origin[2] + c[2] as f64,
            ];
            let expected = [
                [600000000.0, 600000000.0, 0.0],
                [600001000.0, 600000000.0, 0.0],
                [600000000.0, 600001000.0, 0.0],
                [600000000.0, 600000000.0, 1000.0],
            ];
            let best = expected
                .iter()
                .map(|e| {
                    (0..3)
                        .map(|a| (world[a] - e[a]).abs())
                        .fold(0.0_f64, f64::max)
                })
                .fold(f64::INFINITY, f64::min);
            max_err = max_err.max(best);
        }
        assert!(max_err < 1.0, "world reconstruction off by {max_err}");
    }

    /// Same contract on the `IfcShellBasedSurfaceModel` path — it merges
    /// per-shell meshes, so it must reconcile their `rep_origin`s too.
    #[test]
    fn shell_based_surface_model_far_origin_preserves_vertex_spread() {
        let table = EntityTable::build(FAR_ORIGIN_BREP_IFC.as_bytes());
        let mesh = shell_based_surface_model(&table, 70).expect("sbsm #70 meshes");
        assert!((mesh.rep_origin[0] - 600000000.0).abs() < 1e-6);
        let spread = vertex_spread(&mesh);
        for (axis, got) in spread.iter().enumerate() {
            assert!(
                (got - 1000.0).abs() < 1.0,
                "sbsm axis {axis}: expected 1000, got {got}",
            );
        }
    }

    /// Total triangle area of a `LocalMesh` (signed → abs), summed over
    /// all triangles. For a single planar face this is the face area.
    fn tri_area(mesh: &LocalMesh) -> f32 {
        let v = |i: u32| -> Vec3 {
            let b = i as usize * 3;
            Vec3::new(mesh.vertices[b], mesh.vertices[b + 1], mesh.vertices[b + 2])
        };
        mesh.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| 0.5 * (v(t[1]) - v(t[0])).cross(v(t[2]) - v(t[0])).length())
            .sum()
    }

    /// A 10×10 square face in the XY plane with a centred 4×4 square hole
    /// must triangulate to area 100 - 16 = 84, NOT the 100 a hole-blind
    /// fan would yield. This is the geometric core of the GH #53 fix.
    #[test]
    fn face_with_square_hole_excludes_hole_area() {
        let mut mesh = LocalMesh::new();
        // Outer CCW (z=0): 4 verts.
        let outer_pts = [
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [10.0, 10.0, 0.0],
            [0.0, 10.0, 0.0],
        ];
        // Inner hole, wound CW (opposite the outer) as IFC authors holes.
        let hole_pts = [
            [3.0, 3.0, 0.0],
            [3.0, 7.0, 0.0],
            [7.0, 7.0, 0.0],
            [7.0, 3.0, 0.0],
        ];
        let mut push = |p: &[[f32; 3]]| -> Vec<u32> {
            p.iter()
                .map(|c| {
                    let idx = (mesh.vertices.len() / 3) as u32;
                    mesh.vertices.extend_from_slice(c);
                    idx
                })
                .collect()
        };
        let outer = push(&outer_pts);
        let hole = push(&hole_pts);

        assert!(triangulate_face_with_holes(&mut mesh, &outer, &[hole]));
        let area = tri_area(&mesh);
        assert!(
            (area - 84.0).abs() < 1e-3,
            "expected hole-excluded area 84, got {area}"
        );
    }

    /// The same face triangulated by the legacy fan path (outer only)
    /// over-fills the hole — confirming the bug the fix removes. Fan area
    /// is the full 100.
    #[test]
    fn fan_triangulate_overfills_hole() {
        let mut mesh = LocalMesh::new();
        for c in [
            [0.0, 0.0, 0.0f32],
            [10.0, 0.0, 0.0],
            [10.0, 10.0, 0.0],
            [0.0, 10.0, 0.0],
        ] {
            mesh.vertices.extend_from_slice(&c);
        }
        fan_triangulate(&[0, 1, 2, 3], &mut mesh);
        assert!((tri_area(&mesh) - 100.0).abs() < 1e-3);
    }

    /// An angled (non-axis-aligned, tilted) face with a hole still
    /// projects and ear-clips correctly: a unit-square face tilted 45° in
    /// Z with a centred hole keeps the planar 2D area (Newell projection
    /// is rotation-invariant).
    #[test]
    fn tilted_face_with_hole_projects_correctly() {
        let mut mesh = LocalMesh::new();
        // Square in a plane tilted so z = x (45° about Y). Side length in
        // the plane is sqrt(2) per unit of x, so a 0..1 x-range square is
        // 1 (y) × sqrt(2) (in-plane) = sqrt(2) area; with a hole we check
        // the ratio instead of an absolute to stay projection-agnostic.
        let s = |x: f32, y: f32| [x, y, x]; // z=x tilt
        let outer_pts = [s(0.0, 0.0), s(1.0, 0.0), s(1.0, 1.0), s(0.0, 1.0)];
        let hole_pts = [s(0.4, 0.4), s(0.4, 0.6), s(0.6, 0.6), s(0.6, 0.4)];
        let mut push = |p: &[[f32; 3]]| -> Vec<u32> {
            p.iter()
                .map(|c| {
                    let idx = (mesh.vertices.len() / 3) as u32;
                    mesh.vertices.extend_from_slice(c);
                    idx
                })
                .collect()
        };
        let outer = push(&outer_pts);
        let hole = push(&hole_pts);
        assert!(triangulate_face_with_holes(&mut mesh, &outer, &[hole]));
        // Outer in-plane area = sqrt(2); hole = 0.2*0.2*sqrt(2) = 0.04*sqrt(2).
        let expected = std::f32::consts::SQRT_2 * (1.0 - 0.04);
        let area = tri_area(&mesh);
        assert!(
            (area - expected).abs() < 1e-3,
            "expected tilted hole-excluded area {expected}, got {area}"
        );
    }
}
