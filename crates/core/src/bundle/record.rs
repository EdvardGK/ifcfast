//! Per-product substrate record — the unit the streaming sink writes.
//!
//! Built by pairing one [`crate::mesh::ProductMesh`] (geometry +
//! provenance) with the matching [`crate::bundle::ProductSemantics`]
//! (psets, materials, storey, type, classifications) for the same
//! GUID. The record is what gets serialized to a Parquet row (or, in
//! the 3D lane, a glTF/USD prim with `extras`).

use crate::bundle::ProductSemantics;
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

/// Axis-aligned bounding box of a ProductMesh in world coordinates.
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

/// The full substrate row — every column the GeoParquet writer emits.
/// Owned data, so the source `ProductMesh` can be dropped immediately
/// after the sink calls `ProductRecord::pair(...)` (RAM bound = one
/// record at a time, not the model).
#[derive(Debug, Clone)]
pub struct ProductRecord {
    // Identity
    pub ifc_id: u64,
    pub guid: String,
    pub class: String,
    pub source_class: String,
    pub name: Option<String>,
    pub predefined_type: Option<String>,
    pub object_type: Option<String>,
    pub tag: Option<String>,

    // Spatial / structural relationships
    pub storey_guid: Option<String>,
    pub storey_name: Option<String>,
    pub aggregates_parent_guid: Option<String>,
    pub type_guid: Option<String>,
    pub type_name: Option<String>,

    // Geometry (world coordinates)
    pub placement_xyz: [f32; 3],
    pub bbox_min_xyz: [f32; 3],
    pub bbox_max_xyz: [f32; 3],
    pub vertex_count: u32,
    pub triangle_count: u32,
    /// Raw `f32` LE bytes — `vertex_count * 3 * 4` long. Held as bytes
    /// (not `Vec<f32>`) so the Parquet binary-column write is a single
    /// memcpy with no per-vertex marshalling overhead.
    pub vertices_le: Vec<u8>,
    /// Raw `u32` LE bytes — `triangle_count * 3 * 4` long.
    pub indices_le: Vec<u8>,
    pub mesh_source: String,
    pub segments: Vec<SegmentRecord>,

    // Semantic payload
    pub materials: Vec<crate::bundle::MaterialEntry>,
    pub psets: Vec<crate::bundle::PsetValue>,
    pub quantities: Vec<crate::bundle::QuantityEntry>,
    pub classifications: Vec<crate::bundle::ClassificationEntry>,
}

impl ProductRecord {
    /// Build a record by pairing a streamed `ProductMesh` with the
    /// semantics from the `Bundle`. Consumes the mesh (the vertex /
    /// index buffers move into the record's LE byte buffers so we
    /// avoid the second allocation a copy would force).
    pub fn pair(mesh: ProductMesh, semantics: ProductSemantics) -> Self {
        let bbox = AaBb::from_vertices(&mesh.vertices);
        let vertex_count = (mesh.vertices.len() / 3) as u32;
        let triangle_count = (mesh.indices.len() / 3) as u32;

        // f32 → 4 LE bytes per scalar. Same for u32. We do the encode
        // here rather than in the parquet sink so the streaming write
        // path stays a single `write_all(&bytes)` call.
        let mut vertices_le = Vec::with_capacity(mesh.vertices.len() * 4);
        for v in &mesh.vertices {
            vertices_le.extend_from_slice(&v.to_le_bytes());
        }
        let mut indices_le = Vec::with_capacity(mesh.indices.len() * 4);
        for i in &mesh.indices {
            indices_le.extend_from_slice(&i.to_le_bytes());
        }

        let segments = mesh.segments.iter().map(SegmentRecord::from).collect();

        Self {
            ifc_id: semantics.ifc_id,
            guid: mesh.guid,
            class: if semantics.class.is_empty() {
                // Fallback for products the indexer didn't classify —
                // strip Ifc prefix from the mesh's titlecase entity name
                // so we always emit *something* identifiable.
                strip_ifc_prefix(&mesh.entity)
            } else {
                semantics.class
            },
            source_class: if semantics.source_class.is_empty() {
                mesh.entity.clone()
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
            placement_xyz: mesh.placement_origin,
            bbox_min_xyz: bbox.min,
            bbox_max_xyz: bbox.max,
            vertex_count,
            triangle_count,
            vertices_le,
            indices_le,
            mesh_source: mesh.source.to_string(),
            segments,
            materials: semantics.materials,
            psets: semantics.psets,
            quantities: semantics.quantities,
            classifications: semantics.classifications,
        }
    }
}

fn strip_ifc_prefix(s: &str) -> String {
    s.strip_prefix("Ifc")
        .or_else(|| s.strip_prefix("IFC"))
        .unwrap_or(s)
        .to_string()
}
