//! `IfcCsgPrimitive3D` leaves — closed-form tessellation.
//!
//! Five concrete subtypes per the IFC schema:
//!   - `IfcBlock(Position, XLength, YLength, ZLength)`
//!   - `IfcRightCircularCylinder(Position, Height, Radius)`
//!   - `IfcRightCircularCone(Position, Height, BottomRadius)`
//!   - `IfcSphere(Position, Radius)`
//!   - `IfcRectangularPyramid(Position, XLength, YLength, Height)`
//!
//! All five are parametric and self-contained: a 3D placement plus
//! scalar dimensions, no profile lookups, no curve dependencies. The
//! tessellation is closed-form, no triangulation library needed.
//!
//! These are the leaf shapes that `IfcCsgSolid` composes. Without
//! handlers here, the composite dispatcher in `boolean::csg_solid`
//! would bottom out on `MeshFragment::Unhandled` at every leaf and
//! the assembled solid would surface as empty. With handlers, the
//! reveal-all stance applies all the way through the CSG tree:
//! every operand becomes a tagged segment.
//!
//! Discretisation choice: curved surfaces (cylinder, cone, sphere)
//! tessellate at fixed densities (24 segments around, 12 latitudes
//! for the sphere). High enough for QTO `surface_area` / `volume`
//! to converge to within ~1%, low enough that an annotated-heavy
//! file with thousands of small primitives doesn't blow vertex
//! budgets. A future commit can expose this as a tolerance knob.

use std::f32::consts::PI;

use glam::{Mat4, Vec3, Vec4};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;
use crate::mesh::placement::axis_placement_3d_from_id;

/// Curved-surface discretisation. Used for cylinder side, cone side,
/// cylinder/cone caps, sphere longitude ring.
const CIRCUMFERENCE_SEGMENTS: usize = 24;

/// Sphere latitude bands (between the two poles). Combined with
/// `CIRCUMFERENCE_SEGMENTS` longitudes gives 24 × 11 ≈ 264 quads.
const SPHERE_LATITUDE_BANDS: usize = 12;

/// Dispatch any `IfcCsgPrimitive3D` subtype to its tessellator. Returns
/// `None` if the type is unknown — caller surfaces that as
/// `MeshFragment::Unhandled` so the reveal-all guarantee holds.
pub fn csg_primitive(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, _) = table.get(id)?;
    if type_name.eq_ignore_ascii_case(b"IFCBLOCK") {
        block(table, id)
    } else if type_name.eq_ignore_ascii_case(b"IFCRIGHTCIRCULARCYLINDER") {
        right_circular_cylinder(table, id)
    } else if type_name.eq_ignore_ascii_case(b"IFCRIGHTCIRCULARCONE") {
        right_circular_cone(table, id)
    } else if type_name.eq_ignore_ascii_case(b"IFCSPHERE") {
        sphere(table, id)
    } else if type_name.eq_ignore_ascii_case(b"IFCRECTANGULARPYRAMID") {
        rectangular_pyramid(table, id)
    } else {
        None
    }
}

/// `IfcBlock(Position, XLength, YLength, ZLength)` — an axis-aligned
/// box from local origin to `(XLength, YLength, ZLength)`.
pub fn block(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCBLOCK") {
        return None;
    }
    let fields = split_top_level_args(args);
    let position = placement_from_field(table, fields.first().copied())?;
    let x = number_at(&fields, 1)?;
    let y = number_at(&fields, 2)?;
    let z = number_at(&fields, 3)?;

    let mut mesh = LocalMesh::new();
    // 8 corners.
    let corners = [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(x, 0.0, 0.0),
        Vec3::new(x, y, 0.0),
        Vec3::new(0.0, y, 0.0),
        Vec3::new(0.0, 0.0, z),
        Vec3::new(x, 0.0, z),
        Vec3::new(x, y, z),
        Vec3::new(0.0, y, z),
    ];
    let base = push_transformed_vertices(&mut mesh, &corners, &position);

    // 6 faces, each as 2 triangles, wound CCW from the outside.
    // Bottom (z=0), top (z=Z), and four sides.
    let f = |i: u32| base + i;
    let q = |mesh: &mut LocalMesh, a, b, c, d| {
        mesh.indices.extend_from_slice(&[f(a), f(b), f(c), f(a), f(c), f(d)]);
    };
    q(&mut mesh, 0, 3, 2, 1); // bottom (normal -Z)
    q(&mut mesh, 4, 5, 6, 7); // top    (normal +Z)
    q(&mut mesh, 0, 1, 5, 4); // -Y
    q(&mut mesh, 1, 2, 6, 5); // +X
    q(&mut mesh, 2, 3, 7, 6); // +Y
    q(&mut mesh, 3, 0, 4, 7); // -X

    Some(mesh)
}

/// `IfcRightCircularCylinder(Position, Height, Radius)` — cylinder
/// with base centred at the position origin, axis along local +Z,
/// apex at `(0, 0, Height)`.
pub fn right_circular_cylinder(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCRIGHTCIRCULARCYLINDER") {
        return None;
    }
    let fields = split_top_level_args(args);
    let position = placement_from_field(table, fields.first().copied())?;
    let height = number_at(&fields, 1)?;
    let radius = number_at(&fields, 2)?;

    let n = CIRCUMFERENCE_SEGMENTS;
    let mut local: Vec<Vec3> = Vec::with_capacity(2 * n + 2);

    // Bottom ring (0..n), top ring (n..2n), bottom centre (2n), top centre (2n+1).
    for i in 0..n {
        let theta = (i as f32) * 2.0 * PI / (n as f32);
        local.push(Vec3::new(radius * theta.cos(), radius * theta.sin(), 0.0));
    }
    for i in 0..n {
        let theta = (i as f32) * 2.0 * PI / (n as f32);
        local.push(Vec3::new(radius * theta.cos(), radius * theta.sin(), height));
    }
    local.push(Vec3::new(0.0, 0.0, 0.0));
    local.push(Vec3::new(0.0, 0.0, height));

    let mut mesh = LocalMesh::new();
    let base = push_transformed_vertices(&mut mesh, &local, &position);
    let bot_center = base + (2 * n) as u32;
    let top_center = base + (2 * n + 1) as u32;

    for i in 0..n {
        let i0 = base + i as u32;
        let i1 = base + ((i + 1) % n) as u32;
        let j0 = i0 + n as u32;
        let j1 = i1 + n as u32;
        // Side strip (normal pointing outward).
        mesh.indices.extend_from_slice(&[i0, i1, j1, i0, j1, j0]);
        // Bottom fan (normal -Z) — wound CW when viewed from +Z so the
        // outward normal is -Z.
        mesh.indices.extend_from_slice(&[bot_center, i1, i0]);
        // Top fan (normal +Z).
        mesh.indices.extend_from_slice(&[top_center, j0, j1]);
    }

    Some(mesh)
}

/// `IfcRightCircularCone(Position, Height, BottomRadius)` — cone with
/// base centred at the position origin, axis along local +Z, apex at
/// `(0, 0, Height)`. Bottom radius `BottomRadius`, top radius 0.
pub fn right_circular_cone(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCRIGHTCIRCULARCONE") {
        return None;
    }
    let fields = split_top_level_args(args);
    let position = placement_from_field(table, fields.first().copied())?;
    let height = number_at(&fields, 1)?;
    let radius = number_at(&fields, 2)?;

    let n = CIRCUMFERENCE_SEGMENTS;
    let mut local: Vec<Vec3> = Vec::with_capacity(n + 2);

    for i in 0..n {
        let theta = (i as f32) * 2.0 * PI / (n as f32);
        local.push(Vec3::new(radius * theta.cos(), radius * theta.sin(), 0.0));
    }
    local.push(Vec3::new(0.0, 0.0, 0.0));     // bottom centre
    local.push(Vec3::new(0.0, 0.0, height));  // apex

    let mut mesh = LocalMesh::new();
    let base = push_transformed_vertices(&mut mesh, &local, &position);
    let bot_center = base + n as u32;
    let apex = base + (n + 1) as u32;

    for i in 0..n {
        let i0 = base + i as u32;
        let i1 = base + ((i + 1) % n) as u32;
        // Side triangle from apex to base edge (outward normal).
        mesh.indices.extend_from_slice(&[apex, i0, i1]);
        // Bottom fan (outward normal -Z, wound CW from above).
        mesh.indices.extend_from_slice(&[bot_center, i1, i0]);
    }

    Some(mesh)
}

/// `IfcSphere(Position, Radius)` — centre at position origin, latitude-
/// longitude tessellation.
pub fn sphere(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCSPHERE") {
        return None;
    }
    let fields = split_top_level_args(args);
    let position = placement_from_field(table, fields.first().copied())?;
    let radius = number_at(&fields, 1)?;

    let n_lon = CIRCUMFERENCE_SEGMENTS;
    let n_lat = SPHERE_LATITUDE_BANDS;
    let mut local: Vec<Vec3> = Vec::with_capacity((n_lat - 1) * n_lon + 2);

    // Ring vertices for each interior latitude band.
    for lat in 1..n_lat {
        let phi = PI * (lat as f32) / (n_lat as f32);
        let z = radius * phi.cos();
        let r_xy = radius * phi.sin();
        for lon in 0..n_lon {
            let theta = (lon as f32) * 2.0 * PI / (n_lon as f32);
            local.push(Vec3::new(r_xy * theta.cos(), r_xy * theta.sin(), z));
        }
    }
    let north = local.len();
    local.push(Vec3::new(0.0, 0.0, radius));
    let south = local.len();
    local.push(Vec3::new(0.0, 0.0, -radius));

    let mut mesh = LocalMesh::new();
    let base = push_transformed_vertices(&mut mesh, &local, &position);
    let north = base + north as u32;
    let south = base + south as u32;

    let ring = |lat: usize, lon: usize| base + (lat * n_lon + lon) as u32;

    // North pole cap (lat = 0 ring = first latitude band built).
    for lon in 0..n_lon {
        let a = ring(0, lon);
        let b = ring(0, (lon + 1) % n_lon);
        mesh.indices.extend_from_slice(&[north, a, b]);
    }
    // Quads between latitude rings.
    for lat in 0..(n_lat - 2) {
        for lon in 0..n_lon {
            let a = ring(lat, lon);
            let b = ring(lat, (lon + 1) % n_lon);
            let c = ring(lat + 1, (lon + 1) % n_lon);
            let d = ring(lat + 1, lon);
            mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    // South pole cap.
    let last_lat = n_lat - 2;
    for lon in 0..n_lon {
        let a = ring(last_lat, lon);
        let b = ring(last_lat, (lon + 1) % n_lon);
        mesh.indices.extend_from_slice(&[south, b, a]);
    }

    Some(mesh)
}

/// `IfcRectangularPyramid(Position, XLength, YLength, Height)` — base
/// is a rectangle centred at the position origin in the local XY plane
/// (XLength × YLength), apex at `(0, 0, Height)`.
pub fn rectangular_pyramid(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCRECTANGULARPYRAMID") {
        return None;
    }
    let fields = split_top_level_args(args);
    let position = placement_from_field(table, fields.first().copied())?;
    let x = number_at(&fields, 1)?;
    let y = number_at(&fields, 2)?;
    let h = number_at(&fields, 3)?;

    // IFC4 spec: base spans (0..XLength, 0..YLength) in the local XY
    // plane with origin at one corner (not centred).
    let local = [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(x, 0.0, 0.0),
        Vec3::new(x, y, 0.0),
        Vec3::new(0.0, y, 0.0),
        Vec3::new(0.0, 0.0, h),
    ];
    let mut mesh = LocalMesh::new();
    let base = push_transformed_vertices(&mut mesh, &local, &position);
    let v = |i: u32| base + i;

    // Base, normal -Z (CW from above).
    mesh.indices.extend_from_slice(&[v(0), v(3), v(2), v(0), v(2), v(1)]);
    // Four side triangles to the apex.
    mesh.indices.extend_from_slice(&[v(0), v(1), v(4)]);
    mesh.indices.extend_from_slice(&[v(1), v(2), v(4)]);
    mesh.indices.extend_from_slice(&[v(2), v(3), v(4)]);
    mesh.indices.extend_from_slice(&[v(3), v(0), v(4)]);

    Some(mesh)
}

fn placement_from_field(table: &EntityTable, field: Option<&[u8]>) -> Option<Mat4> {
    let f = field?;
    match parse_field(f) {
        Field::Ref(pid) => Some(axis_placement_3d_from_id(table, pid)),
        _ => Some(Mat4::IDENTITY),
    }
}

fn number_at(fields: &[&[u8]], idx: usize) -> Option<f32> {
    match parse_field(*fields.get(idx)?) {
        Field::Number(n) => Some(n as f32),
        _ => None,
    }
}

/// Apply a placement matrix to every vertex and append to the mesh.
/// Returns the index of the first appended vertex so the caller can
/// build triangle indices.
fn push_transformed_vertices(mesh: &mut LocalMesh, verts: &[Vec3], placement: &Mat4) -> u32 {
    let start = (mesh.vertices.len() / 3) as u32;
    for v in verts {
        let w = *placement * Vec4::new(v.x, v.y, v.z, 1.0);
        mesh.vertices.push(w.x);
        mesh.vertices.push(w.y);
        mesh.vertices.push(w.z);
    }
    start
}
