//! `IfcRevolvedAreaSolid` → triangle mesh.
//!
//! Schema:
//! ```text
//! IfcRevolvedAreaSolid(
//!     SweptArea: IfcProfileDef,         -- profile in position-local XY
//!     Position : IfcAxis2Placement3D,   -- local frame
//!     Axis     : IfcAxis1Placement,     -- rotation axis in Position's frame
//!     Angle    : IfcPlaneAngleMeasure)  -- radians
//! ```
//!
//! The profile is rotated around `Axis` (a 3D line = point + unit
//! direction) through `Angle` radians. The result is a solid of
//! revolution: a cylinder when the profile is a rectangle whose
//! edge is parallel to the axis, a torus when the profile is a
//! circle offset from the axis, a vase when the profile is some
//! free-form polyline, etc.
//!
//! Tessellation strategy:
//!   1. Extract the profile via `profile::extract` (outer + holes).
//!   2. Discretise the sweep into `N` angular steps where N scales
//!      with the total angle (max 32 around a full revolution).
//!   3. For each step `k`, rotate every profile boundary point by
//!      `k * dθ` around the axis. This yields `N+1` rings of 2D
//!      → 3D points.
//!   4. Side ribbons: for every edge (vᵢ, vᵢ₊₁) on the outer loop
//!      and each hole loop, emit a quad between consecutive rings.
//!   5. End caps (only if the sweep is *not* a full revolution):
//!      triangulate the profile at step 0 (CW) and step N (CCW)
//!      so the solid is watertight.
//!   6. Apply `Position` to every vertex (the solid's local frame).
//!
//! The reveal-all stance still applies: an unrecognised profile
//! subtype is the only way this handler can fail to surface a mesh,
//! and `profile::extract` already buckets those upstream as
//! representation-level deferrals.

use std::f32::consts::{PI, TAU};

use glam::{Mat4, Vec2, Vec3, Vec4};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;
use crate::mesh::placement::axis_placement_3d_from_id;
use crate::mesh::profile::{self, Polygon2D};

const MAX_ANGULAR_STEPS: usize = 32;
const MIN_ANGULAR_STEPS: usize = 4;
const FULL_REVOLUTION_EPSILON: f32 = 1e-3;

pub fn revolved_area_solid(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCREVOLVEDAREASOLID") {
        return None;
    }
    let fields = split_top_level_args(args);

    // arg[0] SweptArea, arg[1] Position, arg[2] Axis (IfcAxis1Placement), arg[3] Angle.
    let swept_id = match parse_field(fields.first()?) {
        Field::Ref(id) => id,
        _ => return None,
    };
    let polygon = profile::extract(table, swept_id)?;

    let position = match fields.get(1).copied().map(parse_field) {
        Some(Field::Ref(pid)) => axis_placement_3d_from_id(table, pid),
        _ => Mat4::IDENTITY,
    };

    let (axis_origin, axis_direction) = match fields.get(2).copied().map(parse_field) {
        Some(Field::Ref(aid)) => axis_1_placement(table, aid)?,
        _ => return None,
    };

    let angle: f32 = match fields.get(3).copied().map(parse_field) {
        Some(Field::Number(n)) => n as f32,
        _ => return None,
    };
    if angle.abs() < 1e-6 {
        return None;
    }

    // Number of angular steps. Min 4 even for very small angles so
    // a curved surface is recognisable. Scales linearly with angle
    // up to MAX_ANGULAR_STEPS for a full revolution.
    let n_steps = ((MAX_ANGULAR_STEPS as f32) * angle.abs() / TAU)
        .round()
        .max(MIN_ANGULAR_STEPS as f32) as usize;
    let dtheta = angle / n_steps as f32;
    let needs_caps = (angle.abs() - TAU).abs() > FULL_REVOLUTION_EPSILON;

    let axis_dir = axis_direction.normalize_or_zero();
    if axis_dir.length_squared() < 0.5 {
        return None;
    }

    let mut mesh = LocalMesh::new();

    // Build rings: a ring is one rotated copy of the profile boundary
    // (outer + holes concatenated, in the same order each ring).
    let ring_vertex_count = polygon.outer.len()
        + polygon.holes.iter().map(|h| h.len()).sum::<usize>();
    let n_rings = n_steps + 1;
    let mut ring_starts: Vec<u32> = Vec::with_capacity(n_rings);

    for k in 0..n_rings {
        let theta = k as f32 * dtheta;
        let rot = rotation_about_axis(axis_origin, axis_dir, theta);
        let ring_start = (mesh.vertices.len() / 3) as u32;
        ring_starts.push(ring_start);
        push_ring(&mut mesh, &polygon, &rot, &position);
    }

    // Side ribbons. For each loop (outer or hole), connect ring k to
    // ring k+1 with quads. Outer loop is wound CCW in 2D — when
    // rotated forward, the outward normal of the side ribbon points
    // *away* from the axis if the profile is on the +X side.
    let mut loop_offset: usize = 0;
    let outer_len = polygon.outer.len();
    emit_side_ribbon(&mut mesh, &ring_starts, loop_offset, outer_len, dtheta < 0.0);
    loop_offset += outer_len;
    for hole in &polygon.holes {
        let hole_len = hole.len();
        // Holes wound opposite to outer in 2D → flip the side
        // winding so their inward-facing ribbons still face out.
        emit_side_ribbon(&mut mesh, &ring_starts, loop_offset, hole_len, dtheta >= 0.0);
        loop_offset += hole_len;
    }

    // End caps. The triangulation is shared by both caps — same 2D
    // earcut applied at the first and last ring.
    if needs_caps {
        let triangulation = triangulate(&polygon);
        let first = ring_starts[0];
        let last = ring_starts[n_steps];
        // Start cap: wound so its outward normal points opposite the
        // sweep direction (≈ -dθ × profile-plane-normal).
        for tri in triangulation.chunks_exact(3) {
            // Reverse winding so the start cap faces away from the
            // material side.
            mesh.indices.push(first + tri[2] as u32);
            mesh.indices.push(first + tri[1] as u32);
            mesh.indices.push(first + tri[0] as u32);
        }
        // End cap: forward winding.
        for tri in triangulation.chunks_exact(3) {
            mesh.indices.push(last + tri[0] as u32);
            mesh.indices.push(last + tri[1] as u32);
            mesh.indices.push(last + tri[2] as u32);
        }
    }

    if mesh.indices.is_empty() {
        return None;
    }
    let _ = ring_vertex_count; // computed for clarity, not currently asserted
    Some(mesh)
}

fn axis_1_placement(table: &EntityTable, id: u64) -> Option<(Vec3, Vec3)> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCAXIS1PLACEMENT") {
        return None;
    }
    let fields = split_top_level_args(args);
    let loc_id = match parse_field(fields.first()?) {
        Field::Ref(id) => id,
        _ => return None,
    };
    let origin = cartesian_point_3d(table, loc_id).unwrap_or(Vec3::ZERO);
    // arg[1] = Axis (OPTIONAL IfcDirection), default (0,0,1).
    let axis = match fields.get(1).copied().map(parse_field) {
        Some(Field::Ref(did)) => direction_3d(table, did).unwrap_or(Vec3::Z),
        _ => Vec3::Z,
    };
    Some((origin, axis))
}

fn cartesian_point_3d(table: &EntityTable, id: u64) -> Option<Vec3> {
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
    Some(Vec3::new(
        *coords.first().unwrap_or(&0.0),
        *coords.get(1).unwrap_or(&0.0),
        *coords.get(2).unwrap_or(&0.0),
    ))
}

fn direction_3d(table: &EntityTable, id: u64) -> Option<Vec3> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCDIRECTION") {
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
    Some(Vec3::new(
        *coords.first().unwrap_or(&0.0),
        *coords.get(1).unwrap_or(&0.0),
        *coords.get(2).unwrap_or(&0.0),
    ))
}

/// Rotation around an arbitrary axis line (point + unit direction) by
/// `theta` radians. The result is a 4×4 affine matrix that takes a 3D
/// point in the solid's local frame to its rotated counterpart, also
/// in the solid's local frame.
fn rotation_about_axis(origin: Vec3, dir: Vec3, theta: f32) -> Mat4 {
    let t_back = Mat4::from_translation(origin);
    let t_fwd = Mat4::from_translation(-origin);
    let r = Mat4::from_axis_angle(dir, theta);
    t_back * r * t_fwd
}

/// Push one ring of vertices into `mesh`. A ring is the profile
/// boundary (outer + holes, in source order) rotated by `rot` and
/// then mapped into the solid's local frame by `position`.
fn push_ring(mesh: &mut LocalMesh, polygon: &Polygon2D, rot: &Mat4, position: &Mat4) {
    let combined = position.mul_mat4(rot);
    let mut push = |p: Vec2| {
        let world = combined * Vec4::new(p.x, p.y, 0.0, 1.0);
        mesh.vertices.push(world.x);
        mesh.vertices.push(world.y);
        mesh.vertices.push(world.z);
    };
    for p in &polygon.outer {
        push(*p);
    }
    for hole in &polygon.holes {
        for p in hole {
            push(*p);
        }
    }
}

/// Emit side ribbon quads for one loop of the profile, connecting
/// every consecutive pair of rings. `loop_offset` is the index within
/// a ring where this loop starts; `loop_len` is the number of points.
/// `flip_winding` reverses triangle winding so the outward normal
/// points consistently outward regardless of sweep direction.
fn emit_side_ribbon(
    mesh: &mut LocalMesh,
    ring_starts: &[u32],
    loop_offset: usize,
    loop_len: usize,
    flip_winding: bool,
) {
    if loop_len < 2 || ring_starts.len() < 2 {
        return;
    }
    for k in 0..(ring_starts.len() - 1) {
        let r0 = ring_starts[k];
        let r1 = ring_starts[k + 1];
        for i in 0..loop_len {
            let i_next = (i + 1) % loop_len;
            let a = r0 + (loop_offset + i) as u32;
            let b = r0 + (loop_offset + i_next) as u32;
            let c = r1 + (loop_offset + i_next) as u32;
            let d = r1 + (loop_offset + i) as u32;
            if flip_winding {
                mesh.indices.extend_from_slice(&[a, c, b, a, d, c]);
            } else {
                mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }
    }
}

/// Triangulate a `Polygon2D` (outer + holes) — same earcutr-based
/// routine as `extrusion::triangulate`, duplicated here to avoid
/// changing the visibility of that helper. Returns flat triple-of-
/// indices into the ring (outer first, then holes in order).
fn triangulate(polygon: &Polygon2D) -> Vec<usize> {
    let mut coords: Vec<f64> = Vec::with_capacity(
        2 * (polygon.outer.len() + polygon.holes.iter().map(|h| h.len()).sum::<usize>()),
    );
    for p in &polygon.outer {
        coords.push(p.x as f64);
        coords.push(p.y as f64);
    }
    let mut hole_starts: Vec<usize> = Vec::with_capacity(polygon.holes.len());
    let mut acc = polygon.outer.len();
    for h in &polygon.holes {
        hole_starts.push(acc);
        for p in h {
            coords.push(p.x as f64);
            coords.push(p.y as f64);
        }
        acc += h.len();
    }
    earcutr::earcut(&coords, &hole_starts, 2).unwrap_or_default()
}

// Keep `PI` import warning-free if a future tweak drops the usage.
const _: f32 = PI;
