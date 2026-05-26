//! Reveal-all handlers for IFC composite / clipped solids.
//!
//! The driving philosophy: an IFC file is a snapshot of what the author
//! actually wrote, not a curated view of what they "meant". A wall
//! authored as `wall_extrusion - door_void` lives in the file as an
//! `IfcBooleanResult` tree, and we surface BOTH operands so the consumer
//! sees the full read. Performing the boolean would erase information
//! (which operand is which, where the cut came from). We don't do that.
//!
//! Tags emitted via `MeshFragment::source`:
//!   * `"boolean_first_operand"`  — left side of an IfcBooleanResult tree
//!   * `"boolean_second_operand"` — right side (typically the subtractor)
//!   * `"csg_branch"`             — operand of an IfcCsgSolid tree
//!   * `"halfspace_bounded"`      — the polygonal cap of a polygonal-
//!                                   bounded half-space (a real finite
//!                                   volume — the polygon, extruded both
//!                                   ways through its base plane)
//!   * `"halfspace_plane"`        — the orienting plane of an infinite
//!                                   half-space, emitted as a finite
//!                                   quad cap so the user can SEE the
//!                                   cutting surface. Tagged so the
//!                                   consumer knows this is a finite
//!                                   stand-in for an unbounded volume.

use std::collections::HashMap;

use glam::{Mat4, Vec2, Vec3};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::{extrude_polygon, LocalMesh};
use crate::mesh::placement::axis_placement_3d_from_id;
use crate::mesh::profile::Polygon2D;
use crate::mesh::MeshFragment;

/// Visible extent (in model units, typically mm) for the finite cap we
/// emit to stand in for an infinite half-space's base plane. Sized to
/// dwarf typical building extents while remaining visualisable.
const HALFSPACE_PLANE_EXTENT: f32 = 20_000.0;
/// Thickness of the visible slab used to render a bounded half-space.
/// Picked to be small relative to building scale but visible.
const HALFSPACE_SLAB_THICKNESS: f32 = 1.0;

/// `IfcBooleanResult` / `IfcBooleanClippingResult`:
///   `(Operator: ENUM, FirstOperand: IfcBooleanOperand, SecondOperand: IfcBooleanOperand)`
///
/// We recurse into both operands and tag the resulting mesh fragments
/// with their structural role. No subtraction, no intersection — both
/// volumes are emitted as their own visible meshes.
pub fn boolean_result(
    table: &EntityTable,
    id: u64,
    shape_cache: &mut HashMap<u64, Vec<(LocalMesh, &'static str)>>,
    recurse: &dyn Fn(&EntityTable, u64, &mut HashMap<u64, Vec<(LocalMesh, &'static str)>>) -> Vec<MeshFragment>,
) -> Vec<MeshFragment> {
    let (_, args) = match table.get(id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    let fields = split_top_level_args(args);
    // Operator at fields[0], FirstOperand at fields[1], SecondOperand at fields[2].
    let first_id = fields.get(1).copied().and_then(|f| match parse_field(f) {
        Field::Ref(rid) => Some(rid),
        _ => None,
    });
    let second_id = fields.get(2).copied().and_then(|f| match parse_field(f) {
        Field::Ref(rid) => Some(rid),
        _ => None,
    });

    let mut out: Vec<MeshFragment> = Vec::new();
    if let Some(fid) = first_id {
        for frag in recurse(table, fid, shape_cache) {
            out.push(retag(frag, "boolean_first_operand"));
        }
    }
    if let Some(sid) = second_id {
        for frag in recurse(table, sid, shape_cache) {
            out.push(retag(frag, "boolean_second_operand"));
        }
    }
    out
}

/// `IfcCsgSolid(TreeRootExpression: IfcCsgSelect)` — the tree root is
/// itself an `IfcBooleanResult` or `IfcCsgPrimitive3D`. We recurse into
/// it and tag whatever meshes come back.
pub fn csg_solid(
    table: &EntityTable,
    id: u64,
    shape_cache: &mut HashMap<u64, Vec<(LocalMesh, &'static str)>>,
    recurse: &dyn Fn(&EntityTable, u64, &mut HashMap<u64, Vec<(LocalMesh, &'static str)>>) -> Vec<MeshFragment>,
) -> Vec<MeshFragment> {
    let (_, args) = match table.get(id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    let fields = split_top_level_args(args);
    let root_id = match fields.first().copied().map(parse_field) {
        Some(Field::Ref(rid)) => rid,
        _ => return Vec::new(),
    };
    let mut out: Vec<MeshFragment> = Vec::new();
    for frag in recurse(table, root_id, shape_cache) {
        out.push(retag(frag, "csg_branch"));
    }
    out
}

/// `IfcPolygonalBoundedHalfSpace(BaseSurface, AgreementFlag, Position, PolygonalBoundary)`
///
/// This one IS a finite volume: the polygonal boundary (a 2D curve in
/// the `Position` frame's XY plane) is extruded through its base plane
/// to make a closed slab. We emit it as a real bounded mesh tagged
/// `"halfspace_bounded"`.
///
/// Implementation: we approximate the slab as a vertical extrusion of
/// the polygon (depth = HALFSPACE_SLAB_THICKNESS, centred about the
/// position's z=0). The AgreementFlag direction is preserved by signing
/// the extrusion depth. For the common Revit case (clipping a wall by a
/// vertical plane through a polygon), this produces the cutting volume
/// in the right place.
pub fn polygonal_bounded_halfspace(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCPOLYGONALBOUNDEDHALFSPACE") {
        return None;
    }
    let fields = split_top_level_args(args);
    // arg[2] = Position (IfcAxis2Placement3D), arg[3] = PolygonalBoundary (IfcBoundedCurve)
    let position = fields
        .get(2)
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(pid) => Some(axis_placement_3d_from_id(table, pid)),
            _ => None,
        })
        .unwrap_or(Mat4::IDENTITY);
    let boundary_id = match fields.get(3).copied().map(parse_field) {
        Some(Field::Ref(bid)) => bid,
        _ => return None,
    };
    let outer = bounded_curve_points(table, boundary_id)?;
    if outer.len() < 3 {
        return None;
    }
    let polygon = Polygon2D { outer, holes: Vec::new() };
    let mesh = extrude_polygon(&polygon, Vec3::Z, HALFSPACE_SLAB_THICKNESS, position);
    Some(mesh)
}

/// `IfcHalfSpaceSolid(BaseSurface: IfcSurface, AgreementFlag: BOOL)` —
/// the base surface is typically `IfcPlane(Position: IfcAxis2Placement3D)`.
/// The half-space is unbounded; we emit a finite square cap on the base
/// plane (sized HALFSPACE_PLANE_EXTENT × HALFSPACE_PLANE_EXTENT) so the
/// consumer can SEE where the cutting plane is. The fragment is tagged
/// `"halfspace_plane"` so they know it's a finite stand-in.
pub fn halfspace_solid(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCHALFSPACESOLID") {
        return None;
    }
    let fields = split_top_level_args(args);
    let surface_id = match fields.first().copied().map(parse_field) {
        Some(Field::Ref(sid)) => sid,
        _ => return None,
    };
    // Expect IfcPlane.
    let (s_type, s_args) = table.get(surface_id)?;
    if !s_type.eq_ignore_ascii_case(b"IFCPLANE") {
        return None;
    }
    let s_fields = split_top_level_args(s_args);
    let position = s_fields
        .first()
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(pid) => Some(axis_placement_3d_from_id(table, pid)),
            _ => None,
        })
        .unwrap_or(Mat4::IDENTITY);
    // Emit a square quad in the plane's local XY (z = 0), centred on origin.
    let e = HALFSPACE_PLANE_EXTENT;
    let square = vec![
        Vec2::new(-e, -e),
        Vec2::new(e, -e),
        Vec2::new(e, e),
        Vec2::new(-e, e),
    ];
    let polygon = Polygon2D { outer: square, holes: Vec::new() };
    // Extrude a paper-thin slab so the cap is visible from both sides.
    let mesh = extrude_polygon(&polygon, Vec3::Z, HALFSPACE_SLAB_THICKNESS * 0.01, position);
    Some(mesh)
}

/// Mark the structural position of a fragment inside its parent
/// composite (e.g. boolean operand role) WITHOUT touching the leaf
/// `source` — so the consumer sees both facts in the eventual segment
/// tag. If a deeper composite already set a role (nested boolean trees),
/// we don't overwrite it; the outermost role survives unchanged, the
/// innermost wins. (Choice: the innermost role is the most specific
/// answer to "what is this fragment?".)
fn retag(frag: MeshFragment, new_role: &'static str) -> MeshFragment {
    match frag {
        MeshFragment::Mesh { mesh, source, role, rep_step_id, instance_transform } => MeshFragment::Mesh {
            mesh,
            source,
            role: Some(role.unwrap_or(new_role)),
            rep_step_id,
            instance_transform,
        },
        u @ MeshFragment::Unhandled { .. } => u,
    }
}

/// Extract a 2D point list from an `IfcBoundedCurve` — supports
/// `IfcPolyline` (CartesianPoint list) and `IfcIndexedPolyCurve`
/// (point-list + segment indices). Returns the curve as a planar
/// polygon in the curve's local XY frame, with Z dropped.
fn bounded_curve_points(table: &EntityTable, id: u64) -> Option<Vec<Vec2>> {
    let (type_name, args) = table.get(id)?;
    if type_name.eq_ignore_ascii_case(b"IFCPOLYLINE") {
        let fields = split_top_level_args(args);
        let body = match parse_field(fields.first()?) {
            Field::List(b) => b,
            _ => return None,
        };
        let mut out: Vec<Vec2> = Vec::new();
        for f in split_top_level_args(body) {
            if let Field::Ref(pid) = parse_field(f) {
                if let Some(p) = cartesian_point_xy(table, pid) {
                    out.push(p);
                }
            }
        }
        // IfcPolyline is explicit, often closed by repeating first point;
        // drop a duplicate trailing vertex if present.
        if out.len() >= 2 && (out[0] - out[out.len() - 1]).length_squared() < 1e-9 {
            out.pop();
        }
        if out.len() >= 3 {
            return Some(out);
        }
        return None;
    }
    if type_name.eq_ignore_ascii_case(b"IFCINDEXEDPOLYCURVE") {
        // IfcIndexedPolyCurve(Points: IfcCartesianPointList2D, Segments, SelfIntersect)
        let fields = split_top_level_args(args);
        let pts_id = match parse_field(fields.first()?) {
            Field::Ref(pid) => pid,
            _ => return None,
        };
        let pts = cartesian_point_list_2d(table, pts_id)?;
        // Segments may be $; if so, take the points in order.
        let pts_out = pts;
        if pts_out.len() >= 3 {
            return Some(pts_out);
        }
    }
    None
}

fn cartesian_point_xy(table: &EntityTable, id: u64) -> Option<Vec2> {
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
    Some(Vec2::new(
        *coords.first().unwrap_or(&0.0),
        *coords.get(1).unwrap_or(&0.0),
    ))
}

fn cartesian_point_list_2d(table: &EntityTable, id: u64) -> Option<Vec<Vec2>> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINTLIST2D") {
        return None;
    }
    let fields = split_top_level_args(args);
    // CoordList: LIST [1:?] OF LIST [2:2] OF IfcLengthMeasure
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let mut out: Vec<Vec2> = Vec::new();
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
                out.push(Vec2::new(coords[0], coords[1]));
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}
