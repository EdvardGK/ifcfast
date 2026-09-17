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

    // No declared holes (or no explicit outer tag) → triangulate the
    // single loop. Convex loops keep the cheap fan (exact, and breps
    // dominate the triangle budget); concave loops go through earcut so
    // a notch isn't bridged (GH #177).
    if !have_explicit_outer || inner_loops.is_empty() {
        triangulate_simple_loop(mesh, &outer_verts);
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

/// Triangulate ONE closed, hole-free loop of mesh vertex indices.
///
/// GH #177: fanning from vertex 0 is exact only for convex loops. A
/// concave hole-free outline (L / C / U footprints, a notched plate, a
/// brep face whose outer bound wraps a re-entrant corner) gets triangles
/// that bridge the notch, so the mesh over-fills the opening — and the
/// resulting shell over-reports area and, when closed, volume, with no
/// flag on it.
///
/// So: classify first, pay second. A cheap 3D convexity test keeps the
/// fan for the convex quads and triangles that dominate a brep-heavy
/// model's triangle budget (see GH #171 — MEP breps are the hot path),
/// and only concave loops pay Newell + projection + earcut. Triangle
/// count is `n - 2` either way, so nothing but connectivity moves.
pub(crate) fn triangulate_simple_loop(mesh: &mut LocalMesh, verts: &[u32]) {
    let n = verts.len();
    if n < 3 {
        return;
    }
    if n == 3 {
        mesh.indices.push(verts[0]);
        mesh.indices.push(verts[1]);
        mesh.indices.push(verts[2]);
        return;
    }
    if loop_is_convex(mesh, verts) {
        fan_triangulate(verts, mesh);
        return;
    }
    // Concave (or the loop's Newell normal is degenerate) — ear-clip it
    // with an empty hole set. If the projection bails the face is too
    // degenerate to clip, so fan it anyway: a present face beats a
    // dropped one.
    if !triangulate_face_with_holes(mesh, verts, &[]) {
        fan_triangulate(verts, mesh);
    }
}

/// Is a closed 3D loop convex when viewed along its own Newell normal?
///
/// Every consecutive edge pair's cross product, dotted with the face
/// normal, must share one sign. Collinear / near-collinear corners are
/// neutral — they carry no information about convexity and must not be
/// allowed to flip the verdict (a zero-length or doubled-back edge would
/// otherwise mark half the corpus concave).
///
/// The test is **per corner and scale-free**, which a single loop-global
/// epsilon on the raw cross-product cannot be. `turn` has units of
/// length², so a threshold of the form `max_edge² · k` makes a corner's
/// verdict depend on how long the loop's LONGEST edge happens to be: a
/// genuine reflex corner between two short edges is silently neutral,
/// whatever its angle. That is not academic — a chord plus a finely
/// tessellated concave arc (N ≳ 42 segments, so each arc edge is ≲1 % of
/// the chord) reads as convex, fans, and bridges the segment. Two
/// constants, both dimensionless:
///
/// * `MIN_EDGE_FRACTION_SQ = 1e-6` — a corner whose incoming or outgoing
///   edge is shorter than `1e-3` of the longest edge (hence `1e-6` on
///   squared lengths) is neutral. `max_edge_sq` is still computed for
///   exactly this, as the loop's scale reference. At that length the
///   f32 direction noise on rebased vertices is ~1e-4 rad, so this
///   guards the sine threshold below with a 10× margin.
/// * `MIN_SIN_TURN = 1e-3` — for every other corner, `turn` is compared
///   against `1e-3 · |e1| · |e2|`, i.e. `sin θ > 1e-3` (~0.06°),
///   independent of the loop's units and of the other edges' lengths.
///
/// Verdict: convex iff no two signed corners disagree in sign.
///
/// Returns `false` for a degenerate loop (zero Newell normal), which
/// routes the caller to earcut — which reports the degeneracy properly.
fn loop_is_convex(mesh: &LocalMesh, verts: &[u32]) -> bool {
    let vtx = |idx: u32| -> Vec3 {
        let b = idx as usize * 3;
        Vec3::new(mesh.vertices[b], mesh.vertices[b + 1], mesh.vertices[b + 2])
    };
    let n = verts.len();

    // Newell normal + the loop's scale, in one pass.
    let mut normal = Vec3::ZERO;
    let mut max_edge_sq = 0.0_f32;
    for i in 0..n {
        let a = vtx(verts[i]);
        let b = vtx(verts[(i + 1) % n]);
        normal.x += (a.y - b.y) * (a.z + b.z);
        normal.y += (a.z - b.z) * (a.x + b.x);
        normal.z += (a.x - b.x) * (a.y + b.y);
        max_edge_sq = max_edge_sq.max((b - a).length_squared());
    }
    if normal.length_squared() < 1e-20 || max_edge_sq <= 0.0 {
        return false;
    }
    let nrm = normal.normalize();

    // See the doc comment: both constants are dimensionless, and the
    // second one makes each corner's verdict independent of the rest of
    // the loop.
    const MIN_EDGE_FRACTION_SQ: f32 = 1e-6;
    const MIN_SIN_TURN: f32 = 1e-3;
    let min_edge_sq = max_edge_sq * MIN_EDGE_FRACTION_SQ;

    let mut sign = 0_i32;
    for i in 0..n {
        let a = vtx(verts[i]);
        let b = vtx(verts[(i + 1) % n]);
        let c = vtx(verts[(i + 2) % n]);
        let e1 = b - a;
        let e2 = c - b;
        let e1_sq = e1.length_squared();
        let e2_sq = e2.length_squared();
        // Degenerate corner: one of the edges is noise at this scale.
        if e1_sq < min_edge_sq || e2_sq < min_edge_sq {
            continue;
        }
        let turn = e1.cross(e2).dot(nrm);
        // |e1 × e2| = |e1||e2| sin θ, so this is a pure angle threshold.
        let eps = MIN_SIN_TURN * (e1_sq * e2_sq).sqrt();
        if turn > eps {
            if sign < 0 {
                return false;
            }
            sign = 1;
        } else if turn < -eps {
            if sign > 0 {
                return false;
            }
            sign = -1;
        }
    }
    true
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

    /// GH #177: a concave, hole-free loop must NOT be fan-filled. The
    /// L-footprint below is a 10×10 square minus a 6×6 corner: true area
    /// 100 - 36 = 64. Ear-clipping keeps the notch empty; the fan bridges
    /// it (see `fan_triangulate_bridges_concave_notch`). Triangle count
    /// is n - 2 = 4 either way — only connectivity moves.
    #[test]
    fn concave_l_face_does_not_bridge_notch() {
        const L: [[f32; 3]; 6] = [
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [10.0, 4.0, 0.0],
            [4.0, 4.0, 0.0],
            [4.0, 10.0, 0.0],
            [0.0, 10.0, 0.0],
        ];
        let mut mesh = LocalMesh::new();
        let loop_idx: Vec<u32> = L
            .iter()
            .map(|c| {
                let idx = (mesh.vertices.len() / 3) as u32;
                mesh.vertices.extend_from_slice(c);
                idx
            })
            .collect();

        triangulate_simple_loop(&mut mesh, &loop_idx);
        let area = tri_area(&mesh);
        assert!(
            (area - 64.0).abs() < 1e-3,
            "expected the notch left empty (area 64), got {area}"
        );
        assert_eq!(
            mesh.indices.len() / 3,
            4,
            "an n=6 loop must still yield n - 2 = 4 triangles"
        );

        // The same loop started at a different vertex is the same
        // polygon, so it must give the same area. (It does not for the
        // fan: vertex 0 of the ordering above happens to see the whole
        // L, which is exactly why the bug hid for so long.)
        let mut rot = LocalMesh::new();
        let rot_idx: Vec<u32> = [1usize, 2, 3, 4, 5, 0]
            .iter()
            .map(|&k| {
                let idx = (rot.vertices.len() / 3) as u32;
                rot.vertices.extend_from_slice(&L[k]);
                idx
            })
            .collect();
        triangulate_simple_loop(&mut rot, &rot_idx);
        let rot_area = tri_area(&rot);
        assert!(
            (rot_area - 64.0).abs() < 1e-3,
            "rotated start vertex: expected 64, got {rot_area}"
        );
        assert_eq!(rot.indices.len() / 3, 4);
    }

    /// The legacy fan path over-fills the same concave loop — the bug
    /// GH #177 removes. Started at (10,0) the fan emits a triangle that
    /// spans the notch plus one wound backwards, and the unsigned area
    /// comes out at the full 100 instead of 64.
    #[test]
    fn fan_triangulate_bridges_concave_notch() {
        let mut mesh = LocalMesh::new();
        for c in [
            [10.0, 0.0, 0.0f32],
            [10.0, 4.0, 0.0],
            [4.0, 4.0, 0.0],
            [4.0, 10.0, 0.0],
            [0.0, 10.0, 0.0],
            [0.0, 0.0, 0.0],
        ] {
            mesh.vertices.extend_from_slice(&c);
        }
        fan_triangulate(&[0, 1, 2, 3, 4, 5], &mut mesh);
        let area = tri_area(&mesh);
        assert!(
            area > 64.0 + 1e-3,
            "fan should over-fill the notch, got {area}"
        );
        assert!(
            (area - 100.0).abs() < 1e-3,
            "expected fan area 100, got {area}"
        );
    }

    /// Convex loops keep the exact fan — same triangles, same order,
    /// same indices. Breps dominate the triangle budget (GH #171), so
    /// the convexity gate must not push quads and hexagons through
    /// Newell + projection + earcut, and must not move any existing
    /// mesh's connectivity.
    #[test]
    fn convex_face_still_fans() {
        // Regular-ish hexagon in the XY plane.
        let hex: Vec<[f32; 3]> = (0..6)
            .map(|i| {
                let a = std::f32::consts::TAU * (i as f32) / 6.0;
                [5.0 * a.cos(), 5.0 * a.sin(), 0.0]
            })
            .collect();

        let build = |verts: &[[f32; 3]]| -> (LocalMesh, Vec<u32>) {
            let mut m = LocalMesh::new();
            let idx = verts
                .iter()
                .map(|c| {
                    let i = (m.vertices.len() / 3) as u32;
                    m.vertices.extend_from_slice(c);
                    i
                })
                .collect();
            (m, idx)
        };

        let (mut via_helper, idx) = build(&hex);
        triangulate_simple_loop(&mut via_helper, &idx);

        let (mut via_fan, idx2) = build(&hex);
        fan_triangulate(&idx2, &mut via_fan);

        assert_eq!(
            via_helper.indices, via_fan.indices,
            "a convex loop must take the fan fast path unchanged"
        );
        assert_eq!(via_helper.indices.len() / 3, 4);
    }

    /// GH #177 (review): the loop-global epsilon. `eps = max_edge² *
    /// 1e-4` makes a corner's verdict depend on the loop's LONGEST edge,
    /// so a re-entrant boundary made of many short edges is neutral
    /// regardless of its angle. The loop below is a 10 × 5 rectangle
    /// whose bottom side is replaced by a concave 100-segment arc
    /// bulging up to (5, 2): the long sides set `max_edge = 10`
    /// (`eps = 0.01`), while each arc corner contributes only
    /// `|e1||e2| sin θ ≈ 0.11² · 0.0152 ≈ 1.8e-4` — three orders below
    /// the threshold. Every arc corner reads neutral, the four
    /// rectangle corners agree in sign, the loop is called convex, and
    /// the fan bridges the arc and over-reports the face by the
    /// segment's ~13.75 units of area.
    ///
    /// The per-corner test compares against `1e-3 · |e1| · |e2|`, i.e.
    /// `sin θ > 1e-3`, so the same corners are signed at any
    /// tessellation density and the loop is concave.
    #[test]
    fn chord_plus_fine_concave_arc_is_concave() {
        // Circle through (0,0), (5,2), (10,0): centre (5, -5.25), r = 7.25.
        const CX: f64 = 5.0;
        const CY: f64 = -5.25;
        const R: f64 = 7.25;
        const N: usize = 100;

        let a_start = (0.0f64 - CY).atan2(0.0 - CX); // at (0,0)
        let a_end = (0.0f64 - CY).atan2(10.0 - CX); // at (10,0)

        let mut pts: Vec<[f32; 3]> = Vec::new();
        // Concave arc, (0,0) -> (5,2) -> (10,0), endpoints included.
        for i in 0..=N {
            let t = i as f64 / N as f64;
            let a = a_start + (a_end - a_start) * t;
            pts.push([(CX + R * a.cos()) as f32, (CY + R * a.sin()) as f32, 0.0]);
        }
        // Three long straight sides back round: the loop's scale.
        pts.push([10.0, 5.0, 0.0]);
        pts.push([0.0, 5.0, 0.0]);

        let mut mesh = LocalMesh::new();
        let idx: Vec<u32> = pts
            .iter()
            .map(|c| {
                let i = (mesh.vertices.len() / 3) as u32;
                mesh.vertices.extend_from_slice(c);
                i
            })
            .collect();

        assert!(
            !loop_is_convex(&mesh, &idx),
            "a 100-segment concave arc between two long edges must read concave"
        );

        // Shoelace over the authored polygon, in f64.
        let mut shoelace = 0.0f64;
        for i in 0..pts.len() {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            shoelace += a[0] as f64 * b[1] as f64 - b[0] as f64 * a[1] as f64;
        }
        let expected = (0.5 * shoelace).abs() as f32;

        triangulate_simple_loop(&mut mesh, &idx);
        let area = tri_area(&mesh);
        assert!(
            (area - expected).abs() < 1e-3,
            "expected the arc left empty (area {expected}), got {area}"
        );
        assert_eq!(
            mesh.indices.len() / 3,
            pts.len() - 2,
            "an n-gon must still yield n - 2 triangles"
        );
    }

    /// The other side of the same constant: a densely tessellated CONVEX
    /// loop must stay on the fan fast path. A regular 100-gon turns
    /// 3.6° per corner — 60× the `sin θ > 1e-3` floor — so every corner
    /// is signed, and they all agree.
    #[test]
    fn regular_100gon_is_still_convex() {
        let mut mesh = LocalMesh::new();
        let n = 100;
        let idx: Vec<u32> = (0..n)
            .map(|i| {
                let a = std::f32::consts::TAU * (i as f32) / (n as f32);
                let j = (mesh.vertices.len() / 3) as u32;
                mesh.vertices
                    .extend_from_slice(&[5.0 * a.cos(), 5.0 * a.sin(), 0.0]);
                j
            })
            .collect();
        assert!(
            loop_is_convex(&mesh, &idx),
            "a regular 100-gon must still take the fan fast path"
        );
    }

    /// The convexity test and the earcut projection are both computed in
    /// the face's own plane, so a tilted concave face behaves the same.
    /// The L-footprint lifted onto the plane z = x scales in-plane areas
    /// by sqrt(2): 64 * sqrt(2).
    #[test]
    fn concave_face_tilted_plane() {
        let mut mesh = LocalMesh::new();
        let s = |x: f32, y: f32| [x, y, x]; // z = x tilt
        let pts = [
            s(0.0, 0.0),
            s(10.0, 0.0),
            s(10.0, 4.0),
            s(4.0, 4.0),
            s(4.0, 10.0),
            s(0.0, 10.0),
        ];
        let idx: Vec<u32> = pts
            .iter()
            .map(|c| {
                let i = (mesh.vertices.len() / 3) as u32;
                mesh.vertices.extend_from_slice(c);
                i
            })
            .collect();
        triangulate_simple_loop(&mut mesh, &idx);
        let expected = 64.0 * std::f32::consts::SQRT_2;
        let area = tri_area(&mesh);
        assert!(
            (area - expected).abs() < 1e-3,
            "expected tilted concave area {expected}, got {area}"
        );
        assert_eq!(mesh.indices.len() / 3, 4);
    }

    /// GH #177 end-to-end: a closed `IfcFacetedBrep` L-prism (10×10
    /// footprint minus a 6×6 corner, extruded 2) walked through the real
    /// `IfcFace` / `IfcFaceOuterBound` / `IfcPolyLoop` traversal.
    ///
    /// Analytic surface area = 2 caps (64 each) + perimeter 40 × height
    /// 2 = 128 + 80 = 208. The caps are the concave faces; fanning them
    /// from the wrong start vertex inflates the total.
    const L_PRISM_BREP_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('lprism.ifc','2026-09-17T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCCARTESIANPOINT((0.,0.,0.));
#2=IFCCARTESIANPOINT((10.,0.,0.));
#3=IFCCARTESIANPOINT((10.,4.,0.));
#4=IFCCARTESIANPOINT((4.,4.,0.));
#5=IFCCARTESIANPOINT((4.,10.,0.));
#6=IFCCARTESIANPOINT((0.,10.,0.));
#7=IFCCARTESIANPOINT((0.,0.,2.));
#8=IFCCARTESIANPOINT((10.,0.,2.));
#9=IFCCARTESIANPOINT((10.,4.,2.));
#10=IFCCARTESIANPOINT((4.,4.,2.));
#11=IFCCARTESIANPOINT((4.,10.,2.));
#12=IFCCARTESIANPOINT((0.,10.,2.));
#100=IFCPOLYLOOP((#1,#6,#5,#4,#3,#2));
#101=IFCFACEOUTERBOUND(#100,.T.);
#102=IFCFACE((#101));
#110=IFCPOLYLOOP((#7,#8,#9,#10,#11,#12));
#111=IFCFACEOUTERBOUND(#110,.T.);
#112=IFCFACE((#111));
#120=IFCPOLYLOOP((#1,#2,#8,#7));
#121=IFCFACEOUTERBOUND(#120,.T.);
#122=IFCFACE((#121));
#130=IFCPOLYLOOP((#2,#3,#9,#8));
#131=IFCFACEOUTERBOUND(#130,.T.);
#132=IFCFACE((#131));
#140=IFCPOLYLOOP((#3,#4,#10,#9));
#141=IFCFACEOUTERBOUND(#140,.T.);
#142=IFCFACE((#141));
#150=IFCPOLYLOOP((#4,#5,#11,#10));
#151=IFCFACEOUTERBOUND(#150,.T.);
#152=IFCFACE((#151));
#160=IFCPOLYLOOP((#5,#6,#12,#11));
#161=IFCFACEOUTERBOUND(#160,.T.);
#162=IFCFACE((#161));
#170=IFCPOLYLOOP((#6,#1,#7,#12));
#171=IFCFACEOUTERBOUND(#170,.T.);
#172=IFCFACE((#171));
#200=IFCCLOSEDSHELL((#102,#112,#122,#132,#142,#152,#162,#172));
#201=IFCFACETEDBREP(#200);
ENDSEC;
END-ISO-10303-21;
"#;

    #[test]
    fn faceted_brep_concave_l_prism_surface_area() {
        let table = EntityTable::build(L_PRISM_BREP_IFC.as_bytes());
        let mesh = faceted_brep(&table, 201).expect("brep #201 meshes");
        let area = tri_area(&mesh);
        assert!(
            (area - 208.0).abs() < 1e-3,
            "expected L-prism surface area 208 (2*64 caps + 40*2 sides), got {area}"
        );
        // 2 concave caps at 4 triangles + 6 quad sides at 2 = 20.
        assert_eq!(mesh.indices.len() / 3, 20);
    }
}
