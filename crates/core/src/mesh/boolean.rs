//! Reveal-all handlers for IFC composite / clipped solids.
//!
//! **Exception — half-space clips (GH #194).** A half-space second operand
//! of a DIFFERENCE (`IfcBooleanClippingResult`, or `IfcBooleanResult`
//! `.DIFFERENCE.`) is the element's own shape, not an opening:
//! [`boolean_result`] applies it to the first operand here, in every mode,
//! and emits only the clipped host (see `clip_first_operand`). Everything
//! below about emitting both operands applies to solid operands.
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

use glam::{DMat3, DMat4, DVec2, DVec3, Mat4, Vec2, Vec3};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::{extrude_polygon, LocalMesh};
use crate::mesh::placement::{axis_placement_3d_f64, axis_placement_3d_from_id};
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

    // GH #194: a half-space second operand of a DIFFERENCE is the
    // element's own shape, not an opening — clip the first operand here,
    // in every mode, so every consumer (mesh / meshes / QTO / substrate /
    // clash / drift / point cloud / glTF / wasm) inherits the clipped
    // solid. Chains recurse naturally: the inner clipping result arrives
    // already clipped as this level's first operand.
    if second_role == "boolean_second_operand" {
        if let Some(cut) = second_id.and_then(|sid| resolve_halfspace_operand(table, sid)) {
            return clip_first_operand(table, id, first_id, &cut, shape_cache, recurse);
        }
    }

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

/// Distance from the origin, in **metres**, beyond which a half-space
/// clip is evaluated on its f64 path (GH #210). Same threshold and the
/// same reason as the model-wide global shift in [`crate::mesh::rebase`]:
/// below 10 km an `f32` coordinate resolves ~1 mm or better, so the legacy
/// `f32` placement read is kept there and near-origin output stays
/// bit-identical. Beyond it the `f32` ulp grows to 0.5 m at 6.5e6 m, which
/// is what quantised the plane of a UTM-baked brep's clip.
const FAR_ORIGIN_M: f64 = 1.0e4;

/// `true` when any component of `v` (model units) lies beyond
/// [`FAR_ORIGIN_M`] once scaled to metres by `scale`.
fn is_far(v: DVec3, scale: f64) -> bool {
    v.abs().max_element() * scale > FAR_ORIGIN_M
}

/// A half-space cutting plane in f64 (GH #210): `point` on the plane and
/// the unit `normal` pointing into the REMOVED side, both in the boolean's
/// operand frame. Carried in f64 from the placement read to the frame
/// pull-back in `clip_fragment`, which narrows to `f32` only once the
/// plane is expressed in the fragment's near-origin local frame.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CutPlane {
    pub point: DVec3,
    pub normal: DVec3,
}

/// An `IfcPolygonalBoundedHalfSpace`, resolved for the clip (GH #194,
/// f64 since GH #210).
#[derive(Debug, Clone)]
pub(crate) struct BoundedCut {
    pub plane: CutPlane,
    /// Boundary polygon in its `Position` frame. On the far path the ring
    /// is rebased by its first vertex (subtracted in f64) so the `f32`
    /// points stay small; the offset is folded into `boundary_xform`.
    pub boundary: Polygon2D,
    /// Maps `boundary` points (`z = 0`) into the operand frame.
    pub boundary_xform: DMat4,
    /// Any input of this cut lies beyond [`FAR_ORIGIN_M`]: compose the
    /// boundary frame in f64 (`false` keeps the legacy `f32` compose, so
    /// near-origin output is bit-identical).
    pub far: bool,
}

/// A half-space second operand, resolved to what the clip needs (GH #194).
#[derive(Debug, Clone)]
pub(crate) enum HalfspaceCut {
    /// `IfcHalfSpaceSolid` / `IfcBoxedHalfSpace`: remove the side
    /// `normal` points into (the de-facto AgreementFlag convention, GH #39).
    Plane(CutPlane),
    /// `IfcPolygonalBoundedHalfSpace`: the plane intersected with the
    /// boundary polygon's column.
    Bounded(BoundedCut),
    /// A half-space entity we cannot evaluate (non-`IfcPlane` base
    /// surface, unreadable boundary curve). The host stays unclipped and
    /// is marked [`crate::mesh::CLIP_UNAPPLIED_TAG`] — never silently.
    Unresolvable,
}

/// `Some` when `id` is a half-space solid of any flavour; `None` for every
/// other operand (solids, nested booleans), which keep the reveal-all path.
pub(crate) fn resolve_halfspace_operand(table: &EntityTable, id: u64) -> Option<HalfspaceCut> {
    let (type_name, _) = table.get(id)?;
    if type_name.eq_ignore_ascii_case(b"IFCPOLYGONALBOUNDEDHALFSPACE") {
        return Some(match polygonal_bounded_cut(table, id) {
            Some(cut) => HalfspaceCut::Bounded(cut),
            None => HalfspaceCut::Unresolvable,
        });
    }
    if type_name.eq_ignore_ascii_case(b"IFCHALFSPACESOLID")
        || type_name.eq_ignore_ascii_case(b"IFCBOXEDHALFSPACE")
    {
        return Some(match halfspace_plane(table, id) {
            Some(plane) => HalfspaceCut::Plane(plane),
            None => HalfspaceCut::Unresolvable,
        });
    }
    None
}

/// What one clip did to one host fragment.
enum ClipOutcome {
    /// Nothing of the host lies in the removed region.
    Unchanged,
    /// Clipped (possibly to nothing — the host was consumed).
    Clipped(LocalMesh),
    /// Clipped by the Manifold fallback (`csg` builds only).
    #[cfg_attr(not(feature = "csg"), allow(dead_code))]
    Manifold(LocalMesh),
    /// Could not be evaluated; host returned unclipped and marked.
    Unapplied,
}

/// Mesh the first operand and apply the half-space `cut` to every host
/// fragment. The half-space itself is consumed (no stand-in slab is
/// emitted). Fragments that are revealed *cutters* nested inside the
/// first operand (`boolean_second_operand` somewhere in their chain) are
/// passed through untouched: they are not host material.
fn clip_first_operand(
    table: &EntityTable,
    id: u64,
    first_id: Option<u64>,
    cut: &HalfspaceCut,
    shape_cache: &super::ShapeCache,
    recurse: &dyn Fn(&EntityTable, u64, &super::ShapeCache) -> Vec<MeshFragment>,
) -> Vec<MeshFragment> {
    let mut out: Vec<MeshFragment> = Vec::new();
    let Some(fid) = first_id else {
        return out;
    };
    let scale = crate::mesh::profile::length_scale(table);
    let eps = crate::mesh::halfspace_clip::on_plane_eps(scale);
    for frag in recurse(table, fid, shape_cache) {
        let MeshFragment::Mesh {
            mesh,
            source,
            mut roles,
            rep_step_id,
            instance_transform,
            bounded_halfspace,
        } = frag
        else {
            out.push(frag);
            continue;
        };
        if roles.contains(&"boolean_second_operand") {
            roles.push("boolean_first_operand");
            out.push(MeshFragment::Mesh {
                mesh,
                source,
                roles,
                rep_step_id,
                instance_transform,
                bounded_halfspace,
            });
            continue;
        }
        let (mesh, rep_step_id) = match clip_fragment(&mesh, instance_transform, cut, eps, scale) {
            ClipOutcome::Unchanged => (mesh, rep_step_id),
            // The clipped shape is a function of THIS boolean node, not of
            // the leaf it came from: key it by the boolean's step id so the
            // substrate's rep dedup can never share it with an unclipped
            // use of the same leaf. (`styles` resolves the boolean id to the
            // leaf's colour when the boolean itself is unstyled.)
            ClipOutcome::Clipped(m) => {
                if m.indices.is_empty() {
                    continue; // host consumed by the clip
                }
                (m, id)
            }
            ClipOutcome::Manifold(m) => {
                roles.push(crate::mesh::CLIP_MANIFOLD_TAG);
                if m.indices.is_empty() {
                    continue;
                }
                (m, id)
            }
            ClipOutcome::Unapplied => {
                roles.push(crate::mesh::CLIP_UNAPPLIED_TAG);
                (mesh, rep_step_id)
            }
        };
        roles.push("boolean_first_operand");
        out.push(MeshFragment::Mesh {
            mesh,
            source,
            roles,
            rep_step_id,
            instance_transform,
            bounded_halfspace,
        });
    }
    out
}

/// Apply one half-space to one fragment. The cut is authored in the
/// boolean's operand frame; the fragment's vertices are local to
/// `instance_transform * translate(rep_origin)` (identity + the far-origin
/// rebase for facesets / breps), so the cut is mapped into that frame in
/// f64 first and narrowed to `f32` only there, where the numbers are small
/// (GH #210 — the `mesh::rebase` pattern). `clip_by_plane` /
/// `clip_plane_closed` / `clip_bounded` downstream therefore stay `f32`:
/// every coordinate they see is near the local origin.
///
/// `scale` is metres per model length unit, for the far-origin gate.
fn clip_fragment(
    mesh: &LocalMesh,
    instance_transform: Mat4,
    cut: &HalfspaceCut,
    eps: f32,
    scale: f32,
) -> ClipOutcome {
    if mesh.indices.len() < 3 {
        return ClipOutcome::Unchanged;
    }
    let fwd = instance_transform.as_dmat4()
        * DMat4::from_translation(DVec3::new(
            mesh.rep_origin[0],
            mesh.rep_origin[1],
            mesh.rep_origin[2],
        ));
    if fwd.determinant().abs() < 1.0e-18 {
        return ClipOutcome::Unapplied;
    }
    let inv = fwd.inverse();
    let scale = scale as f64;
    // Far path (GH #210): the cut, or the fragment's own frame, sits beyond
    // FAR_ORIGIN_M. Near-origin clips keep the exact legacy arithmetic.
    let far = is_far(fwd.w_axis.truncate(), scale)
        || match cut {
            HalfspaceCut::Plane(pl) => is_far(pl.point, scale),
            HalfspaceCut::Bounded(b) => b.far,
            HalfspaceCut::Unresolvable => false,
        };
    // On the far path the plane's authored origin can lie anywhere on the
    // plane — e.g. a horizontal clip anchored at the project origin while
    // the brep is baked 6.5e6 m away — so its local image is still huge and
    // narrowing it to f32 would re-quantise the plane. Slide it, in f64,
    // to the point of the plane closest to the fragment's AABB centre: the
    // same plane, expressed by a small number.
    let centre = if far {
        let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        for c in mesh.vertices.as_chunks::<3>().0 {
            let v = DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        Some((lo + hi) * 0.5)
    } else {
        None
    };
    // Normals map with the inverse-transpose of the point map (`inv`),
    // i.e. the transpose of `fwd`'s linear part.
    let lin_t = DMat3::from_mat4(fwd).transpose();
    let to_local = |pl: &CutPlane| -> (Vec3, Vec3) {
        let n = lin_t.mul_vec3(pl.normal).normalize_or_zero();
        let mut p = inv.transform_point3(pl.point);
        if let Some(c) = centre {
            if n.length_squared() > 0.5 {
                p = c + n * (p - c).dot(n);
            }
        }
        (p.as_vec3(), n.as_vec3())
    };
    match cut {
        HalfspaceCut::Unresolvable => ClipOutcome::Unapplied,
        HalfspaceCut::Plane(plane) => {
            let (p, n) = to_local(plane);
            if n.length_squared() < 0.5 {
                return ClipOutcome::Unapplied;
            }
            // Only material strictly past the on-plane band is removed: a
            // host face lying ON the plane (Revit anchors clip planes on
            // wall faces) merely touches the half-space — nothing to clip,
            // same rule as the bounded route.
            let removes_any = mesh
                .vertices
                .as_chunks::<3>()
                .0
                .iter()
                .any(|c| (Vec3::new(c[0], c[1], c[2]) - p).dot(n) > eps);
            if !removes_any {
                return ClipOutcome::Unchanged;
            }
            let (v, i) = crate::mesh::bounded_clip::clip_plane_closed(
                &mesh.vertices,
                &mesh.indices,
                p,
                n,
                eps,
            );
            ClipOutcome::Clipped(LocalMesh {
                vertices: v,
                indices: i,
                rep_origin: mesh.rep_origin,
            })
        }
        HalfspaceCut::Bounded(bounded) => {
            use crate::mesh::bounded_clip::{clip_bounded, BoundedClip};
            let (p, n) = to_local(&bounded.plane);
            // Far: compose in f64 and narrow the product, whose translation
            // is local (small). Near: the legacy f32 compose, bit-identical.
            let xform = if far {
                (inv * bounded.boundary_xform).as_mat4()
            } else {
                inv.as_mat4() * bounded.boundary_xform.as_mat4()
            };
            if n.length_squared() < 0.5 {
                return ClipOutcome::Unapplied;
            }
            match clip_bounded(
                &mesh.vertices,
                &mesh.indices,
                &bounded.boundary,
                xform,
                p,
                n,
                eps,
            ) {
                BoundedClip::Unchanged => ClipOutcome::Unchanged,
                BoundedClip::Clipped { vertices, indices } => ClipOutcome::Clipped(LocalMesh {
                    vertices,
                    indices,
                    rep_origin: mesh.rep_origin,
                }),
                BoundedClip::NonConvex | BoundedClip::Failed => {
                    manifold_bounded_fallback(mesh, &bounded.boundary, xform, p, n, eps)
                }
            }
        }
    }
}

/// Non-convex (or refused) bounded clip: `host − finite column cutter`
/// through Manifold when `csg` is compiled in; otherwise unapplied.
#[cfg(feature = "csg")]
fn manifold_bounded_fallback(
    mesh: &LocalMesh,
    boundary: &crate::mesh::profile::Polygon2D,
    xform: Mat4,
    p: Vec3,
    n: Vec3,
    eps: f32,
) -> ClipOutcome {
    let Some((cv, ci)) = crate::mesh::halfspace_clip::bounded_halfspace_cutter(
        &mesh.vertices,
        boundary,
        xform,
        p,
        n,
        eps,
    ) else {
        // Nothing of the host on the remove side / degenerate boundary.
        return ClipOutcome::Unchanged;
    };
    match crate::geom::csg::subtract(&mesh.vertices, &mesh.indices, &cv, &ci) {
        Ok((v, i)) => ClipOutcome::Manifold(LocalMesh {
            vertices: v,
            indices: i,
            rep_origin: mesh.rep_origin,
        }),
        Err(_) => ClipOutcome::Unapplied,
    }
}

#[cfg(not(feature = "csg"))]
fn manifold_bounded_fallback(
    _mesh: &LocalMesh,
    _boundary: &crate::mesh::profile::Polygon2D,
    _xform: Mat4,
    _p: Vec3,
    _n: Vec3,
    _eps: f32,
) -> ClipOutcome {
    ClipOutcome::Unapplied
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
    let parts = pbhs_parts(table, id)?;
    let outer = bounded_curve_raw(table, parts.boundary_id)?
        .finish(DVec2::ZERO, crate::mesh::profile::length_scale(table))?;
    if outer.len() < 3 {
        return None;
    }
    let polygon = Polygon2D {
        outer,
        holes: Vec::new(),
    };
    let (base_surface_position, boundary_position, frame) = pbhs_frames_f32(table, &parts);
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
    Some((mesh, parts.agreement, payload))
}

/// The raw references of an `IfcPolygonalBoundedHalfSpace`.
struct PbhsParts {
    agreement: bool,
    /// `BaseSurface.Position` when the base surface is an `IfcPlane` with
    /// a placement; `None` reads as identity (the pre-#210 behaviour).
    base_position: Option<u64>,
    /// `Position` (arg 2), the boundary polygon's frame.
    boundary_position: Option<u64>,
    boundary_id: u64,
}

fn pbhs_parts(table: &EntityTable, id: u64) -> Option<PbhsParts> {
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
    // BaseSurface must be an `IfcPlane`; its Position is read by the
    // caller (f32 for the slab / payload, f64 for the far clip). Anything
    // else reads as an identity frame, as before GH #210.
    let base_position = fields.first().copied().and_then(|f| match parse_field(f) {
        Field::Ref(sid) => {
            let (s_type, s_args) = table.get(sid)?;
            if !s_type.eq_ignore_ascii_case(b"IFCPLANE") {
                return None;
            }
            let s_fields = split_top_level_args(s_args);
            match s_fields.first().copied().map(parse_field) {
                Some(Field::Ref(pid)) => Some(pid),
                _ => None,
            }
        }
        _ => None,
    });
    // arg[2] = Position (IfcAxis2Placement3D) — the LOCAL frame the
    // PolygonalBoundary's 2D points live in. Independent from
    // BaseSurface.Position. W6 needs it to place the boundary polygon in
    // world for the bounded cut. Defaults to identity when absent.
    //
    // Read in ALL builds (GH #64 W6 default-path fix): the default
    // halfspace clip now honours the polygon bound too (it clips only the
    // boundary column, matching the ifcopenshell / Solibri kernel), so the
    // boundary frame must be resolved whether or not `prism-csg-fast` is
    // on. Previously this was gated and default builds carried an inert
    // identity xform, which silently dropped the boundary and over-cut.
    let boundary_position = fields.get(2).copied().and_then(|f| match parse_field(f) {
        Field::Ref(pid) => Some(pid),
        _ => None,
    });
    let boundary_id = match fields.get(3).copied().map(parse_field) {
        Some(Field::Ref(bid)) => bid,
        _ => return None,
    };
    Some(PbhsParts {
        agreement,
        base_position,
        boundary_position,
        boundary_id,
    })
}

/// The legacy `f32` frames of a polygonal bounded half-space:
/// `(BaseSurface.Position, boundary Position, slab frame)`.
///
/// Slab orientation follows the **de-facto** IFC convention — what
/// ifcopenshell, Revit and web-ifc all do, which is the OPPOSITE of
/// the literal reading of the IFC4 doc text for `AgreementFlag`. See
/// ifcopenshell `src/ifcgeom/mapping/IfcHalfSpaceSolid.cpp:33`:
///
/// ```text
/// f->orientation.reset(!inst->AgreementFlag());
/// ```
///
/// and the OCCT kernel at `kernels/opencascade/solid.cpp:47`:
///
/// ```text
/// pnt = pln.Location().Translated(orientation ? +axis : -axis);
/// halfspace = BRepPrimAPI_MakeHalfSpace(face, pnt);
/// ```
///
/// where `BRepPrimAPI_MakeHalfSpace(face, refPnt)` builds the half-
/// space CONTAINING `refPnt`. Net mapping:
///   * `.T.` (`agreement=true`)  → keep +position.Z side
///   * `.F.` (`agreement=false`) → keep -position.Z side
///
/// We build a thin one-sided slab whose top-cap normal lives on the
/// SUBTRACTED side (the side `halfspace_clip` is told to remove).
/// `halfspace_clip::clip_by_plane` keeps the **negative** side of
/// the normal it's given, so:
///   * `.T.` → slab built on -position.Z side (apply Y-180° rotation
///     so local +Z lands on world -position.Z) → clip keeps +position.Z.
///   * `.F.` → slab built on +position.Z side (no rotation) → clip
///     keeps -position.Z.
///
/// The Y-180° rotation has `det=+1`, so outward-facing windings are
/// preserved through the matrix.
/// The slab is built in **BaseSurface.Position**'s frame — its
/// local +Z is the cutting plane's normal direction (which is what
/// `cut_openings` needs). The polygon vertices were authored in the
/// arg[2].Position frame, so when that frame diverges from
/// BaseSurface.Position the slab's polygonal footprint will look
/// sheared/rotated in world — a visualisation cost, but cut_openings
/// only reads the first triangle's normal direction, so the cut is
/// still correct.
fn pbhs_frames_f32(table: &EntityTable, parts: &PbhsParts) -> (Mat4, Mat4, Mat4) {
    let base_surface_position = parts
        .base_position
        .map(|pid| axis_placement_3d_from_id(table, pid))
        .unwrap_or(Mat4::IDENTITY);
    let boundary_position = parts
        .boundary_position
        .map(|pid| axis_placement_3d_from_id(table, pid))
        .unwrap_or(Mat4::IDENTITY);
    let frame = if parts.agreement {
        base_surface_position * Mat4::from_rotation_y(std::f32::consts::PI)
    } else {
        base_surface_position
    };
    (base_surface_position, boundary_position, frame)
}

/// Resolve an `IfcPolygonalBoundedHalfSpace` for the clip (GH #194), in
/// f64 (GH #210).
///
/// Near the origin (every placement location and boundary vertex within
/// [`FAR_ORIGIN_M`]) the values are exactly the legacy `f32` ones widened,
/// so output is bit-identical. Beyond it, `BaseSurface.Position`, the
/// boundary `Position` and the boundary vertices are read in f64: the
/// plane keeps its authored origin to the digit instead of the 0.5 m
/// `f32` lattice at 6.5e6 m, and the ring is rebased by its first vertex
/// (in f64) so its `f32` points are small, the offset riding on
/// `boundary_xform`.
fn polygonal_bounded_cut(table: &EntityTable, id: u64) -> Option<BoundedCut> {
    let parts = pbhs_parts(table, id)?;
    let scale32 = crate::mesh::profile::length_scale(table);
    let scale = scale32 as f64;
    let raw = bounded_curve_raw(table, parts.boundary_id)?;
    let base64 = parts
        .base_position
        .map(|pid| axis_placement_3d_f64(table, pid));
    let boundary64 = parts
        .boundary_position
        .map(|pid| axis_placement_3d_f64(table, pid));
    let far = base64.is_some_and(|m| is_far(m.w_axis.truncate(), scale))
        || boundary64.is_some_and(|m| is_far(m.w_axis.truncate(), scale))
        || raw.max_abs() * scale > FAR_ORIGIN_M;

    if !far {
        let outer = raw.finish(DVec2::ZERO, scale32)?;
        if outer.len() < 3 {
            return None;
        }
        let (base, boundary, frame) = pbhs_frames_f32(table, &parts);
        let normal = transform_vector(&frame, Vec3::Z).normalize_or_zero();
        let point = transform_point_local(&base, Vec3::ZERO);
        return Some(BoundedCut {
            plane: CutPlane {
                point: point.as_dvec3(),
                normal: normal.as_dvec3(),
            },
            boundary: Polygon2D {
                outer,
                holes: Vec::new(),
            },
            boundary_xform: boundary.as_dmat4(),
            far: false,
        });
    }

    let offset = raw.first().unwrap_or(DVec2::ZERO);
    let outer = raw.finish(offset, scale32)?;
    if outer.len() < 3 {
        return None;
    }
    let base = base64.unwrap_or(DMat4::IDENTITY);
    let axis = base.z_axis.truncate().normalize_or_zero();
    Some(BoundedCut {
        plane: CutPlane {
            point: base.w_axis.truncate(),
            normal: if parts.agreement { -axis } else { axis },
        },
        boundary: Polygon2D {
            outer,
            holes: Vec::new(),
        },
        boundary_xform: boundary64.unwrap_or(DMat4::IDENTITY)
            * DMat4::from_translation(DVec3::new(offset.x, offset.y, 0.0)),
        far: true,
    })
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
    // `IfcBoxedHalfSpace` is an `IfcHalfSpaceSolid` whose `Enclosure` box
    // is a computational hint only — same geometry (GH #194).
    if !type_name.eq_ignore_ascii_case(b"IFCHALFSPACESOLID")
        && !type_name.eq_ignore_ascii_case(b"IFCBOXEDHALFSPACE")
    {
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
    let polygon = Polygon2D {
        outer: square,
        holes: Vec::new(),
    };
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

/// The cutting plane of an `IfcHalfSpaceSolid` / `IfcBoxedHalfSpace`:
/// `(point on plane, unit normal into the REMOVED side)`, in the operand
/// frame. Same orientation as the stand-in slab [`halfspace_solid`] builds
/// (its first-triangle normal) — `.T.` removes `-Position.Z`, `.F.`
/// removes `+Position.Z` (the de-facto convention, GH #39) — but read
/// exactly from `BaseSurface.Position` instead of re-derived from the
/// slab, whose centroid sits half a slab thickness off the plane.
/// `None` when the base surface is not an `IfcPlane` (GH #194 counts that
/// host as unclipped rather than guessing).
///
/// f64 since GH #210: within [`FAR_ORIGIN_M`] of the origin the plane is
/// the legacy `f32` read widened (bit-identical output); beyond it
/// `BaseSurface.Position` is read in f64, so a plane baked at
/// x = 6 500 003.37 m keeps its 0.37 instead of snapping to the 0.5 m
/// `f32` lattice.
pub(crate) fn halfspace_plane(table: &EntityTable, id: u64) -> Option<CutPlane> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCHALFSPACESOLID")
        && !type_name.eq_ignore_ascii_case(b"IFCBOXEDHALFSPACE")
    {
        return None;
    }
    let fields = split_top_level_args(args);
    let surface_id = match fields.first().copied().map(parse_field) {
        Some(Field::Ref(sid)) => sid,
        _ => return None,
    };
    let agreement = parse_agreement_flag(fields.get(1).copied());
    let (s_type, s_args) = table.get(surface_id)?;
    if !s_type.eq_ignore_ascii_case(b"IFCPLANE") {
        return None;
    }
    let s_fields = split_top_level_args(s_args);
    let position_id = s_fields
        .first()
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(pid) => Some(pid),
            _ => None,
        });
    if let Some(pos64) = position_id.map(|pid| axis_placement_3d_f64(table, pid)) {
        let scale = crate::mesh::profile::length_scale(table) as f64;
        if is_far(pos64.w_axis.truncate(), scale) {
            let axis = pos64.z_axis.truncate().normalize_or_zero();
            if axis.length_squared() < 0.5 {
                return None;
            }
            return Some(CutPlane {
                point: pos64.w_axis.truncate(),
                normal: if agreement { -axis } else { axis },
            });
        }
    }
    let position = position_id
        .map(|pid| axis_placement_3d_from_id(table, pid))
        .unwrap_or(Mat4::IDENTITY);
    let axis = transform_vector(&position, Vec3::Z).normalize_or_zero();
    if axis.length_squared() < 0.5 {
        return None;
    }
    let normal = if agreement { -axis } else { axis };
    Some(CutPlane {
        point: transform_point_local(&position, Vec3::ZERO).as_dvec3(),
        normal: normal.as_dvec3(),
    })
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

/// An `IfcBoundedCurve` boundary read in f64, before narrowing (GH #210).
/// Supports `IfcPolyline` (CartesianPoint list) and `IfcIndexedPolyCurve`
/// (point-list + segment indices).
enum BoundedCurveRaw<'a> {
    Polyline(Vec<DVec2>),
    Indexed {
        pts: Vec<DVec2>,
        segments: Option<&'a [u8]>,
    },
}

impl BoundedCurveRaw<'_> {
    fn points(&self) -> &[DVec2] {
        match self {
            BoundedCurveRaw::Polyline(p) => p,
            BoundedCurveRaw::Indexed { pts, .. } => pts,
        }
    }

    /// Largest absolute coordinate, model units — the far-origin gate.
    fn max_abs(&self) -> f64 {
        self.points()
            .iter()
            .map(|p| p.abs().max_element())
            .fold(0.0, f64::max)
    }

    fn first(&self) -> Option<DVec2> {
        self.points().first().copied()
    }

    /// The boundary as a planar `f32` polygon in the curve's local XY
    /// frame, **minus `offset`** (subtracted in f64 before the narrowing).
    /// `offset = 0` is the pre-#210 read exactly (`(n - 0.0) as f32 ==
    /// n as f32`). `IfcArcIndex` segments are evaluated on the rebased
    /// points — an arc is translation-invariant.
    fn finish(&self, offset: DVec2, unit_scale: f32) -> Option<Vec<Vec2>> {
        let narrow =
            |p: &[DVec2]| -> Vec<Vec2> { p.iter().map(|q| (*q - offset).as_vec2()).collect() };
        match self {
            BoundedCurveRaw::Polyline(pts) => {
                let mut out = narrow(pts);
                // IfcPolyline is explicit, often closed by repeating first
                // point; drop a duplicate trailing vertex if present.
                if out.len() >= 2 && (out[0] - out[out.len() - 1]).length_squared() < 1e-9 {
                    out.pop();
                }
                if out.len() >= 3 {
                    Some(out)
                } else {
                    None
                }
            }
            BoundedCurveRaw::Indexed { pts, segments } => {
                let raw_pts = narrow(pts);
                // Evaluate IfcArcIndex / IfcLineIndex segments when present —
                // otherwise booleans on curved profiles collapse to polygonal
                // chords (GH #48).
                if let Some(seg_body) = segments {
                    if let Some(poly) =
                        crate::mesh::indexed_curve::eval_segments_2d(&raw_pts, seg_body, unit_scale)
                    {
                        if poly.len() >= 3 {
                            return Some(poly);
                        }
                    }
                }
                if raw_pts.len() >= 3 {
                    Some(raw_pts)
                } else {
                    None
                }
            }
        }
    }
}

/// Read an `IfcBoundedCurve` boundary in f64 (see [`BoundedCurveRaw`]).
fn bounded_curve_raw<'a>(table: &'a EntityTable, id: u64) -> Option<BoundedCurveRaw<'a>> {
    let (type_name, args) = table.get(id)?;
    if type_name.eq_ignore_ascii_case(b"IFCPOLYLINE") {
        let fields = split_top_level_args(args);
        let body = match parse_field(fields.first()?) {
            Field::List(b) => b,
            _ => return None,
        };
        let pts = split_top_level_args(body)
            .into_iter()
            .filter_map(|f| match parse_field(f) {
                Field::Ref(pid) => cartesian_point_xy_f64(table, pid),
                _ => None,
            })
            .collect();
        return Some(BoundedCurveRaw::Polyline(pts));
    }
    if type_name.eq_ignore_ascii_case(b"IFCINDEXEDPOLYCURVE") {
        // IfcIndexedPolyCurve(Points: IfcCartesianPointList2D, Segments, SelfIntersect)
        let fields = split_top_level_args(args);
        let pts_id = match parse_field(fields.first()?) {
            Field::Ref(pid) => pid,
            _ => return None,
        };
        let pts = cartesian_point_list_2d_f64(table, pts_id)?;
        let segments = match fields.get(1).copied().map(parse_field) {
            Some(Field::List(seg_body)) => Some(seg_body),
            _ => None,
        };
        return Some(BoundedCurveRaw::Indexed { pts, segments });
    }
    None
}

fn cartesian_point_xy_f64(table: &EntityTable, id: u64) -> Option<DVec2> {
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
    Some(DVec2::new(
        *coords.first().unwrap_or(&0.0),
        *coords.get(1).unwrap_or(&0.0),
    ))
}

fn cartesian_point_list_2d_f64(table: &EntityTable, id: u64) -> Option<Vec<DVec2>> {
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
    let mut out: Vec<DVec2> = Vec::new();
    for f in split_top_level_args(body) {
        if let Field::List(inner) = parse_field(f) {
            let coords: Vec<f64> = split_top_level_args(inner)
                .into_iter()
                .filter_map(|g| match parse_field(g) {
                    Field::Number(n) => Some(n),
                    _ => None,
                })
                .collect();
            if coords.len() >= 2 {
                out.push(DVec2::new(coords[0], coords[1]));
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

    fn fixture_table(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures")
                .join(name),
        )
        .expect("fixture readable")
    }

    fn only_halfspace(table: &EntityTable) -> HalfspaceCut {
        let id = (1..400)
            .find(|&i| {
                table.get(i).is_some_and(|(t, _)| {
                    t.eq_ignore_ascii_case(b"IFCHALFSPACESOLID")
                        || t.eq_ignore_ascii_case(b"IFCPOLYGONALBOUNDEDHALFSPACE")
                })
            })
            .expect("fixture carries a half-space");
        resolve_halfspace_operand(table, id).expect("resolves")
    }

    /// GH #210: a plane baked at UTM magnitude is read to the digit —
    /// `f32` would put x = 6 500 003.37 on the 0.5 m lattice (…3.5).
    #[test]
    fn far_origin_plane_is_read_in_f64() {
        let buf = fixture_table("far_origin_clip_plane_210.ifc");
        let table = EntityTable::build(&buf);
        let HalfspaceCut::Plane(p) = only_halfspace(&table) else {
            panic!("expected a plane cut");
        };
        assert_eq!(p.point, DVec3::new(6_500_003.37, 1_200_000.0, 50.0));
        assert_eq!(p.normal, DVec3::X, ".F. removes +Position.Z");

        let buf = fixture_table("far_origin_clip_pbhs_210.ifc");
        let table = EntityTable::build(&buf);
        let HalfspaceCut::Bounded(b) = only_halfspace(&table) else {
            panic!("expected a bounded cut");
        };
        assert!(b.far);
        assert_eq!(b.plane.point, DVec3::new(6_500_003.37, 1_200_000.0, 50.0));
        // The ring is rebased by its first vertex; the offset rides on the
        // f64 boundary frame, so the boundary edge at local x = 0.13 (world
        // y = 1 200 000.13) survives: f32 would snap it to …0.125.
        let edge = b.boundary_xform.transform_point3(DVec3::new(
            b.boundary.outer[1].x as f64,
            b.boundary.outer[1].y as f64,
            0.0,
        ));
        assert!((edge.y - 1_200_000.13).abs() < 1e-6, "{edge:?}");
    }

    /// GH #210: on the far path the plane's authored origin may lie far
    /// from the host ALONG the plane (a clip anchored 1300 km away on the
    /// same plane). `clip_fragment` slides it, in f64, to the plane point
    /// nearest the host before narrowing; without that the local point is
    /// 1.3e6 m out and its `f32` image shifts the plane (measured: 1.55 m³
    /// kept instead of 1.5).
    #[test]
    fn far_plane_origin_is_slid_next_to_the_host() {
        // Unit box [0,1]^2 × [0,3], rebased from (6.5e6, 1.2e6, 50).
        let v: Vec<f32> = vec![
            0., 0., 0., 1., 0., 0., 1., 1., 0., 0., 1., 0., 0., 0., 3., 1., 0., 3., 1., 1., 3., 0.,
            1., 3.,
        ];
        let i: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 2, 3, 7, 2, 7, 6, 1, 2, 6, 1, 6,
            5, 0, 4, 7, 0, 7, 3,
        ];
        let mesh = LocalMesh {
            vertices: v,
            indices: i,
            rep_origin: [6_500_000.0, 1_200_000.0, 50.0],
        };
        // Oblique vertical plane through the box's centre column: any line
        // through a square's centre bisects it, so exactly 1.5 m³ is kept.
        // (An irrational-ish slope: with (1,1) or (1,2) the two f32
        // rounding errors happen to cancel along the normal.)
        let n = DVec3::new(1.0, 0.37, 0.0).normalize();
        let along = DVec3::new(n.y, -n.x, 0.0);
        let centre = DVec3::new(6_500_000.5, 1_200_000.5, 50.0);
        let cut = HalfspaceCut::Plane(CutPlane {
            point: centre + along * 1.3e6,
            normal: n,
        });
        let ClipOutcome::Clipped(out) = clip_fragment(&mesh, Mat4::IDENTITY, &cut, 1e-3, 1.0)
        else {
            panic!("the diagonal plane cuts the box");
        };
        let p = |k: u32| {
            let b = k as usize * 3;
            DVec3::new(
                out.vertices[b] as f64,
                out.vertices[b + 1] as f64,
                out.vertices[b + 2] as f64,
            )
        };
        let vol: f64 = out
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| p(t[0]).dot(p(t[1]).cross(p(t[2]))) / 6.0)
            .sum();
        assert!((vol - 1.5).abs() < 1e-5, "kept {vol} m³, want 1.5");
    }

    /// Near the origin the cut is the legacy `f32` read, widened — the
    /// guarantee behind bit-identical near-origin output.
    #[test]
    fn near_origin_plane_is_the_legacy_f32_read() {
        let buf = fixture_table("origin_clip_plane_210.ifc");
        let table = EntityTable::build(&buf);
        let HalfspaceCut::Plane(p) = only_halfspace(&table) else {
            panic!("expected a plane cut");
        };
        assert_eq!(p.point, DVec3::new(3.37_f32 as f64, 0.0, 50.0));

        let buf = fixture_table("origin_clip_pbhs_210.ifc");
        let table = EntityTable::build(&buf);
        let HalfspaceCut::Bounded(b) = only_halfspace(&table) else {
            panic!("expected a bounded cut");
        };
        assert!(!b.far);
        let id = (1..400)
            .find(|&i| {
                table
                    .get(i)
                    .is_some_and(|(t, _)| t.eq_ignore_ascii_case(b"IFCPOLYGONALBOUNDEDHALFSPACE"))
            })
            .unwrap();
        let (_, _, legacy) = polygonal_bounded_halfspace(&table, id).unwrap();
        assert_eq!(b.plane.point, legacy.plane_point.as_dvec3());
        assert_eq!(b.plane.normal, legacy.plane_normal.as_dvec3());
        assert_eq!(b.boundary_xform, legacy.boundary_xform.as_dmat4());
        assert_eq!(b.boundary.outer, legacy.boundary.outer);
    }

    #[test]
    fn second_operand_role_maps_operator() {
        // DIFFERENCE (and the clipping-result default) → cutter tag.
        assert_eq!(
            second_operand_role(Some(b".DIFFERENCE.")),
            "boolean_second_operand"
        );
        // UNION / INTERSECTION get their own non-cutter tags.
        assert_eq!(
            second_operand_role(Some(b".UNION.")),
            "boolean_union_operand"
        );
        assert_eq!(
            second_operand_role(Some(b".INTERSECTION.")),
            "boolean_intersection_operand"
        );
        // Missing / malformed operator falls back to DIFFERENCE — the
        // overwhelmingly common case, and the pre-W4 behaviour.
        assert_eq!(second_operand_role(None), "boolean_second_operand");
        assert_eq!(second_operand_role(Some(b"$")), "boolean_second_operand");
        assert_eq!(
            second_operand_role(Some(b".WAT.")),
            "boolean_second_operand"
        );
    }
}
