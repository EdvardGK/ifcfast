//! Native IFC → triangle mesh emitter.
//!
//! Feature-gated behind `mesh` so the default `ifcfast` builds (Python
//! extension, bench binary) don't pull in `earcutr` and `glam`.
//!
//! Phase 1A scope (this commit):
//!   * `IfcExtrudedAreaSolid` with parametric profiles (Rectangle,
//!     Circle, Ellipse, I/L/U/T/Z shapes) + arbitrary closed +
//!     arbitrary-with-voids.
//!   * `IfcLocalPlacement` recursive chain → world matrix.
//!   * `IfcShapeRepresentation` walk — body / facetation / reference
//!     contexts.
//!   * Output: Wavefront `.obj` for visual verification.
//!
//! Phase 1B / 2 / 3 add IfcMappedItem expansion, faceted/face-set BREP,
//! boolean clipping for openings, and Fragments binary serialisation.

pub mod brep;
pub mod extrusion;
pub mod faceset;
pub mod gltf;
pub mod mapped;
pub mod obj;
pub mod placement;
pub mod profile;
pub mod stats;

use std::collections::HashMap;
use std::time::Instant;

use glam::{Mat4, Vec3, Vec4};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;
use crate::mesh::placement::PlacementResolver;

/// A finished mesh in world coordinates, keyed back to its IfcProduct.
#[derive(Debug, Clone)]
pub struct ProductMesh {
    pub guid: String,
    pub entity: String,
    /// Flat `[x, y, z, x, y, z, ...]` vertex positions in model units (mm).
    pub vertices: Vec<f32>,
    /// Triangle indices into `vertices` (every 3 = one triangle).
    pub indices: Vec<u32>,
    /// Source representation tag — `"extrusion"`, `"mapped"`, `"faceset"`,
    /// `"brep"`, `"deferred"`.
    pub source: &'static str,
    /// World-space position of the product's IfcLocalPlacement origin —
    /// i.e. where the authoring tool thinks the element "is". Used by
    /// the drift analyser to detect placement-vs-geometry mismatches
    /// (a 50mm sensor whose mesh is 100m from its basepoint is an
    /// authoring bug).
    pub placement_origin: [f32; 3],
}

#[derive(Debug, Default, Clone)]
pub struct MeshStats {
    pub products_seen: usize,
    pub products_meshed: usize,
    pub products_deferred: usize,
    pub triangles: usize,
    pub by_source: HashMap<String, usize>,
    pub elapsed_ms: f64,
    pub entity_table_build_ms: f64,
}

/// Mesh every product in the IFC and return them keyed by GUID order.
pub fn mesh_ifc(buf: &[u8]) -> (Vec<ProductMesh>, MeshStats) {
    let mut stats = MeshStats::default();

    let t0 = Instant::now();
    let table = EntityTable::build(buf);
    stats.entity_table_build_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let _ = table.len();

    let t_mesh = Instant::now();
    let mut resolver = PlacementResolver::new(&table);
    let mut shape_cache: HashMap<u64, Vec<(LocalMesh, &'static str)>> = HashMap::new();
    let mut out: Vec<ProductMesh> = Vec::new();

    for (step_id, type_name, args) in table.iter() {
        // Skip anything we know isn't a product (rels, primitives, etc.).
        if !is_product_type(type_name) {
            continue;
        }
        stats.products_seen += 1;

        let fields = split_top_level_args(args);
        let guid = string_at(&fields, 0).unwrap_or_default();
        // IfcProduct: arg[5] = ObjectPlacement, arg[6] = Representation
        let placement_id = match fields.get(5).copied().map(parse_field) {
            Some(Field::Ref(id)) => Some(id),
            _ => None,
        };
        let repr_id = match fields.get(6).copied().map(parse_field) {
            Some(Field::Ref(id)) => Some(id),
            _ => None,
        };
        let repr_id = match repr_id {
            Some(id) => id,
            None => {
                stats.products_deferred += 1;
                *stats.by_source.entry("no_representation".into()).or_insert(0) += 1;
                continue;
            }
        };

        let world = placement_id
            .map(|pid| resolver.world(pid))
            .unwrap_or(Mat4::IDENTITY);

        // Find a body/facetation Items list.
        let items = body_items(&table, repr_id);
        if items.is_empty() {
            stats.products_deferred += 1;
            *stats.by_source.entry("no_body_items".into()).or_insert(0) += 1;
            continue;
        }

        // Mesh each item, union into the product mesh.
        let entity_name = type_name_titlecase(type_name);
        let mut combined_v: Vec<f32> = Vec::new();
        let mut combined_i: Vec<u32> = Vec::new();
        let mut source_tag: &'static str = "deferred";

        for item_id in items {
            let item_meshes = mesh_item(&table, item_id, &mut shape_cache);
            for (local, src) in item_meshes {
                let base = (combined_v.len() / 3) as u32;
                // Apply the product's world transform to every vertex.
                for chunk in local.vertices.chunks_exact(3) {
                    let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
                    let w = world * Vec4::new(p.x, p.y, p.z, 1.0);
                    combined_v.push(w.x);
                    combined_v.push(w.y);
                    combined_v.push(w.z);
                }
                for &idx in &local.indices {
                    combined_i.push(base + idx);
                }
                source_tag = src;
            }
        }

        if combined_i.is_empty() {
            stats.products_deferred += 1;
            *stats.by_source.entry("item_unhandled".into()).or_insert(0) += 1;
            continue;
        }

        // Capture placement origin in world space — what the authoring
        // tool considers the "location" of this product. Compare against
        // mesh centroid downstream to detect placement-vs-geometry drift.
        let placement_origin = {
            let p = world * Vec4::new(0.0, 0.0, 0.0, 1.0);
            [p.x, p.y, p.z]
        };

        stats.products_meshed += 1;
        stats.triangles += combined_i.len() / 3;
        *stats.by_source.entry(source_tag.to_string()).or_insert(0) += 1;
        out.push(ProductMesh {
            guid,
            entity: entity_name,
            vertices: combined_v,
            indices: combined_i,
            source: source_tag,
            placement_origin,
        });
        let _ = step_id;
    }

    stats.elapsed_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;
    (out, stats)
}

/// Mesh a single `IfcRepresentationItem` (or recurse via `IfcMappedItem`).
pub(crate) fn mesh_item(
    table: &EntityTable,
    item_id: u64,
    shape_cache: &mut HashMap<u64, Vec<(LocalMesh, &'static str)>>,
) -> Vec<(LocalMesh, &'static str)> {
    if let Some(cached) = shape_cache.get(&item_id) {
        return cached
            .iter()
            .map(|(m, s)| (clone_local(m), *s))
            .collect();
    }

    let (type_name, _args) = match table.get(item_id) {
        Some(x) => x,
        None => return Vec::new(),
    };

    let result: Vec<(LocalMesh, &'static str)> =
        if type_name.eq_ignore_ascii_case(b"IFCEXTRUDEDAREASOLID") {
            extrusion::extrude(table, item_id)
                .map(|m| vec![(m, "extrusion")])
                .unwrap_or_default()
        } else if type_name.eq_ignore_ascii_case(b"IFCMAPPEDITEM") {
            mapped::expand(table, item_id, shape_cache)
        } else if type_name.eq_ignore_ascii_case(b"IFCPOLYGONALFACESET") {
            faceset::polygonal_face_set(table, item_id)
                .map(|m| vec![(m, "polygonal_faceset")])
                .unwrap_or_default()
        } else if type_name.eq_ignore_ascii_case(b"IFCTRIANGULATEDFACESET") {
            faceset::triangulated_face_set(table, item_id)
                .map(|m| vec![(m, "triangulated_faceset")])
                .unwrap_or_default()
        } else if type_name.eq_ignore_ascii_case(b"IFCFACETEDBREP")
            || type_name.eq_ignore_ascii_case(b"IFCMANIFOLDSOLIDBREP")
        {
            brep::faceted_brep(table, item_id)
                .map(|m| vec![(m, "brep")])
                .unwrap_or_default()
        } else if type_name.eq_ignore_ascii_case(b"IFCFACEBASEDSURFACEMODEL") {
            brep::face_based_surface_model(table, item_id)
                .map(|m| vec![(m, "faceset_fbsm")])
                .unwrap_or_default()
        } else if type_name.eq_ignore_ascii_case(b"IFCSHELLBASEDSURFACEMODEL") {
            brep::shell_based_surface_model(table, item_id)
                .map(|m| vec![(m, "faceset_sbsm")])
                .unwrap_or_default()
        } else {
            // IfcBooleanClippingResult / advanced BREPs — Phase 2.
            Vec::new()
        };

    // Cache only the extrusion / direct-mesh case; mapped items recurse
    // via the cache on their inner shape.
    if !type_name.eq_ignore_ascii_case(b"IFCMAPPEDITEM") {
        shape_cache.insert(
            item_id,
            result.iter().map(|(m, s)| (clone_local(m), *s)).collect(),
        );
    }
    result
}

fn clone_local(m: &LocalMesh) -> LocalMesh {
    LocalMesh {
        vertices: m.vertices.clone(),
        indices: m.indices.clone(),
    }
}

/// Collect the top-level Items list from a representation, preferring
/// Body / Facetation contexts.
fn body_items(table: &EntityTable, repr_id: u64) -> Vec<u64> {
    let (type_name, args) = match table.get(repr_id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    // IfcProductDefinitionShape(Name, Description, Representations: LIST OF IfcRepresentation)
    if type_name.eq_ignore_ascii_case(b"IFCPRODUCTDEFINITIONSHAPE") {
        let fields = split_top_level_args(args);
        let body = match parse_field(*fields.get(2).unwrap_or(&&[][..])) {
            Field::List(b) => b,
            _ => return Vec::new(),
        };
        // Try every representation; prefer Body / Facetation context.
        let mut body_id: Option<u64> = None;
        let mut any_id: Option<u64> = None;
        for f in split_top_level_args(body) {
            if let Field::Ref(rid) = parse_field(f) {
                if is_body_or_facetation(table, rid) {
                    body_id = Some(rid);
                    break;
                }
                if any_id.is_none() {
                    any_id = Some(rid);
                }
            }
        }
        let chosen = body_id.or(any_id);
        return chosen.map(|id| representation_items(table, id)).unwrap_or_default();
    }
    // IfcShapeRepresentation directly (rare top-level).
    representation_items(table, repr_id)
}

fn is_body_or_facetation(table: &EntityTable, repr_id: u64) -> bool {
    let (type_name, args) = match table.get(repr_id) {
        Some(x) => x,
        None => return false,
    };
    if !type_name.eq_ignore_ascii_case(b"IFCSHAPEREPRESENTATION") {
        return false;
    }
    let fields = split_top_level_args(args);
    // IfcShapeRepresentation: (ContextOfItems, RepresentationIdentifier,
    //                          RepresentationType, Items)
    // RepresentationIdentifier at arg[1].
    let ident = match parse_field(*fields.get(1).unwrap_or(&&[][..])) {
        Field::String(s) => s.to_lowercase(),
        _ => return false,
    };
    matches!(ident.as_str(), "body" | "facetation")
}

pub(crate) fn representation_items(table: &EntityTable, repr_id: u64) -> Vec<u64> {
    let (type_name, args) = match table.get(repr_id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    if !type_name.eq_ignore_ascii_case(b"IFCSHAPEREPRESENTATION") {
        return Vec::new();
    }
    let fields = split_top_level_args(args);
    let items = match parse_field(*fields.get(3).unwrap_or(&&[][..])) {
        Field::List(b) => b,
        _ => return Vec::new(),
    };
    split_top_level_args(items)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Ref(id) => Some(id),
            _ => None,
        })
        .collect()
}

fn is_product_type(type_name: &[u8]) -> bool {
    // Reuse the indexer's PRODUCT_TYPES list — but exposing it cross-module
    // would require pub-marking it. For now, a cheap "starts with IFC and is
    // not in a known non-product set" check. We'll mesh anything that has a
    // Representation; the body_items walk skips entities without one.
    type_name.starts_with(b"IFC")
        && !matches!(
            type_name,
            b"IFCCARTESIANPOINT"
                | b"IFCDIRECTION"
                | b"IFCAXIS2PLACEMENT2D"
                | b"IFCAXIS2PLACEMENT3D"
                | b"IFCLOCALPLACEMENT"
                | b"IFCSHAPEREPRESENTATION"
                | b"IFCPRODUCTDEFINITIONSHAPE"
                | b"IFCREPRESENTATIONMAP"
                | b"IFCMAPPEDITEM"
                | b"IFCEXTRUDEDAREASOLID"
                | b"IFCRECTANGLEPROFILEDEF"
                | b"IFCROUNDEDRECTANGLEPROFILEDEF"
                | b"IFCCIRCLEPROFILEDEF"
                | b"IFCCIRCLEHOLLOWPROFILEDEF"
                | b"IFCELLIPSEPROFILEDEF"
                | b"IFCISHAPEPROFILEDEF"
                | b"IFCLSHAPEPROFILEDEF"
                | b"IFCUSHAPEPROFILEDEF"
                | b"IFCTSHAPEPROFILEDEF"
                | b"IFCZSHAPEPROFILEDEF"
                | b"IFCARBITRARYCLOSEDPROFILEDEF"
                | b"IFCARBITRARYPROFILEDEFWITHVOIDS"
                | b"IFCCOMPOSITEPROFILEDEF"
                | b"IFCPOLYLINE"
                | b"IFCINDEXEDPOLYCURVE"
                | b"IFCCARTESIANPOINTLIST2D"
                | b"IFCCARTESIANPOINTLIST3D"
                | b"IFCRELCONTAINEDINSPATIALSTRUCTURE"
                | b"IFCRELAGGREGATES"
                | b"IFCRELDEFINESBYPROPERTIES"
                | b"IFCRELDEFINESBYTYPE"
                | b"IFCRELASSOCIATESMATERIAL"
                | b"IFCRELASSOCIATESCLASSIFICATION"
                | b"IFCSIUNIT"
                | b"IFCUNITASSIGNMENT"
                | b"IFCOWNERHISTORY"
                | b"IFCAPPLICATION"
                | b"IFCPERSON"
                | b"IFCORGANIZATION"
                | b"IFCPERSONANDORGANIZATION"
                | b"IFCGEOMETRICREPRESENTATIONCONTEXT"
                | b"IFCGEOMETRICREPRESENTATIONSUBCONTEXT"
                | b"IFCPROJECT"
                | b"IFCSITE"
                | b"IFCBUILDING"
                | b"IFCBUILDINGSTOREY"
                | b"IFCMATERIAL"
                | b"IFCMATERIALLAYER"
                | b"IFCMATERIALLAYERSET"
                | b"IFCMATERIALLAYERSETUSAGE"
                | b"IFCPROPERTYSET"
                | b"IFCPROPERTYSINGLEVALUE"
                | b"IFCQUANTITYAREA"
                | b"IFCQUANTITYLENGTH"
                | b"IFCQUANTITYVOLUME"
                | b"IFCQUANTITYCOUNT"
                | b"IFCELEMENTQUANTITY"
                | b"IFCCARTESIANTRANSFORMATIONOPERATOR3D"
                | b"IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM"
        )
}

fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    let f = fields.get(idx)?;
    match parse_field(f) {
        Field::String(s) => Some(s),
        _ => None,
    }
}

fn type_name_titlecase(t: &[u8]) -> String {
    if t.len() < 3 || !t[..3].eq_ignore_ascii_case(b"IFC") {
        return std::str::from_utf8(t).unwrap_or("").to_string();
    }
    let mut s = String::with_capacity(t.len());
    s.push('I');
    s.push('f');
    s.push('c');
    let mut upper_next = true;
    for &c in &t[3..] {
        let ch = c as char;
        if upper_next {
            s.push(ch.to_ascii_uppercase());
            upper_next = false;
        } else {
            s.push(ch.to_ascii_lowercase());
        }
    }
    s
}
