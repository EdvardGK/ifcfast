//! `IfcExtrudedAreaSolid` → triangle mesh.
//!
//! Per Agent C's port spec:
//!   1. Resolve `SweptArea` (IfcProfileDef) → 2D polygon (outer + holes)
//!   2. Resolve `ExtrudedDirection` → unit Vec3 (defaults to (0, 0, 1))
//!   3. Read `Depth` → f32
//!   4. Resolve `Position` (IfcAxis2Placement3D) → 4×4 local transform
//!   5. Triangulate the profile with `i_triangle` (holes-aware)
//!   6. Emit bottom-cap (CW, normal -dir) + top-cap (CCW, normal +dir)
//!      + side strip (one quad per outer edge, two per hole edge)
//!   7. Apply `Position` transform to every vertex
//!
//! Output is in the solid's body-local frame. The caller multiplies by
//! the product's `ObjectPlacement` world matrix to position the mesh.

use glam::{Mat4, Vec3};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::placement::axis_placement_3d_from_id;
use crate::mesh::profile::{self, Polygon2D};

/// Triangle-list mesh in local 3D coordinates.
#[derive(Debug, Clone, Default)]
pub struct LocalMesh {
    pub vertices: Vec<f32>, // [x, y, z, ...]
    pub indices: Vec<u32>,  // triangle indices into vertices
}

impl LocalMesh {
    pub fn new() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
        }
    }

    /// Append vertices, returning the starting index for the appended block.
    fn push_vertices(&mut self, verts: &[Vec3]) -> u32 {
        let start = (self.vertices.len() / 3) as u32;
        for v in verts {
            self.vertices.push(v.x);
            self.vertices.push(v.y);
            self.vertices.push(v.z);
        }
        start
    }

    fn push_triangle(&mut self, a: u32, b: u32, c: u32) {
        self.indices.push(a);
        self.indices.push(b);
        self.indices.push(c);
    }
}

/// Build a mesh for an `IfcExtrudedAreaSolid` entity (by step_id).
pub fn extrude(table: &EntityTable, id: u64) -> Option<LocalMesh> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCEXTRUDEDAREASOLID") {
        return None;
    }
    let fields = split_top_level_args(args);
    // IfcExtrudedAreaSolid(SweptArea, Position, ExtrudedDirection, Depth)
    let swept_id = match parse_field(fields.first()?) {
        Field::Ref(id) => id,
        _ => return None,
    };
    let polygon = profile::extract(table, swept_id)?;

    // ExtrudedDirection (arg[2]), default (0, 0, 1).
    let dir = match fields
        .get(2)
        .copied()
        .map(parse_field)
        .unwrap_or(Field::Null)
    {
        Field::Ref(did) => direction(table, did).unwrap_or(Vec3::Z),
        _ => Vec3::Z,
    };
    let dir = dir.normalize_or_zero();
    let dir = if dir.length_squared() < 1e-12 { Vec3::Z } else { dir };

    // Depth (arg[3]).
    let depth = match parse_field(*fields.get(3)?) {
        Field::Number(n) => n as f32,
        _ => return None,
    };

    // Position (arg[1]) — solid-local axis placement; optional.
    let local_xform = fields
        .get(1)
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(pid) => Some(axis_placement_3d_from_id(table, pid)),
            _ => None,
        })
        .unwrap_or(Mat4::IDENTITY);

    Some(extrude_polygon(&polygon, dir, depth, local_xform))
}

/// Build mesh from an already-resolved 2D polygon + extrusion params.
/// Exposed so tests can call it without an IFC file.
pub fn extrude_polygon(polygon: &Polygon2D, dir: Vec3, depth: f32, local_xform: Mat4) -> LocalMesh {
    let mut mesh = LocalMesh::new();

    // 1. Triangulate the profile (i_triangle: holes-aware, robust).
    let tris = triangulate(polygon);

    // 2. Build vertex lists for bottom (z=0) and top (z=depth*dir) caps.
    let cap_xy: Vec<glam::Vec2> = polygon
        .outer
        .iter()
        .copied()
        .chain(polygon.holes.iter().flat_map(|h| h.iter().copied()))
        .collect();

    let bottom: Vec<Vec3> = cap_xy
        .iter()
        .map(|p| {
            let v = Vec3::new(p.x, p.y, 0.0);
            transform_point(&local_xform, v)
        })
        .collect();
    let top: Vec<Vec3> = cap_xy
        .iter()
        .map(|p| {
            let v = Vec3::new(p.x + dir.x * depth, p.y + dir.y * depth, dir.z * depth);
            transform_point(&local_xform, v)
        })
        .collect();

    let bot_base = mesh.push_vertices(&bottom);
    let top_base = mesh.push_vertices(&top);

    // 3. Cap triangles. Bottom flipped (CW so normal points -dir).
    for tri in tris.chunks_exact(3) {
        let (a, b, c) = (tri[0] as u32, tri[1] as u32, tri[2] as u32);
        // Top cap (CCW for +dir normal).
        mesh.push_triangle(top_base + a, top_base + b, top_base + c);
        // Bottom cap (flipped winding).
        mesh.push_triangle(bot_base + a, bot_base + c, bot_base + b);
    }

    // 4. Side strip — one quad per polygon edge (outer + each hole).
    //    Each quad = (bot[i], bot[i+1], top[i+1], top[i]).
    //    Indices into cap_xy use the same flat layout as bottom/top.
    let mut edge_loops: Vec<std::ops::Range<usize>> = Vec::new();
    edge_loops.push(0..polygon.outer.len());
    let mut offset = polygon.outer.len();
    for h in &polygon.holes {
        edge_loops.push(offset..offset + h.len());
        offset += h.len();
    }

    for loop_range in &edge_loops {
        let n = loop_range.end - loop_range.start;
        for i in 0..n {
            let i0 = loop_range.start + i;
            let i1 = loop_range.start + (i + 1) % n;
            let b0 = bot_base + i0 as u32;
            let b1 = bot_base + i1 as u32;
            let t0 = top_base + i0 as u32;
            let t1 = top_base + i1 as u32;
            // Quad as two tris (b0, b1, t1) + (b0, t1, t0)
            mesh.push_triangle(b0, b1, t1);
            mesh.push_triangle(b0, t1, t0);
        }
    }

    mesh
}

fn transform_point(m: &Mat4, p: Vec3) -> Vec3 {
    let r = *m * glam::Vec4::new(p.x, p.y, p.z, 1.0);
    Vec3::new(r.x, r.y, r.z)
}

fn direction(table: &EntityTable, id: u64) -> Option<Vec3> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCDIRECTION") {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(*fields.first()?) {
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
    let x = *ratios.first().unwrap_or(&0.0);
    let y = *ratios.get(1).unwrap_or(&0.0);
    let z = *ratios.get(2).unwrap_or(&0.0);
    Some(Vec3::new(x, y, z))
}

/// Triangulate a `Polygon2D` (outer + holes) into a flat triangle index
/// list. Uses `earcutr` — production-grade ear-clipping with hole support.
fn triangulate(polygon: &Polygon2D) -> Vec<usize> {
    // earcutr expects a flat [x, y, x, y, ...] coord array + a list of
    // hole start indices.
    let mut coords: Vec<f64> = Vec::with_capacity(2 * (polygon.outer.len() + polygon.holes.iter().map(|h| h.len()).sum::<usize>()));
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
