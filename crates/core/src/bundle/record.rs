//! Per-product substrate records — the units the streaming sinks write.
//!
//! The substrate is two tables (not one row per product as in the
//! pre-instancing layout):
//!
//! - `RepresentationRecord` — one row per unique mesh shape, keyed by
//!   `rep_id`. Carries the actual `vertices_le` / `indices_le` blob
//!   plus its provenance segments. Written through `RepresentationSink`,
//!   which dedupes by `rep_id` (a 5000-window facade with one shared
//!   `IfcRepresentationMap` writes ONE rep row, not 5000).
//!
//! - `InstanceRecord` — one row per `IfcProduct`. Carries identity,
//!   semantic payload (psets, materials, quantities, classifications),
//!   the world transform that places its rep into world space, and a
//!   `rep_id` foreign key into the representations table.
//!
//! Built from a [`crate::mesh::ProductMesh`] paired with its
//! [`crate::bundle::ProductSemantics`]. The pairing chooses how to
//! assign `rep_id`:
//!
//! 1. Single-fragment product with a non-zero rep_step_id: dedup via
//!    `rep_id = parts[0].rep_step_id` (shared across instances pointing
//!    to the same `IfcRepresentationMap`).
//! 2. Multi-fragment product (boolean walls, multi-item representations):
//!    fall back to `rep_id = ifc_id` of the product itself — guarantees
//!    a unique rep row per product. Cross-product dedup of composites
//!    requires content hashing, which is queued as a follow-on (lands
//!    a further ~5-20% saving on top of the IfcMappedItem case).

use std::sync::Arc;

use crate::bundle::ProductSemantics;
use crate::mesh::qto::{self, MeshQto, PlanarSurface};
use crate::mesh::{MeshSegment, ProductMesh};

/// Provenance entry per mesh fragment, mirrored to the substrate row
/// so analysis queries can drill into "which triangles came from the
/// boolean's first operand vs the cut volume" without re-running the
/// mesher.
#[derive(Debug, Clone)]
pub struct SegmentRecord {
    pub source: String,
    pub index_start: u32,
    pub triangle_count: u32,
}

impl From<&MeshSegment> for SegmentRecord {
    fn from(seg: &MeshSegment) -> Self {
        Self {
            source: seg.source.clone(),
            index_start: seg.index_start,
            triangle_count: seg.index_count / 3,
        }
    }
}

/// Axis-aligned bounding box of a mesh in some coordinate frame.
#[derive(Debug, Clone, Copy)]
pub struct AaBb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl AaBb {
    pub fn from_vertices(vertices: &[f32]) -> Self {
        if vertices.is_empty() {
            return Self {
                min: [0.0; 3],
                max: [0.0; 3],
            };
        }
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for chunk in vertices.chunks_exact(3) {
            for k in 0..3 {
                if chunk[k] < min[k] {
                    min[k] = chunk[k];
                }
                if chunk[k] > max[k] {
                    max[k] = chunk[k];
                }
            }
        }
        Self { min, max }
    }
}

/// One representation — the unique geometry kernel output for one or
/// more instances. The substrate writer writes one row per unique
/// `rep_id` (subsequent instances pointing to the same rep_id skip the
/// write). The `local_*` geometry is what `instance.transform`
/// transforms into world space.
#[derive(Debug, Clone)]
pub struct RepresentationRecord {
    pub rep_id: u64,
    /// Whether the rep originated from a real `IfcRepresentationMap`
    /// dedup (single-fragment IfcMappedItem expansion) or is a fallback
    /// composite synthesised from a multi-fragment product. Lets
    /// downstream consumers distinguish "this rep is shared by N
    /// instances" from "this rep has exactly one referencing instance".
    pub source_kind: &'static str,
    /// Dominant mesh source tag from the first contributing fragment.
    pub mesh_source: String,
    pub vertex_count: u32,
    pub triangle_count: u32,
    /// Local-coordinate vertices — the shared geometry that every
    /// instance reuses. Held as raw LE bytes so the Parquet binary
    /// column write is a single memcpy.
    pub vertices_le: Vec<u8>,
    /// Local-coordinate indices (`u32` LE).
    pub indices_le: Vec<u8>,
    /// Segment provenance — `boolean_first_operand|extrusion` etc.
    /// Stays on the rep (not the instance) because the structural
    /// shape of the geometry IS a rep property, not a per-instance one.
    pub segments: Vec<SegmentRecord>,
    /// Local-frame AABB. Useful for spatial queries that join rep +
    /// instance and want geometry size without applying the transform.
    pub local_bbox_min_xyz: [f32; 3],
    pub local_bbox_max_xyz: [f32; 3],
}

/// One product instance — identity + semantics + a pointer (by `rep_id`)
/// into the representations table and the per-instance transform that
/// places that rep into world space.
#[derive(Debug, Clone)]
pub struct InstanceRecord {
    // Identity. `class` and `source_class` are interned `Arc<str>` —
    // shared with the bundle's `ProductCore` map, so a 100K-product
    // file's 50 distinct entity-class strings live as 50 Arc<str>
    // allocations, not 100K * 2 owned Strings.
    pub ifc_id: u64,
    pub guid: String,
    pub class: Arc<str>,
    pub source_class: Arc<str>,
    pub name: Option<String>,
    pub predefined_type: Option<String>,
    pub object_type: Option<String>,
    pub tag: Option<String>,

    // Spatial / structural relationships. storey_name + type_name are
    // interned (low-cardinality vocabularies); the GUID fields stay
    // owned `String` since GUIDs are unique per product.
    pub storey_guid: Option<String>,
    pub storey_name: Option<Arc<str>>,
    pub aggregates_parent_guid: Option<String>,
    pub type_guid: Option<String>,
    pub type_name: Option<Arc<str>>,

    // Geometry pointer + transform
    /// Foreign key into `representations.parquet`. `None` only if the
    /// product produced no meshable geometry (rare; still emitted so
    /// the instance row records its identity + semantics).
    pub rep_id: Option<u64>,
    /// Effective 4x4 column-major transform that maps the rep's local
    /// vertices into world space — `world * instance_transform`
    /// composed at the kernel layer.
    pub transform: [f32; 16],
    /// World-coord AABB (computed by the kernel before we throw the
    /// world-baked vertices away). Spatial queries hit this directly
    /// without applying the transform.
    pub bbox_min_xyz: [f32; 3],
    pub bbox_max_xyz: [f32; 3],
    /// World-space placement origin — the authoring tool's notion of
    /// "where this element is".
    pub placement_xyz: [f32; 3],

    // Semantic payload — unchanged from the pre-instancing layout
    pub materials: Vec<crate::bundle::MaterialEntry>,
    pub psets: Vec<crate::bundle::PsetValue>,
    pub quantities: Vec<crate::bundle::QuantityEntry>,
    pub classifications: Vec<crate::bundle::ClassificationEntry>,

    // Geometric QTO — computed in one pass over the world-coord mesh
    // during `pair_split`. Always in m² / m³ (the IFC project's
    // linear unit scale is applied at compute time). Authored
    // IfcElementQuantity values, when present, live in `quantities`
    // and override these; these are the geometric truth that survives
    // when the author forgot to export Qto_* sets.
    pub volume_m3: f32,
    pub aabb_volume_m3: f32,
    pub surface_area_m2: f32,
    pub area_top_m2: f32,
    pub area_bottom_m2: f32,
    pub area_side_m2: f32,
    pub area_inclined_m2: f32,
    pub largest_surface_m2: f32,
    pub smallest_surface_m2: f32,
    pub surface_count: u32,
    /// Distinct planar surfaces sorted by area descending. DuckDB
    /// UNNEST gives one row per face — that's how "biggest /
    /// smallest surface on type X" turns into a single query.
    pub surfaces: Vec<PlanarSurface>,
    /// Validity classifier for `volume_m3` — one of `"closed"`,
    /// `"open_shell"`, `"degenerate"`. See `MeshQto::mesh_quality` for
    /// the full taxonomy. Consumers doing
    /// `SUM(volume_m3) WHERE class = 'Wall'` should filter on
    /// `mesh_quality = 'closed'` to avoid summing physically impossible
    /// open-shell volumes alongside valid figures (~9% of Duplex's
    /// products land in `open_shell`).
    pub mesh_quality: &'static str,
}

/// Compute (rep_id, kind) for a `ProductMesh` per the assignment rules
/// in the module docs.
fn pick_rep_id(mesh: &ProductMesh) -> (Option<u64>, &'static str) {
    if mesh.parts.is_empty() || mesh.indices.is_empty() {
        // No geometry — no rep. The instance row still gets written
        // with rep_id = None so semantics + identity aren't dropped.
        return (None, "none");
    }
    if mesh.parts.len() == 1 && mesh.parts[0].rep_step_id != 0 {
        // Single-fragment product → dedup by the rep's step_id. For
        // direct geometry, this is the inner item_id (unique per
        // representation, so no false sharing). For IfcMappedItem
        // expansion, this is the shared MappedRepresentation inner —
        // the basis for the 10-1000x compression on family-heavy files.
        return (Some(mesh.parts[0].rep_step_id), "shared_or_direct");
    }
    // Multi-fragment composite (boolean / multi-item representation) —
    // fall back to the product's own step_id. Guaranteed unique →
    // never collides, never dedupes. A content-hash follow-on would
    // unlock cross-product composite sharing.
    (Some(mesh.ifc_id), "composite")
}

/// Build a `(rep, instance)` pair from a `ProductMesh` + its semantics.
/// Returns `(maybe_rep, instance)` — the caller's sink decides whether
/// `maybe_rep` is new (write a row) or already-seen (skip).
///
/// `unit_scale` is the IFC project's linear-unit-to-metres factor
/// (0.001 for millimetre files, 1.0 for metre files). The QTO pass
/// scales area by `unit_scale²` and volume by `unit_scale³` so the
/// substrate is always in m² / m³ regardless of source units.
pub fn pair_split(
    mesh: ProductMesh,
    semantics: ProductSemantics,
    unit_scale: f32,
) -> (Option<RepresentationRecord>, InstanceRecord) {
    let (rep_id_opt, source_kind) = pick_rep_id(&mesh);

    // Bbox of the world-baked vertices — what spatial queries want.
    let world_bbox = AaBb::from_vertices(&mesh.vertices);

    // Geometric QTO — one O(triangles) sweep over the same vertices
    // we just bbox'd. Runs before the rep-record build so the same
    // cache-warm vertex buffer feeds both. Sub-ms on typical
    // products, ~30ms on the biggest individual meshes.
    let qto: MeshQto = qto::compute(&mesh.vertices, &mesh.indices, unit_scale);

    // Build the representation record. For single-fragment products
    // the rep carries the LOCAL (untransformed) mesh from
    // `parts[0].local_*`. For multi-fragment composites we bake the
    // world geometry into the rep (transform = identity on the
    // instance) — same shape information, just not shared.
    let rep = rep_id_opt.map(|rep_id| {
        if source_kind == "composite" {
            // Composite: rep is the world-baked geometry; transform
            // applied on the instance side is identity. Lossy on
            // dedup, lossless on geometry + segments.
            let segments: Vec<SegmentRecord> =
                mesh.segments.iter().map(SegmentRecord::from).collect();
            let mut vertices_le = Vec::with_capacity(mesh.vertices.len() * 4);
            for v in &mesh.vertices {
                vertices_le.extend_from_slice(&v.to_le_bytes());
            }
            let mut indices_le = Vec::with_capacity(mesh.indices.len() * 4);
            for i in &mesh.indices {
                indices_le.extend_from_slice(&i.to_le_bytes());
            }
            RepresentationRecord {
                rep_id,
                source_kind: "composite",
                mesh_source: mesh.source.to_string(),
                vertex_count: (mesh.vertices.len() / 3) as u32,
                triangle_count: (mesh.indices.len() / 3) as u32,
                vertices_le,
                indices_le,
                segments,
                local_bbox_min_xyz: world_bbox.min,
                local_bbox_max_xyz: world_bbox.max,
            }
        } else {
            // Single-fragment: rep is the LOCAL mesh from parts[0].
            // This is what dedupes across instances.
            let part = &mesh.parts[0];
            let local_bbox = AaBb::from_vertices(&part.local_vertices);
            let mut vertices_le = Vec::with_capacity(part.local_vertices.len() * 4);
            for v in &part.local_vertices {
                vertices_le.extend_from_slice(&v.to_le_bytes());
            }
            let mut indices_le = Vec::with_capacity(part.local_indices.len() * 4);
            for i in &part.local_indices {
                indices_le.extend_from_slice(&i.to_le_bytes());
            }
            // Segments mirror the single part — keep the compound
            // source tag so reveal-all consumers can still drill in.
            let segments = vec![SegmentRecord {
                source: part.source.clone(),
                index_start: 0,
                triangle_count: (part.local_indices.len() / 3) as u32,
            }];
            RepresentationRecord {
                rep_id,
                source_kind,
                mesh_source: mesh.source.to_string(),
                vertex_count: (part.local_vertices.len() / 3) as u32,
                triangle_count: (part.local_indices.len() / 3) as u32,
                vertices_le,
                indices_le,
                segments,
                local_bbox_min_xyz: local_bbox.min,
                local_bbox_max_xyz: local_bbox.max,
            }
        }
    });

    // Instance transform: for single-fragment products it's
    // `world * instance_transform` (where instance_transform is the
    // IfcMappedItem composition). For composites we baked world into
    // the rep, so the instance transform is identity.
    let transform: [f32; 16] = if source_kind == "composite" || mesh.parts.is_empty() {
        identity_mat4_cols()
    } else {
        compose_world_with_part(mesh.world_transform, mesh.parts[0].instance_transform)
    };

    let instance = InstanceRecord {
        // The Bundle's semantics map is keyed on the indexer's PRODUCT
        // dispatch — entities that the indexer routes to other kinds
        // (e.g. IfcSpace → `EntityKind::Space`) won't have a populated
        // `semantics.ifc_id`. Fall back to the mesh's `ifc_id`, which
        // is the STEP id captured directly from the entity-table walk
        // and is always correct. Mirrors the `class` fallback below.
        ifc_id: if semantics.ifc_id == 0 {
            mesh.ifc_id
        } else {
            semantics.ifc_id
        },
        guid: mesh.guid,
        class: if semantics.class.is_empty() {
            // Bundle didn't have a normalized class for this product
            // (proxy element / unknown schema); synth one from the raw
            // entity. One-off allocation per fallback hit — fine.
            Arc::from(strip_ifc_prefix(&mesh.entity).as_str())
        } else {
            semantics.class
        },
        source_class: if semantics.source_class.is_empty() {
            Arc::from(mesh.entity.as_str())
        } else {
            semantics.source_class
        },
        name: semantics.name,
        predefined_type: semantics.predefined_type,
        object_type: semantics.object_type,
        tag: semantics.tag,
        storey_guid: semantics.storey_guid,
        storey_name: semantics.storey_name,
        aggregates_parent_guid: semantics.aggregates_parent_guid,
        type_guid: semantics.type_guid,
        type_name: semantics.type_name,
        rep_id: rep_id_opt,
        transform,
        bbox_min_xyz: world_bbox.min,
        bbox_max_xyz: world_bbox.max,
        placement_xyz: mesh.placement_origin,
        materials: semantics.materials,
        psets: semantics.psets,
        quantities: semantics.quantities,
        classifications: semantics.classifications,
        volume_m3: qto.volume_m3.abs(),
        aabb_volume_m3: qto.aabb_volume_m3,
        surface_area_m2: qto.surface_area_m2,
        area_top_m2: qto.area_top_m2,
        area_bottom_m2: qto.area_bottom_m2,
        area_side_m2: qto.area_side_m2,
        area_inclined_m2: qto.area_inclined_m2,
        largest_surface_m2: qto.largest_surface_m2,
        smallest_surface_m2: qto.smallest_surface_m2,
        surface_count: qto.surface_count,
        surfaces: qto.surfaces,
        mesh_quality: qto.mesh_quality,
    };

    (rep, instance)
}

fn identity_mat4_cols() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]
}

/// Multiply two column-major 4x4s — used to compose
/// `world * instance_transform` into the single effective transform
/// we write on the instance row.
fn compose_world_with_part(world: [f32; 16], part: [f32; 16]) -> [f32; 16] {
    // glam Mat4 is col-major; ProductMesh and InstancePart both emit
    // `to_cols_array()`. Use glam to compose so the multiply matches
    // the kernel's bake (`effective = world * instance_transform`).
    let w = glam::Mat4::from_cols_array(&world);
    let p = glam::Mat4::from_cols_array(&part);
    (w * p).to_cols_array()
}

fn strip_ifc_prefix(s: &str) -> String {
    s.strip_prefix("Ifc")
        .or_else(|| s.strip_prefix("IFC"))
        .unwrap_or(s)
        .to_string()
}
