//! `IfcLocalPlacement` chain → world 4×4 matrix.
//!
//! Algorithm (from Agent C's port spec of `extents.py::_world_placement`):
//!
//! ```text
//! world(placement) =
//!     placement is None         -> Identity
//!     placement.PlacementRelTo  -> world(parent) @ axis_placement(placement.RelativePlacement)
//!     placement is root          -> axis_placement(placement.RelativePlacement)
//! ```
//!
//! `IfcAxis2Placement3D` carries `Location` (point), `Axis` (Z direction,
//! defaults to (0,0,1)), `RefDirection` (X, defaults to (1,0,0)). Y is
//! computed as `axis × ref_direction` after Gram-Schmidt orthogonalisation.

use std::collections::HashMap;

use glam::{DMat4, DVec3, DVec4, Mat4, Vec3, Vec4};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

/// Memoise resolved world matrices per `IfcLocalPlacement` step_id.
/// Placement chains share long tails (every product under a building
/// inherits the same `IfcLocalPlacement` for the building) so the cache
/// is worth ~10-100× on big files.
///
/// Resolved in **f64** (`DMat4`). The chain accumulates the large
/// georeferenced site/building translation, which f32 can't even
/// represent past ~16.7M (gaps of tens of mm at 1e8, metres at 1e9) —
/// so an f32 resolve quantises the world origin before any geometry is
/// placed. f64 keeps the origin exact; callers downcast the *rotation*
/// (small, f32-safe) for local-geometry math and keep the translation
/// in f64 for precise positioning / global-shift.
pub struct PlacementResolver<'a> {
    table: &'a EntityTable<'a>,
    cache: HashMap<u64, DMat4>,
}

impl<'a> PlacementResolver<'a> {
    pub fn new(table: &'a EntityTable<'a>) -> Self {
        Self {
            table,
            cache: HashMap::with_capacity(2048),
        }
    }

    /// Resolve an `IfcLocalPlacement` step_id to a world matrix (f64).
    pub fn world(&mut self, placement_id: u64) -> DMat4 {
        if let Some(&m) = self.cache.get(&placement_id) {
            return m;
        }
        let m = self.resolve(placement_id);
        self.cache.insert(placement_id, m);
        m
    }

    /// Consume the resolver and return its fully-warmed cache as a
    /// frozen `HashMap`. The cache is the only state worth preserving
    /// across the resolver's lifetime; the rest is back-references.
    /// Used by `mesh_ifc_streaming_framed`'s parallel phase 1 so the
    /// finalize step can index a shared `Arc<HashMap>` instead of
    /// taking `&mut PlacementResolver` across worker threads.
    pub fn into_cache(self) -> HashMap<u64, DMat4> {
        self.cache
    }

    fn resolve(&mut self, placement_id: u64) -> DMat4 {
        let (type_name, args) = match self.table.get(placement_id) {
            Some(x) => x,
            None => return DMat4::IDENTITY,
        };
        // Only IfcLocalPlacement uses (PlacementRelTo, RelativePlacement)
        // — but defensive: handle Ifc{Local,}Placement subtypes by name.
        if !type_name.eq_ignore_ascii_case(b"IFCLOCALPLACEMENT")
            && !type_name.eq_ignore_ascii_case(b"IFCGRIDPLACEMENT")
        {
            return DMat4::IDENTITY;
        }
        let fields = split_top_level_args(args);
        // IfcLocalPlacement(PlacementRelTo, RelativePlacement)
        //   arg[0] = PlacementRelTo (OPTIONAL IfcObjectPlacement ref or $)
        //   arg[1] = RelativePlacement (IfcAxis2Placement3D ref)
        if fields.len() < 2 {
            return DMat4::IDENTITY;
        }
        let local_axis = match parse_field(fields[1]) {
            Field::Ref(id) => axis_placement_3d_f64(self.table, id),
            _ => DMat4::IDENTITY,
        };
        let parent = match parse_field(fields[0]) {
            Field::Ref(parent_id) => self.world(parent_id),
            _ => DMat4::IDENTITY,
        };
        parent * local_axis
    }
}

/// f64 build of an `IfcAxis2Placement3D`/`2D` — same algorithm as
/// [`axis_placement_3d_from_id`] but the Location is parsed in f64 so
/// the world-chain translation stays exact at georeferenced magnitudes.
/// Used only by [`PlacementResolver`]; local-geometry code keeps the
/// f32 builder.
pub fn axis_placement_3d_f64(table: &EntityTable, id: u64) -> DMat4 {
    let (type_name, args) = match table.get(id) {
        Some(x) => x,
        None => return DMat4::IDENTITY,
    };
    let is_3d = type_name.eq_ignore_ascii_case(b"IFCAXIS2PLACEMENT3D");
    let is_2d = type_name.eq_ignore_ascii_case(b"IFCAXIS2PLACEMENT2D");
    if !is_3d && !is_2d {
        return DMat4::IDENTITY;
    }
    let fields = split_top_level_args(args);
    let location = fields
        .first()
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(pid) => cartesian_point_f64(table, pid),
            _ => None,
        })
        .unwrap_or(DVec3::ZERO);

    let (axis_z, ref_x) = if is_3d {
        let z = fields.get(1).copied().and_then(|f| match parse_field(f) {
            Field::Ref(did) => direction_f64(table, did),
            _ => None,
        });
        let x = fields.get(2).copied().and_then(|f| match parse_field(f) {
            Field::Ref(did) => direction_f64(table, did),
            _ => None,
        });
        (z, x)
    } else {
        let x = fields
            .get(1)
            .copied()
            .and_then(|f| match parse_field(f) {
                Field::Ref(did) => direction_f64(table, did),
                _ => None,
            })
            .map(|d| DVec3::new(d.x, d.y, 0.0));
        (None, x)
    };

    let z = axis_z.unwrap_or(DVec3::Z).normalize_or_zero();
    let z = if z.length_squared() < 1e-12 { DVec3::Z } else { z };
    let mut x = ref_x.unwrap_or(DVec3::X);
    x = (x - z * x.dot(z)).normalize_or_zero();
    let x = if x.length_squared() < 1e-12 { DVec3::X } else { x };
    let y = z.cross(x);

    DMat4::from_cols(
        DVec4::new(x.x, x.y, x.z, 0.0),
        DVec4::new(y.x, y.y, y.z, 0.0),
        DVec4::new(z.x, z.y, z.z, 0.0),
        DVec4::new(location.x, location.y, location.z, 1.0),
    )
}

fn cartesian_point_f64(table: &EntityTable, id: u64) -> Option<DVec3> {
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

fn direction_f64(table: &EntityTable, id: u64) -> Option<DVec3> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCDIRECTION") {
        return None;
    }
    let fields = split_top_level_args(args);
    let body = match parse_field(fields.first()?) {
        Field::List(b) => b,
        _ => return None,
    };
    let ratios: Vec<f64> = split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Number(n) => Some(n),
            _ => None,
        })
        .collect();
    Some(DVec3::new(
        *ratios.first().unwrap_or(&0.0),
        *ratios.get(1).unwrap_or(&0.0),
        *ratios.get(2).unwrap_or(&0.0),
    ))
}

/// Build the 4×4 from an `IfcAxis2Placement3D` (or 2D) by step_id.
pub fn axis_placement_3d_from_id(table: &EntityTable, id: u64) -> Mat4 {
    let (type_name, args) = match table.get(id) {
        Some(x) => x,
        None => return Mat4::IDENTITY,
    };
    let is_3d = type_name.eq_ignore_ascii_case(b"IFCAXIS2PLACEMENT3D");
    let is_2d = type_name.eq_ignore_ascii_case(b"IFCAXIS2PLACEMENT2D");
    if !is_3d && !is_2d {
        return Mat4::IDENTITY;
    }
    let fields = split_top_level_args(args);
    // arg[0] = Location (IfcCartesianPoint ref)
    // arg[1] = Axis (Z direction, OPTIONAL — 3D only)
    // arg[2] = RefDirection (X direction, OPTIONAL — 3D), arg[1] for 2D
    let location = fields
        .first()
        .copied()
        .and_then(|f| match parse_field(f) {
            Field::Ref(pid) => cartesian_point(table, pid),
            _ => None,
        })
        .unwrap_or(Vec3::ZERO);

    let (axis_z, ref_x) = if is_3d {
        let z = fields
            .get(1)
            .copied()
            .and_then(|f| match parse_field(f) {
                Field::Ref(did) => direction(table, did),
                _ => None,
            });
        let x = fields
            .get(2)
            .copied()
            .and_then(|f| match parse_field(f) {
                Field::Ref(did) => direction(table, did),
                _ => None,
            });
        (z, x)
    } else {
        // 2D: only RefDirection at arg[1]; Z stays world-up.
        let x = fields
            .get(1)
            .copied()
            .and_then(|f| match parse_field(f) {
                Field::Ref(did) => direction(table, did),
                _ => None,
            })
            .map(|d| Vec3::new(d.x, d.y, 0.0));
        (None, x)
    };

    let z = axis_z.unwrap_or(Vec3::Z).normalize_or_zero();
    let z = if z.length_squared() < 1e-12 { Vec3::Z } else { z };
    let mut x = ref_x.unwrap_or(Vec3::X);
    x = (x - z * x.dot(z)).normalize_or_zero();
    let x = if x.length_squared() < 1e-12 { Vec3::X } else { x };
    let y = z.cross(x);

    Mat4::from_cols(
        Vec4::new(x.x, x.y, x.z, 0.0),
        Vec4::new(y.x, y.y, y.z, 0.0),
        Vec4::new(z.x, z.y, z.z, 0.0),
        Vec4::new(location.x, location.y, location.z, 1.0),
    )
}

fn cartesian_point(table: &EntityTable, id: u64) -> Option<Vec3> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCARTESIANPOINT") {
        return None;
    }
    let fields = split_top_level_args(args);
    // arg[0] = Coordinates (LIST of REAL)
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
    let x = *coords.first().unwrap_or(&0.0);
    let y = *coords.get(1).unwrap_or(&0.0);
    let z = *coords.get(2).unwrap_or(&0.0);
    Some(Vec3::new(x, y, z))
}

fn direction(table: &EntityTable, id: u64) -> Option<Vec3> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCDIRECTION") {
        return None;
    }
    let fields = split_top_level_args(args);
    // arg[0] = DirectionRatios (LIST of REAL)
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
    let x = *ratios.first().unwrap_or(&0.0);
    let y = *ratios.get(1).unwrap_or(&0.0);
    let z = *ratios.get(2).unwrap_or(&0.0);
    Some(Vec3::new(x, y, z))
}

/// Identity transform helper.
pub fn identity() -> Mat4 {
    Mat4::IDENTITY
}
