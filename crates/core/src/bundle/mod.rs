//! Streaming substrate writer — IFC → GeoParquet-of-products.
//!
//! **Why this exists.** The mesh pipeline accumulates every product
//! into a `Vec<ProductMesh>` before writing OBJ/glTF. Working-set RAM
//! tracks file size at ~2-3×; the pipeline OOM-killed a 1 GB IFC on a
//! 15 GB host. Worse: OBJ strips every IFC semantic the downstream
//! analyser needs (psets, materials, storey, type), so anyone doing
//! real analysis has to re-parse the IFC alongside the OBJ.
//!
//! Both failures collapse into one: convert as a *streaming per-product
//! emit*, pairing geometry with the IFC semantic fingerprint, into a
//! format that's natively analyzable. The cross-industry precedent is
//! GeoParquet (GIS) + USD (film/games) — both solved equivalent
//! problems in their domains. We adopt the GeoParquet pattern here for
//! the analysis lane.
//!
//! Architecture:
//!
//!   1. **Pre-pass (`Bundle::build`).** Walk the EntityTable once,
//!      build IndexedFile + four extractors (psets, materials,
//!      quantities, classifications). Bounded by entity count, not
//!      geometry size.
//!   2. **GUID-keyed regrouping.** Pre-group the long-format extractor
//!      output into `guid -> Vec<...>` maps, plus product→storey,
//!      product→type, product→aggregate-parent lookups.
//!   3. **Streaming mesh pass.** [`crate::mesh::mesh_ifc_streaming`]
//!      hands each `ProductMesh` to a `ProductSink`. The sink looks up
//!      semantics via the `Bundle`, builds one substrate `ProductRecord`,
//!      writes it, drops the mesh. RAM working-set ≈ one product +
//!      one Parquet row-group buffer.
//!
//! The substrate record schema is *source-format-agnostic* — class
//! names are normalized ("Wall" not "IfcWallStandardCase"), and the
//! raw source class is kept alongside for round-trip / traceability.
//! That's deliberate: IFC5 abandons STEP entirely, so the substrate
//! has to outlive any one input dialect.

pub mod parquet_sink;
pub mod record;

use std::collections::HashMap;
use std::sync::Arc;

use crate::entity_table::EntityTable;
use crate::extractors::{classifications, materials, psets, quantities};
use crate::indexer;
use crate::lexer::{parse_field, split_top_level_args, Field};

/// One pset key-value as it appears on a product. The two name fields
/// repeat heavily across products (a 100K-product file typically has
/// <10K distinct prop_name + <2K distinct set_name), so they're stored
/// behind `Arc<str>` and interned by `Bundle::build`. The literal
/// `value` stays an owned `String` — values are mostly unique and
/// interning would just add a HashMap probe with no payoff.
#[derive(Debug, Clone)]
pub struct PsetValue {
    pub set_name: Arc<str>,
    pub name: Arc<str>,
    pub value: Option<String>,
    pub value_type: Option<Arc<str>>,
}

/// One material layer or material assignment on a product. `name` and
/// `category` are interned — material catalogues are tiny (10s-100s of
/// unique strings) but referenced O(layers) times.
#[derive(Debug, Clone)]
pub struct MaterialEntry {
    pub role: &'static str,
    pub layer_index: i32,
    pub name: Option<Arc<str>>,
    pub thickness_mm: Option<f64>,
    pub category: Option<Arc<str>>,
    /// Composition fraction for `role = "constituent"` rows (IFC4
    /// `IfcMaterialConstituent.Fraction`, typically a unit-normalised
    /// 0..1 weight). `None` for all other roles. See
    /// [`crate::extractors::materials::MaterialTable::fraction`].
    pub fraction: Option<f64>,
}

/// One quantity (length/area/volume/count) on a product.
#[derive(Debug, Clone)]
pub struct QuantityEntry {
    pub set_name: Arc<str>,
    pub name: Arc<str>,
    pub value: Option<String>,
    pub quantity_type: Arc<str>,
    pub unit_step_id: Option<u64>,
}

/// One classification reference on a product. Every field is low-
/// cardinality except `identification` (one classification code per
/// element-class typically), so all six string fields are interned.
#[derive(Debug, Clone)]
pub struct ClassificationEntry {
    pub system_name: Option<Arc<str>>,
    pub edition: Option<Arc<str>>,
    pub identification: Option<Arc<str>>,
    pub name: Option<Arc<str>>,
    pub location: Option<Arc<str>>,
    pub source: Option<Arc<str>>,
}

/// Per-product semantic snapshot, paired with geometry by the sink.
/// Class/source_class/storey_name/type_name come from low-cardinality
/// vocabularies (entity names, storey labels, type catalogue) so
/// they're held as `Arc<str>` to dedupe across the 10K-100K instances
/// pointing at the same value.
#[derive(Debug, Clone)]
pub struct ProductSemantics {
    pub ifc_id: u64,
    pub class: Arc<str>,
    pub source_class: Arc<str>,
    pub name: Option<String>,
    pub predefined_type: Option<String>,
    pub object_type: Option<String>,
    pub tag: Option<String>,
    pub storey_guid: Option<String>,
    pub storey_name: Option<Arc<str>>,
    pub aggregates_parent_guid: Option<String>,
    pub type_guid: Option<String>,
    pub type_name: Option<Arc<str>>,
    pub materials: Vec<MaterialEntry>,
    pub psets: Vec<PsetValue>,
    pub quantities: Vec<QuantityEntry>,
    pub classifications: Vec<ClassificationEntry>,
}

/// All semantic data the streaming converter needs, pre-built once and
/// looked up by product GUID during the mesh pass.
pub struct Bundle {
    pub schema: String,
    pub project_name: Option<String>,
    pub authoring_app: Option<String>,
    pub unit_scale: Option<f64>,
    pub product_count: usize,

    // Per-product lookups — keyed by GUID. Populated once during
    // `build`, then read-only during the streaming pass.
    by_guid: HashMap<String, ProductCore>,
    psets_by_guid: HashMap<String, Vec<PsetValue>>,
    materials_by_guid: HashMap<String, Vec<MaterialEntry>>,
    quantities_by_guid: HashMap<String, Vec<QuantityEntry>>,
    classifications_by_guid: HashMap<String, Vec<ClassificationEntry>>,
}

#[derive(Debug, Clone)]
struct ProductCore {
    ifc_id: u64,
    source_class: Arc<str>,
    class: Arc<str>,
    name: Option<String>,
    predefined_type: Option<String>,
    object_type: Option<String>,
    tag: Option<String>,
    storey_guid: Option<String>,
    storey_name: Option<Arc<str>>,
    aggregates_parent_guid: Option<String>,
    type_guid: Option<String>,
    type_name: Option<Arc<str>>,
}

/// Tiny string-interning helper used by `Bundle::build` so the
/// regrouped maps share one `Arc<str>` per unique value rather than
/// holding N copies. The cache is dropped at the end of `build` — the
/// `Arc<str>` references it returned now live in the bundle's maps.
fn intern(cache: &mut HashMap<String, Arc<str>>, s: String) -> Arc<str> {
    if let Some(a) = cache.get(&s) {
        return Arc::clone(a);
    }
    let a: Arc<str> = Arc::from(s.as_str());
    cache.insert(s, Arc::clone(&a));
    a
}

fn intern_opt(
    cache: &mut HashMap<String, Arc<str>>,
    s: Option<String>,
) -> Option<Arc<str>> {
    s.map(|v| intern(cache, v))
}

/// Fallback `Arc<str>` for the rare semantics_for() lookup that misses
/// the by_guid map. Equivalent to `String::new()` but for `Arc<str>`.
fn empty_arc_str() -> Arc<str> {
    Arc::from("")
}

impl Bundle {
    /// Build the bundle from a memory-mapped IFC buffer. Does all the
    /// up-front work the streaming pass relies on. Cost is bounded by
    /// entity count (not geometry size) — fits in modest RAM even on
    /// 1 GB+ files.
    pub fn build(buf: &[u8]) -> Self {
        let table = EntityTable::build_from_slice(buf);
        let mut idx = indexer::index(buf);

        // Step-id -> guid for every IfcRoot-derived entity, used by
        // every extractor and by the storey/type lookups below.
        let mut step_to_guid: HashMap<u64, String> =
            HashMap::with_capacity(idx.product_step_id.len() + idx.type_object_step_id.len() + 64);
        for (sid, _t, args) in table.iter() {
            let fields = split_top_level_args(args);
            if let Some(first) = fields.first() {
                if let Field::String(s) = parse_field(first) {
                    // IfcRoot.GlobalId is always 22 chars (Base64-encoded
                    // 128-bit GUID). Anything else is a different first
                    // field (label, descriptor) and would corrupt the map.
                    if s.len() == 22 {
                        step_to_guid.insert(sid, s);
                    }
                }
            }
        }

        // Storey-id -> name (also covers building/site so aggregates can
        // resolve any spatial parent's name without re-walking the file).
        let mut storey_name_by_id: HashMap<u64, String> = HashMap::new();
        for (i, sid) in idx.storey_step_id.iter().enumerate() {
            if let Some(Some(n)) = idx.storey_name.get(i) {
                storey_name_by_id.insert(*sid, n.clone());
            }
        }

        // Product step_id -> containing storey step_id, from
        // IfcRelContainedInSpatialStructure. Many products fall into the
        // building or site directly rather than a storey — we record
        // whichever spatial parent the file declared (those `structure`
        // ids may not be storeys, and that's fine; the GUID + name still
        // resolve through `step_to_guid` + `storey_name_by_id`).
        let mut contained_in: HashMap<u64, u64> =
            HashMap::with_capacity(idx.contained_in_child.len());
        for (child, structure) in idx
            .contained_in_child
            .iter()
            .zip(idx.contained_in_structure.iter())
        {
            contained_in.insert(*child, *structure);
        }

        // Product step_id -> aggregate parent step_id. IfcRelAggregates
        // child->parent (e.g. an assembly's parts -> the assembly).
        let mut agg_parent: HashMap<u64, u64> =
            HashMap::with_capacity(idx.aggregates_child.len());
        for (child, parent) in idx
            .aggregates_child
            .iter()
            .zip(idx.aggregates_parent.iter())
        {
            agg_parent.insert(*child, *parent);
        }

        // Product step_id -> type_object step_id, from
        // IfcRelDefinesByType. One type per product (the relation fans
        // out N products per type; we record the type each product
        // belongs to).
        let mut product_type: HashMap<u64, u64> =
            HashMap::with_capacity(idx.defines_by_type_product.len());
        for (prod, ty) in idx
            .defines_by_type_product
            .iter()
            .zip(idx.defines_by_type_type.iter())
        {
            product_type.insert(*prod, *ty);
        }

        // Type-object step_id -> (guid, name) for resolution above.
        let mut type_info: HashMap<u64, (String, Option<String>)> =
            HashMap::with_capacity(idx.type_object_step_id.len());
        for i in 0..idx.type_object_step_id.len() {
            let sid = idx.type_object_step_id[i];
            let guid = idx.type_object_guid[i].clone();
            let name = idx.type_object_name[i].clone();
            type_info.insert(sid, (guid, name));
        }

        // String-intern cache shared across the per-product assembly
        // AND all four regrouping loops below. Dropped at the end of
        // `build` — every `Arc<str>` it returned is owned by the
        // bundle's maps by then. Single-threaded, no locking.
        let mut str_cache: HashMap<String, Arc<str>> = HashMap::new();

        // Assemble per-product core records. Consume the indexer's
        // parallel Vecs by-move so we don't double-allocate the
        // owned-String columns: `mem::take` snatches the backing buffer
        // and replaces the indexer's vec with an empty one we don't
        // touch again.
        let product_step_id = std::mem::take(&mut idx.product_step_id);
        let product_guid = std::mem::take(&mut idx.product_guid);
        let product_entity = std::mem::take(&mut idx.product_entity);
        let product_name = std::mem::take(&mut idx.product_name);
        let product_predefined_type = std::mem::take(&mut idx.product_predefined_type);
        let product_object_type = std::mem::take(&mut idx.product_object_type);
        let product_tag = std::mem::take(&mut idx.product_tag);

        let mut by_guid: HashMap<String, ProductCore> =
            HashMap::with_capacity(product_step_id.len());

        let product_iter = product_step_id
            .into_iter()
            .zip(product_guid)
            .zip(product_entity)
            .zip(product_name)
            .zip(product_predefined_type)
            .zip(product_object_type)
            .zip(product_tag)
            .map(|((((((a, b), c), d), e), f), g)| (a, b, c, d, e, f, g));

        for (sid, guid, entity, name, predef, obj_ty, tag) in product_iter {
            let source_class = intern(&mut str_cache, entity);
            let class = intern(&mut str_cache, normalize_class(&source_class));

            // Two ways an IFC declares "this product is in storey X":
            //   1. IfcRelContainedInSpatialStructure (the common case
            //      for walls, slabs, doors, etc.)
            //   2. IfcRelAggregates with a storey as the parent (used
            //      for IfcSpace in many authoring tools — Duplex's
            //      Revit export aggregates all spaces under storeys).
            // The contained_in path is checked first; the aggregate
            // fallback walks up to a depth limit, picking the first
            // storey ancestor it finds.
            let storey_sid = contained_in.get(&sid).copied().or_else(|| {
                let mut cur = sid;
                for _ in 0..32 {
                    match agg_parent.get(&cur).copied() {
                        Some(parent) if storey_name_by_id.contains_key(&parent) => {
                            return Some(parent);
                        }
                        Some(parent) => cur = parent,
                        None => return None,
                    }
                }
                None
            });
            let storey_guid = storey_sid.and_then(|s| step_to_guid.get(&s).cloned());
            let storey_name = storey_sid
                .and_then(|s| storey_name_by_id.get(&s).cloned())
                .map(|s| intern(&mut str_cache, s));

            let agg_parent_sid = agg_parent.get(&sid).copied();
            let aggregates_parent_guid =
                agg_parent_sid.and_then(|s| step_to_guid.get(&s).cloned());

            let type_sid = product_type.get(&sid).copied();
            let (type_guid, type_name) = match type_sid.and_then(|s| type_info.get(&s).cloned()) {
                Some((g, n)) => (Some(g), n.map(|s| intern(&mut str_cache, s))),
                None => (None, None),
            };

            by_guid.insert(
                guid,
                ProductCore {
                    ifc_id: sid,
                    source_class,
                    class,
                    name,
                    predefined_type: predef,
                    object_type: obj_ty,
                    tag,
                    storey_guid,
                    storey_name,
                    aggregates_parent_guid,
                    type_guid,
                    type_name,
                },
            );
        }

        // Extractors emit long-format columnar tables; regroup into
        // per-product Vecs so the streaming sink can look one up in
        // O(1) per product. We consume the extractor `Vec<String>`
        // columns by `into_iter()` so the row strings are MOVED (not
        // cloned) into the regrouped maps; the high-repeat fields
        // (set_name, prop_name, …) pass through `intern` so duplicate
        // values share one `Arc<str>`.
        let psets_table = psets::build(&table, &step_to_guid);
        let mat_table = materials::build(&table, &step_to_guid, idx.unit_scale.unwrap_or(1.0));
        let qty_table = quantities::build(&table, &step_to_guid);
        let cls_table = classifications::build(&table, &step_to_guid);

        let mut psets_by_guid: HashMap<String, Vec<PsetValue>> = HashMap::new();
        let pset_iter = psets_table
            .guid
            .into_iter()
            .zip(psets_table.pset_name)
            .zip(psets_table.prop_name)
            .zip(psets_table.value)
            .zip(psets_table.value_type);
        for ((((guid, set_name), prop_name), value), value_type) in pset_iter {
            psets_by_guid.entry(guid).or_default().push(PsetValue {
                set_name: intern(&mut str_cache, set_name),
                name: intern(&mut str_cache, prop_name),
                value,
                value_type: intern_opt(&mut str_cache, value_type),
            });
        }

        let mut materials_by_guid: HashMap<String, Vec<MaterialEntry>> = HashMap::new();
        let mat_iter = mat_table
            .guid
            .into_iter()
            .zip(mat_table.role)
            .zip(mat_table.layer_index)
            .zip(mat_table.material_name)
            .zip(mat_table.layer_thickness_mm)
            .zip(mat_table.category)
            .zip(mat_table.fraction);
        for ((((((guid, role), layer_index), material_name), thickness_mm), category), fraction) in
            mat_iter
        {
            materials_by_guid.entry(guid).or_default().push(MaterialEntry {
                role,
                layer_index,
                name: intern_opt(&mut str_cache, material_name),
                thickness_mm,
                category: intern_opt(&mut str_cache, category),
                fraction,
            });
        }

        let mut quantities_by_guid: HashMap<String, Vec<QuantityEntry>> = HashMap::new();
        let qty_iter = qty_table
            .guid
            .into_iter()
            .zip(qty_table.qto_name)
            .zip(qty_table.quantity_name)
            .zip(qty_table.value)
            .zip(qty_table.quantity_type)
            .zip(qty_table.unit_step_id);
        for (((((guid, qto_name), quantity_name), value), quantity_type), unit_step_id) in qty_iter
        {
            quantities_by_guid
                .entry(guid)
                .or_default()
                .push(QuantityEntry {
                    set_name: intern(&mut str_cache, qto_name),
                    name: intern(&mut str_cache, quantity_name),
                    value,
                    quantity_type: intern(&mut str_cache, quantity_type),
                    unit_step_id,
                });
        }

        let mut classifications_by_guid: HashMap<String, Vec<ClassificationEntry>> =
            HashMap::new();
        let cls_iter = cls_table
            .guid
            .into_iter()
            .zip(cls_table.system_name)
            .zip(cls_table.edition)
            .zip(cls_table.identification)
            .zip(cls_table.name)
            .zip(cls_table.location)
            .zip(cls_table.source);
        for ((((((guid, system_name), edition), identification), name), location), source) in
            cls_iter
        {
            classifications_by_guid
                .entry(guid)
                .or_default()
                .push(ClassificationEntry {
                    system_name: intern_opt(&mut str_cache, system_name),
                    edition: intern_opt(&mut str_cache, edition),
                    identification: intern_opt(&mut str_cache, identification),
                    name: intern_opt(&mut str_cache, name),
                    location: intern_opt(&mut str_cache, location),
                    source: intern_opt(&mut str_cache, source),
                });
        }

        // str_cache drops here — the bundle's maps now own all the
        // Arc<str>s outright.
        drop(str_cache);

        Self {
            schema: idx.schema,
            project_name: idx.project_name,
            authoring_app: idx.authoring_app,
            unit_scale: idx.unit_scale,
            product_count: by_guid.len(),
            by_guid,
            psets_by_guid,
            materials_by_guid,
            quantities_by_guid,
            classifications_by_guid,
        }
    }

    /// Look up everything we know about a product by its IFC GUID.
    /// Returns an empty-ish snapshot if the GUID is unknown — the mesh
    /// pipeline may surface products the indexer doesn't classify
    /// (proxy elements, schema versions we don't enumerate). We don't
    /// drop them; we emit the geometry with whatever semantics we have.
    pub fn semantics_for(&self, guid: &str) -> ProductSemantics {
        let core = self.by_guid.get(guid);
        ProductSemantics {
            ifc_id: core.map(|c| c.ifc_id).unwrap_or(0),
            class: core.map(|c| Arc::clone(&c.class)).unwrap_or_else(empty_arc_str),
            source_class: core
                .map(|c| Arc::clone(&c.source_class))
                .unwrap_or_else(empty_arc_str),
            name: core.and_then(|c| c.name.clone()),
            predefined_type: core.and_then(|c| c.predefined_type.clone()),
            object_type: core.and_then(|c| c.object_type.clone()),
            tag: core.and_then(|c| c.tag.clone()),
            storey_guid: core.and_then(|c| c.storey_guid.clone()),
            storey_name: core.and_then(|c| c.storey_name.as_ref().map(Arc::clone)),
            aggregates_parent_guid: core.and_then(|c| c.aggregates_parent_guid.clone()),
            type_guid: core.and_then(|c| c.type_guid.clone()),
            type_name: core.and_then(|c| c.type_name.as_ref().map(Arc::clone)),
            materials: self
                .materials_by_guid
                .get(guid)
                .cloned()
                .unwrap_or_default(),
            psets: self.psets_by_guid.get(guid).cloned().unwrap_or_default(),
            quantities: self
                .quantities_by_guid
                .get(guid)
                .cloned()
                .unwrap_or_default(),
            classifications: self
                .classifications_by_guid
                .get(guid)
                .cloned()
                .unwrap_or_default(),
        }
    }

    pub fn product_count(&self) -> usize {
        self.product_count
    }

    /// Lift the four extractor table sizes for diagnostic output.
    pub fn semantic_stats(&self) -> SemanticStats {
        SemanticStats {
            products_indexed: self.by_guid.len(),
            pset_rows: self.psets_by_guid.values().map(|v| v.len()).sum(),
            material_rows: self.materials_by_guid.values().map(|v| v.len()).sum(),
            quantity_rows: self.quantities_by_guid.values().map(|v| v.len()).sum(),
            classification_rows: self.classifications_by_guid.values().map(|v| v.len()).sum(),
        }
    }
}

pub struct SemanticStats {
    pub products_indexed: usize,
    pub pset_rows: usize,
    pub material_rows: usize,
    pub quantity_rows: usize,
    pub classification_rows: usize,
}

/// Map an IFC type name to a domain-level class. The substrate is
/// source-format-agnostic: downstream users filter on "Wall", not
/// "IfcWallStandardCase". Source class stays on the record for round-
/// trip and for users who need the raw IFC type.
///
/// The mapping is deliberately conservative — strip the `Ifc` prefix
/// and the StandardCase/ElementedCase suffixes, leave the rest. A
/// richer normalization (folding IfcCurtainWall + IfcWall* into "Wall",
/// IfcPipeFitting + IfcPipeSegment into "Pipe", etc.) is a follow-on
/// when a downstream query actually wants it.
fn normalize_class(source: &str) -> String {
    let trimmed = source
        .strip_prefix("Ifc")
        .or_else(|| source.strip_prefix("IFC"))
        .unwrap_or(source);
    let trimmed = trimmed
        .strip_suffix("StandardCase")
        .or_else(|| trimmed.strip_suffix("ElementedCase"))
        .unwrap_or(trimmed);
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::normalize_class;

    #[test]
    fn normalize_strips_ifc_prefix() {
        assert_eq!(normalize_class("IfcWall"), "Wall");
        assert_eq!(normalize_class("IfcSlab"), "Slab");
    }

    #[test]
    fn normalize_strips_case_suffixes() {
        assert_eq!(normalize_class("IfcWallStandardCase"), "Wall");
        assert_eq!(normalize_class("IfcSlabElementedCase"), "Slab");
    }

    #[test]
    fn normalize_passes_through_unknown() {
        assert_eq!(normalize_class("IfcCustomThing"), "CustomThing");
    }
}
