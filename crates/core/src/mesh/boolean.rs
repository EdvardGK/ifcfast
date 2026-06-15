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

use glam::{Mat4, Vec2, Vec3};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::{extrude_polygon, LocalMesh};
use crate::mesh::placement::axis_placement_3d_from_id;
use crate::mesh::profile::Polygon2D;
use crate::mesh::{BoundedHalfspacePayload, MeshFragment};

/// Visible extent (in model units, typically mm) for the finite cap we
/// emit to stand in for an infinite half-space's base plane. Sized to
/// dwarf typical building extents while remaining visualisable.
const HALFSPACE_PLANE_EXTENT: f32 = 20_000.0;
/// Thickness of the visible slab used to render a bounded half-space.
/// Picked to be small relative to building scale but visible.
const HALFSPACE_SLAB_THICKNESS: f32 = 1.0;

/// Map an `IfcBooleanOperator` enum token to the role tag the second
/// operand carries in the source chain. The operator distinguishes
/// how `cut_openings` treats the second operand:
///
/// * `.DIFFERENCE.` → `"boolean_second_operand"` — the operand is a
///   cutter; in cut mode it is subtracted from the first operand.
///   `IfcBooleanClippingResult` is always DIFFERENCE by schema rule.
/// * `.UNION.` → `"boolean_union_operand"` — additive geometry, NOT a
///   cutter. Reveal-all already emits both operands; cut mode must not
///   subtract it (doing so produces `first − second` where the file
///   says `first ∪ second`). Surfaced as
///   `Outcome::Unsupported(UnionWithOverlap)` because the overlap
///   volume is double-counted (we don't compute the true union).
/// * `.INTERSECTION.` → `"boolean_intersection_operand"` — the net
///   solid is `first ∩ second`, which we don't compute; reveal-all
///   over-reports. Surfaced as
///   `Outcome::Unsupported(IntersectionNotImplemented)`.
///
/// A missing / malformed operator defaults to DIFFERENCE: it is the
/// overwhelmingly common case (every clipping result, almost every
/// authored boolean) and preserves the pre-W4 behaviour exactly. See
/// [GH #58] / W4.
fn second_operand_role(operator: Option<&[u8]>) -> &'static str {
    match operator.map(parse_field) {
        Some(Field::Enum(b"UNION")) => "boolean_union_operand",
        Some(Field::Enum(b"INTERSECTION")) => "boolean_intersection_operand",
        // `.DIFFERENCE.` and anything we can't read fall here.
        _ => "boolean_second_operand",
    }
}

/// `IfcBooleanResult` / `IfcBooleanClippingResult`:
///   `(Operator: ENUM, FirstOperand: IfcBooleanOperand, SecondOperand: IfcBooleanOperand)`
///
/// We recurse into both operands and tag the resulting mesh fragments
/// with their structural role. No subtraction, no intersection — both
/// volumes are emitted as their own visible meshes (reveal-all). The
/// second operand's role tag encodes the operator (W4) so downstream
/// `cut_openings` knows whether it is a cutter (DIFFERENCE) or additive
/// / intersecting geometry it must not subtract.
pub fn boolean_result(
    table: &EntityTable,
    id: u64,
    shape_cache: &super::ShapeCache,
    recurse: &dyn Fn(&EntityTable, u64, &super::ShapeCache) -> Vec<MeshFragment>,
) -> Vec<MeshFragment> {
    let (_, args) = match table.get(id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    let fields = split_top_level_args(args);
    // Operator at fields[0], FirstOperand at fields[1], SecondOperand at fields[2].
    let second_role = second_operand_role(fields.first().copied());
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
            out.push(retag(frag, second_role));
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
    shape_cache: &super::ShapeCache,
    recurse: &dyn Fn(&EntityTable, u64, &super::ShapeCache) -> Vec<MeshFragment>,
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

/// Parse an IFC BOOLEAN field (`.T.` / `.F.`). IFC has no schema
/// default for AgreementFlag; honest fallback for malformed input is
/// `true` (the most common value across Revit / ArchiCAD / Tekla
/// exports, and the orientation that has the half-space pointing
/// along the surface's own normal).
fn parse_agreement_flag(raw: Option<&[u8]>) -> bool {
    match raw.map(parse_field) {
        Some(Field::Enum(b"T")) => true,
        Some(Field::Enum(b"F")) => false,
        _ => true,
    }
}

/// `IfcPolygonalBoundedHalfSpace(BaseSurface, AgreementFlag, Position, PolygonalBoundary)`
///
/// Emits a thin one-sided slab on the AgreementFlag side of the base
/// plane (the polygon, extruded by `HALFSPACE_SLAB_THICKNESS` in the
/// agreement direction). The slab is a visualisation stand-in — the
/// consumer can see the cutting plane and which side the half-space
/// occupies. `cut_openings` consumes the same slab via the
/// `halfspace_bounded:{agreement}` tag and clips the host against the
/// derived plane directly (no CSG kernel involvement) — see GH #39
/// and `mesh::halfspace_clip`.
pub fn polygonal_bounded_halfspace(
    table: &EntityTable,
    id: u64,
) -> Option<(LocalMesh, bool, BoundedHalfspacePayload)> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCPOLYGONALBOUNDEDHALFSPACE") {
        return None;
    }
    let fields = split_top_level_args(args);
    // IfcPolygonalBoundedHalfSpace inherits from IfcHalfSpaceSolid:
    //   arg[0] = BaseSurface (IfcPlane, inherited) — defines the
    //           cutting plane's normal via its Position.Axis.
    //   arg[1] = AgreementFlag (BOOL, inherited).
    //   arg[2] = Position (IfcAxis2Placement3D) — defines the LOCAL XY
    //           frame in which PolygonalBoundary's 2D points live;
    //           independent from BaseSurface.Position.
    //   arg[3] = PolygonalBoundary (IfcBoundedCurve).
    //
    // Pre-GH #52, we used arg[2]'s Position for the slab's orientation.
    // That's wrong: when BaseSurface.Position.Axis differs from
    // arg[2].Position.Axis, the slab's world normal lands on the
    // polygon's Z direction, not the cutting plane's normal — and
    // `cut_openings::derive_plane_from_slab` reads exactly that normal
    // to clip the host. Sannergata wall #50724 reproduced cleanly:
    // BaseSurface.Axis = (-0.02, 0, -0.9998) (tilted), arg[2].Axis =
    // (0, 0, 1) — pre-fix the wall emptied; post-fix it's preserved.
    let agreement = parse_agreement_flag(fields.get(1).copied());
    let base_surface_position = fields
        .first()
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(sid) => {
                let (s_type, s_args) = table.get(sid)?;
                if !s_type.eq_ignore_ascii_case(b"IFCPLANE") {
                    return None;
                }
                let s_fields = split_top_level_args(s_args);
                match s_fields.first().copied().map(parse_field) {
                    Some(Field::Ref(pid)) => Some(axis_placement_3d_from_id(table, pid)),
                    _ => None,
                }
            }
            _ => None,
        })
        .unwrap_or(Mat4::IDENTITY);
    // arg[2] = Position (IfcAxis2Placement3D) — the LOCAL frame the
    // PolygonalBoundary's 2D points live in. Independent from
    // BaseSurface.Position. W6 needs it to place the boundary polygon in
    // world for the bounded cut. Defaults to identity when absent.
    //
    // Only the `prism-csg-fast` bounded fast-path ever reads this; default
    // builds skip the placement-chain walk and carry an inert identity
    // xform (the payload is constructed but never re-baked or consumed).
    let boundary_position = if cfg!(feature = "prism-csg-fast") {
        fields
            .get(2)
            .copied()
            .and_then(|f| match parse_field(f) {
                Field::Ref(pid) => Some(axis_placement_3d_from_id(table, pid)),
                _ => None,
            })
            .unwrap_or(Mat4::IDENTITY)
    } else {
        Mat4::IDENTITY
    };
    let boundary_id = match fields.get(3).copied().map(parse_field) {
        Some(Field::Ref(bid)) => bid,
        _ => return None,
    };
    let outer = bounded_curve_points(table, boundary_id)?;
    if outer.len() < 3 {
        return None;
    }
    let polygon = Polygon2D { outer, holes: Vec::new() };
    // Slab orientation follows the **de-facto** IFC convention — what
    // ifcopenshell, Revit and web-ifc all do, which is the OPPOSITE of
    // the literal reading of the IFC4 doc text for `AgreementFlag`. See
    // ifcopenshell `src/ifcgeom/mapping/IfcHalfSpaceSolid.cpp:33`:
    //
    //     f->orientation.reset(!inst->AgreementFlag());
    //
    // and the OCCT kernel at `kernels/opencascade/solid.cpp:47`:
    //
    //     pnt = pln.Location().Translated(orientation ? +axis : -axis);
    //     halfspace = BRepPrimAPI_MakeHalfSpace(face, pnt);
    //
    // where `BRepPrimAPI_MakeHalfSpace(face, refPnt)` builds the half-
    // space CONTAINING `refPnt`. Net mapping:
    //   * `.T.` (`agreement=true`)  → keep +position.Z side
    //   * `.F.` (`agreement=false`) → keep -position.Z side
    //
    // We build a thin one-sided slab whose top-cap normal lives on the
    // SUBTRACTED side (the side `halfspace_clip` is told to remove).
    // `halfspace_clip::clip_by_plane` keeps the **negative** side of
    // the normal it's given, so:
    //   * `.T.` → slab built on -position.Z side (apply Y-180° rotation
    //              so local +Z lands on world -position.Z) → clip
    //              keeps +position.Z.
    //   * `.F.` → slab built on +position.Z side (no rotation) → clip
    //              keeps -position.Z.
    //
    // The Y-180° rotation has `det=+1`, so outward-facing windings are
    // preserved through the matrix.
    // The slab is built in **BaseSurface.Position**'s frame — its
    // local +Z is the cutting plane's normal direction (which is what
    // `cut_openings` needs). The polygon vertices were authored in the
    // arg[2].Position frame, so when that frame diverges from
    // BaseSurface.Position the slab's polygonal footprint will look
    // sheared/rotated in world — a visualisation cost, but cut_openings
    // only reads the first triangle's normal direction, so the cut is
    // still correct. Faithful polygon-shape preservation under
    // diverging frames would require projecting the polygon prism
    // onto the BaseSurface plane (out of scope here).
    let frame = if agreement {
        base_surface_position * Mat4::from_rotation_y(std::f32::consts::PI)
    } else {
        base_surface_position
    };
    let mesh = extrude_polygon(&polygon, Vec3::Z, HALFSPACE_SLAB_THICKNESS, frame);

    // W6 / F6 payload. `plane_normal` matches the slab's top-cap normal
    // (`frame`'s local +Z) — the direction `cut_openings` removes — so
    // the bounded fast-path and the existing infinite-plane fallback read
    // the same orientation. `plane_point` is the BaseSurface origin.
    // `boundary` stays in its arg[2] frame; `boundary_xform` maps it to
    // the (still solid-local) working frame the slab was built in. Both
    // are re-baked into the product's world frame by `tessellate_one`.
    let plane_normal = transform_vector(&frame, Vec3::Z).normalize_or_zero();
    let plane_point = transform_point_local(&base_surface_position, Vec3::ZERO);
    let payload = BoundedHalfspacePayload {
        boundary: polygon,
        boundary_xform: boundary_position,
        plane_normal,
        plane_point,
    };
    Some((mesh, agreement, payload))
}

fn transform_vector(m: &Mat4, v: Vec3) -> Vec3 {
    let r = *m * glam::Vec4::new(v.x, v.y, v.z, 0.0);
    Vec3::new(r.x, r.y, r.z)
}

fn transform_point_local(m: &Mat4, p: Vec3) -> Vec3 {
    let r = *m * glam::Vec4::new(p.x, p.y, p.z, 1.0);
    Vec3::new(r.x, r.y, r.z)
}

/// `IfcHalfSpaceSolid(BaseSurface: IfcSurface, AgreementFlag: BOOL)` —
/// the base surface is typically `IfcPlane(Position: IfcAxis2Placement3D)`.
/// We emit a square thin slab on the AgreementFlag side of the base
/// plane (`HALFSPACE_PLANE_EXTENT × HALFSPACE_PLANE_EXTENT` lateral,
/// `HALFSPACE_SLAB_THICKNESS * 0.01` deep — near-paper-thin) so the
/// consumer can see the cutting plane and which side is "inside" the
/// half-space. The half-space's actual subtraction effect is computed
/// in `cut_openings` via the `mesh::halfspace_clip` plane-clipping
/// primitive, NOT via CSG on this slab — see GH #39 for the rationale
/// (Manifold's batch boolean is fragile when a half-space cutter is
/// materialised as a finite box and stacked across a deep
/// IfcBooleanClippingResult tree).
pub fn halfspace_solid(table: &EntityTable, id: u64) -> Option<(LocalMesh, bool)> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCHALFSPACESOLID") {
        return None;
    }
    let fields = split_top_level_args(args);
    let surface_id = match fields.first().copied().map(parse_field) {
        Some(Field::Ref(sid)) => sid,
        _ => return None,
    };
    let agreement = parse_agreement_flag(fields.get(1).copied());
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
    let e = HALFSPACE_PLANE_EXTENT;
    let square = vec![
        Vec2::new(-e, -e),
        Vec2::new(e, -e),
        Vec2::new(e, e),
        Vec2::new(-e, e),
    ];
    let polygon = Polygon2D { outer: square, holes: Vec::new() };
    // De-facto IFC convention — see `polygonal_bounded_halfspace` above
    // for the citation chain (ifcopenshell `!AgreementFlag` flip + OCCT
    // `BRepPrimAPI_MakeHalfSpace`). Net mapping:
    //   * `.T.` → keep +position.Z side (slab on -position.Z, rotated).
    //   * `.F.` → keep -position.Z side (slab on +position.Z, no rotation).
    // `halfspace_clip` keeps the negative side of the slab-top normal.
    let frame = if agreement {
        position * Mat4::from_rotation_y(std::f32::consts::PI)
    } else {
        position
    };
    let mesh = extrude_polygon(&polygon, Vec3::Z, HALFSPACE_SLAB_THICKNESS * 0.01, frame);
    Some((mesh, agreement))
}

/// Annotate a fragment with its structural position inside the current
/// composite — the boolean operand role for `IfcBooleanResult` /
/// `IfcBooleanClippingResult`, or the `csg_branch` marker for an
/// `IfcCsgSolid` subtree. Roles accumulate: each retag call pushes
/// `new_role` onto the existing chain (innermost-first), so a
/// fragment that wraps through N levels of composite carries N roles
/// plus its leaf `source`. Serialisation reverses the vec so the chain
/// reads outermost-first.
///
/// Pre-W1 ([GH #58]) this function returned `role.unwrap_or(new_role)`
/// against a single `Option<&'static str>`, which silently dropped the
/// outer role whenever an inner one was already set. A nested
/// `IfcBooleanResult(host=wall, cutter=IfcBooleanResult(host=door,
/// cutter=handle))` would lose the outer-cutter annotation on the
/// door fragment, causing `cut_openings::is_cutter` to mis-classify
/// it as a host segment and assemble it with the wall. Accumulating
/// the full chain fixes that: every wrapping role is preserved, and
/// readers see the structural truth at every level via
/// `cut_openings::chain_contains` / `chain_count`.
fn retag(frag: MeshFragment, new_role: &'static str) -> MeshFragment {
    match frag {
        MeshFragment::Mesh {
            mesh,
            source,
            mut roles,
            rep_step_id,
            instance_transform,
            bounded_halfspace,
        } => {
            roles.push(new_role);
            MeshFragment::Mesh {
                mesh,
                source,
                roles,
                rep_step_id,
                instance_transform,
                // Carry the W6 bounded-halfspace payload up the boolean
                // tree unchanged so it reaches the product.
                bounded_halfspace,
            }
        }
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
        let raw_pts = cartesian_point_list_2d(table, pts_id)?;
        // Evaluate IfcArcIndex / IfcLineIndex segments when present —
        // otherwise booleans on curved profiles collapse to polygonal
        // chords (GH #48).
        if let Some(Field::List(seg_body)) =
            fields.get(1).copied().map(parse_field)
        {
            if let Some(poly) =
                crate::mesh::indexed_curve::eval_segments_2d(&raw_pts, seg_body)
            {
                if poly.len() >= 3 {
                    return Some(poly);
                }
            }
        }
        if raw_pts.len() >= 3 {
            return Some(raw_pts);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// World normal of a mesh's FIRST triangle — exactly what
    /// `cut_openings::derive_plane_from_slab` reads off the half-space
    /// slab to build the clipping plane. The slab's first triangle is
    /// its top cap (CCW), so this normal is the direction the cut
    /// removes (the agreement direction).
    fn first_triangle_normal(mesh: &LocalMesh) -> Vec3 {
        assert!(mesh.indices.len() >= 3, "slab has no triangles");
        let idx = |k: usize| {
            let i = mesh.indices[k] as usize;
            Vec3::new(
                mesh.vertices[i * 3],
                mesh.vertices[i * 3 + 1],
                mesh.vertices[i * 3 + 2],
            )
        };
        let (v0, v1, v2) = (idx(0), idx(1), idx(2));
        (v1 - v0).cross(v2 - v0).normalize()
    }

    /// GH #52: the half-space's cutting-plane normal must come from
    /// `BaseSurface.Position` (the IfcPlane's axis placement), NOT from
    /// the `IfcPolygonalBoundedHalfSpace.Position` polygon frame. When
    /// the two diverge, deriving the normal from the polygon frame
    /// clips the host against the wrong plane and empties it (the
    /// Sannergata `3_6AbaPP55…` regression).
    ///
    /// This fixture reproduces that divergence: BaseSurface.Axis is the
    /// tilted, nearly-horizontal `(-0.02, 0, -0.9998)` from the issue;
    /// the polygon Position uses the schema-default `(0, 0, 1)`. We
    /// assert the slab's first-triangle normal lands on the BaseSurface
    /// axis (up to the AgreementFlag sign), never on world +Z.
    const DIVERGENT_HALFSPACE_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('hs.ifc','2026-06-13T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
/* BaseSurface plane: tilted axis (-0.02, 0, -0.9998), refdir (-0.9998, 0, 0.02) */
#10=IFCCARTESIANPOINT((0.,0.,0.));
#11=IFCDIRECTION((-0.02,0.,-0.9998));
#12=IFCDIRECTION((-0.9998,0.,0.02));
#13=IFCAXIS2PLACEMENT3D(#10,#11,#12);
#14=IFCPLANE(#13);
/* Polygon Position: schema-default frame (Axis=$ -> (0,0,1), RefDir=$ -> (1,0,0)) */
#20=IFCCARTESIANPOINT((0.,0.,0.));
#21=IFCAXIS2PLACEMENT3D(#20,$,$);
/* PolygonalBoundary: 2D rectangle in the polygon frame */
#30=IFCCARTESIANPOINT((0.,0.));
#31=IFCCARTESIANPOINT((6626.,0.));
#32=IFCCARTESIANPOINT((6626.,100.));
#33=IFCCARTESIANPOINT((0.,100.));
#34=IFCPOLYLINE((#30,#31,#32,#33,#30));
#40=IFCPOLYGONALBOUNDEDHALFSPACE(#14,.T.,#21,#34);
ENDSEC;
END-ISO-10303-21;
"#;

    #[test]
    fn polygonal_bounded_halfspace_normal_from_base_surface() {
        let table = EntityTable::build(DIVERGENT_HALFSPACE_IFC.as_bytes());
        let (mesh, agreement, payload) =
            polygonal_bounded_halfspace(&table, 40).expect("halfspace #40 parses");
        assert!(agreement, "AgreementFlag .T. -> agreement = true");

        let base_axis = Vec3::new(-0.02, 0.0, -0.9998).normalize();
        let got = first_triangle_normal(&mesh);

        // The slab's first-triangle normal (the direction cut_openings
        // removes) must lie along the BaseSurface plane axis, NOT the
        // polygon frame's +Z. `.T.` builds the slab on the -axis side
        // (Y-180° rotation) so the cut keeps the +axis side — the
        // first-triangle normal points along -base_axis.
        let align_base = got.dot(base_axis).abs();
        assert!(
            align_base > 0.999,
            "slab normal {got:?} must align with BaseSurface axis \
             {base_axis:?} (|dot| = {align_base}), not the polygon frame"
        );

        // Guard against the pre-#52 bug: if the normal had come from the
        // polygon's default frame it would be ±world-Z, whose dot with
        // the near-horizontal base axis is ~0.9998 in Z but the X
        // component (-0.02) would be lost. Assert the X component is
        // actually present — proving we used the tilted BaseSurface
        // frame, not the axis-aligned polygon frame.
        assert!(
            got.x.abs() > 0.01,
            "slab normal {got:?} has no X tilt -> it came from the \
             polygon's (0,0,1) frame, not the tilted BaseSurface"
        );

        // The payload carries the same orientation for the W6 fast path.
        let align_payload = payload.plane_normal.dot(base_axis).abs();
        assert!(
            align_payload > 0.999,
            "payload plane_normal {:?} must also align with BaseSurface axis",
            payload.plane_normal
        );
    }

    #[test]
    fn second_operand_role_maps_operator() {
        // DIFFERENCE (and the clipping-result default) → cutter tag.
        assert_eq!(second_operand_role(Some(b".DIFFERENCE.")), "boolean_second_operand");
        // UNION / INTERSECTION get their own non-cutter tags.
        assert_eq!(second_operand_role(Some(b".UNION.")), "boolean_union_operand");
        assert_eq!(
            second_operand_role(Some(b".INTERSECTION.")),
            "boolean_intersection_operand"
        );
        // Missing / malformed operator falls back to DIFFERENCE — the
        // overwhelmingly common case, and the pre-W4 behaviour.
        assert_eq!(second_operand_role(None), "boolean_second_operand");
        assert_eq!(second_operand_role(Some(b"$")), "boolean_second_operand");
        assert_eq!(second_operand_role(Some(b".WAT.")), "boolean_second_operand");
    }
}
