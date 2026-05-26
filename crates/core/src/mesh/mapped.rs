//! `IfcMappedItem` → instance of a shared shape with a transform.
//!
//! Algorithm (Agent C spec):
//!   1. `src = MappingSource` (IfcRepresentationMap)
//!   2. `target = MappingTarget` (IfcCartesianTransformationOperator3D[NonUniform])
//!   3. `t_target` = transformation operator matrix
//!   4. `t_origin` = axis-placement matrix of `src.MappingOrigin`
//!   5. For each item in `src.MappedRepresentation.Items`, mesh it once
//!      (cached by step_id) and transform every vertex by
//!      `(t_target @ t_origin)`.
//!
//! Mapped items can nest, so the meshing recursion lives in
//! `super::mesh_item`; this module just resolves the references and the
//! composition matrix.

use std::collections::HashMap;

use glam::{Mat4, Vec3, Vec4};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;
use crate::mesh::placement::axis_placement_3d_from_id;
use crate::mesh::MeshFragment;

/// Resolve an `IfcMappedItem`, recursing into its source's items.
pub fn expand(
    table: &EntityTable,
    item_id: u64,
    shape_cache: &mut HashMap<u64, Vec<(LocalMesh, &'static str)>>,
) -> Vec<MeshFragment> {
    let (type_name, args) = match table.get(item_id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    if !type_name.eq_ignore_ascii_case(b"IFCMAPPEDITEM") {
        return Vec::new();
    }
    let fields = split_top_level_args(args);
    // IfcMappedItem(MappingSource, MappingTarget)
    let src_id = match parse_field(fields.first().unwrap_or(&&[][..])) {
        Field::Ref(id) => id,
        _ => return Vec::new(),
    };
    let target_id = match parse_field(fields.get(1).unwrap_or(&&[][..])) {
        Field::Ref(id) => Some(id),
        _ => None,
    };

    // src: IfcRepresentationMap(MappingOrigin, MappedRepresentation)
    let (src_type, src_args) = match table.get(src_id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    if !src_type.eq_ignore_ascii_case(b"IFCREPRESENTATIONMAP") {
        return Vec::new();
    }
    let src_fields = split_top_level_args(src_args);
    let origin_id = match parse_field(src_fields.first().unwrap_or(&&[][..])) {
        Field::Ref(id) => Some(id),
        _ => None,
    };
    let mapped_repr_id = match parse_field(src_fields.get(1).unwrap_or(&&[][..])) {
        Field::Ref(id) => id,
        _ => return Vec::new(),
    };

    let t_origin = origin_id
        .map(|id| axis_placement_3d_from_id(table, id))
        .unwrap_or(Mat4::IDENTITY);
    let t_target = target_id
        .map(|id| transformation_operator_matrix(table, id))
        .unwrap_or(Mat4::IDENTITY);
    let composed = t_target * t_origin;

    // Walk the source representation's Items (it's an IfcShapeRepresentation).
    // We pass the untransformed mesh through and attach `composed` as
    // the fragment's `instance_transform` — the streaming loop composes
    // it with the product's world placement when it bakes world-coord
    // vertices, AND the substrate writer sees the SAME local mesh bytes
    // across every instance of this mapping, which is the basis for
    // rep dedup in `instances.parquet` + `representations.parquet`.
    // Pre-instancing we used to call `transform_mesh(&mesh, composed)`
    // here, which baked the composition into the vertex stream and
    // erased the sharing.
    let item_ids = super::representation_items(table, mapped_repr_id);
    let mut out: Vec<MeshFragment> = Vec::new();
    for inner in item_ids {
        for frag in super::mesh_item(table, inner, shape_cache) {
            match frag {
                MeshFragment::Mesh { mesh, source: src, role, rep_step_id, instance_transform: inner_xform } => {
                    // Inner direct geometry contributes Mat4::IDENTITY;
                    // if a nested IfcMappedItem ever propagates here it
                    // brings its own composition that we multiply in
                    // (composed * inner) so deeper nests still produce
                    // a single per-instance transform.
                    out.push(MeshFragment::Mesh {
                        mesh,
                        source: if src == "extrusion" { "mapped" } else { src },
                        role,
                        rep_step_id,
                        instance_transform: composed * inner_xform,
                    });
                }
                u @ MeshFragment::Unhandled { .. } => {
                    // Pass through — the consumer needs to see what's
                    // inside the mapping that we couldn't tessellate.
                    out.push(u);
                }
            }
        }
    }
    out
}

fn transformation_operator_matrix(table: &EntityTable, id: u64) -> Mat4 {
    let (type_name, args) = match table.get(id) {
        Some(x) => x,
        None => return Mat4::IDENTITY,
    };
    let is_3d = type_name.eq_ignore_ascii_case(b"IFCCARTESIANTRANSFORMATIONOPERATOR3D");
    let is_3d_non = type_name.eq_ignore_ascii_case(b"IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM");
    if !is_3d && !is_3d_non {
        return Mat4::IDENTITY;
    }
    let fields = split_top_level_args(args);
    // IfcCartesianTransformationOperator3D
    //   arg[0] = Axis1, arg[1] = Axis2, arg[2] = LocalOrigin, arg[3] = Scale, arg[4] = Axis3
    // *3DnonUniform adds Scale2 (arg[5]) + Scale3 (arg[6]).
    let read_dir = |idx: usize| -> Option<Vec3> {
        let f = fields.get(idx).copied()?;
        match parse_field(f) {
            Field::Ref(did) => direction(table, did),
            _ => None,
        }
    };
    let read_pt = |idx: usize| -> Vec3 {
        fields
            .get(idx)
            .copied()
            .and_then(|f| match parse_field(f) {
                Field::Ref(pid) => cartesian_point(table, pid),
                _ => None,
            })
            .unwrap_or(Vec3::ZERO)
    };
    let read_num = |idx: usize, default: f32| -> f32 {
        fields
            .get(idx)
            .copied()
            .and_then(|f| match parse_field(f) {
                Field::Number(n) => Some(n as f32),
                _ => None,
            })
            .unwrap_or(default)
    };

    let axis1 = read_dir(0).unwrap_or(Vec3::X).normalize_or_zero();
    let axis2 = read_dir(1).unwrap_or(Vec3::Y).normalize_or_zero();
    let origin = read_pt(2);
    let scale = read_num(3, 1.0);
    let axis3 = read_dir(4).unwrap_or(axis1.cross(axis2)).normalize_or_zero();
    let (s1, s2, s3) = if is_3d_non {
        (scale, read_num(5, scale), read_num(6, scale))
    } else {
        (scale, scale, scale)
    };

    Mat4::from_cols(
        Vec4::new(axis1.x * s1, axis1.y * s1, axis1.z * s1, 0.0),
        Vec4::new(axis2.x * s2, axis2.y * s2, axis2.z * s2, 0.0),
        Vec4::new(axis3.x * s3, axis3.y * s3, axis3.z * s3, 0.0),
        Vec4::new(origin.x, origin.y, origin.z, 1.0),
    )
}

fn cartesian_point(table: &EntityTable, id: u64) -> Option<Vec3> {
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

fn direction(table: &EntityTable, id: u64) -> Option<Vec3> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCDIRECTION") {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let ratios: Vec<f32> = split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Number(n) => Some(n as f32),
            _ => None,
        })
        .collect();
    Some(Vec3::new(
        *ratios.first().unwrap_or(&0.0),
        *ratios.get(1).unwrap_or(&0.0),
        *ratios.get(2).unwrap_or(&0.0),
    ))
}
