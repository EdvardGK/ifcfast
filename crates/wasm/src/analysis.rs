//! Rust port of the sidecar pipeline `scripts/generate_sample_sidecars.py`
//! drives through the Python `Model` wrapper (GH #172).
//!
//! Every number here comes from the same `ifcfast-core` entry point the
//! Python wheel calls — `indexer::index`, the four `extractors::*`
//! builders, `mesh::mesh_ifc` + `mesh::stats::ProductStats` (the engine
//! behind `_core.analyse_drift`). The only logic reimplemented is the
//! Python-side *joining*: storey resolution, type linkage, the
//! aggregate rollup, and the per-entity QTO aggregation.
//!
//! Two Python-layer artefacts are reproduced deliberately because the
//! shipped sidecars carry them and the site's instrument reads them:
//!
//!   * `pandas.DataFrame.to_json` writes floats at `double_precision=10`.
//!     Every drift-derived measure therefore reaches the JSON rounded to
//!     10 decimal places, and the per-class QTO sums are sums *of the
//!     rounded values*. [`round10`] reproduces that exactly.
//!   * `graph.json`'s `project_name` is `getattr(model.header,
//!     "project_name", None)` — `IFCHeader` has no such attribute, so the
//!     field is always `null`. Kept verbatim rather than silently
//!     "fixed", so a dropped file and the baked sample agree.

use std::collections::{HashMap, HashSet};

use ifcfast_core::clock::Instant;
use ifcfast_core::entity_table::EntityTable;
use ifcfast_core::extractors::{classifications, materials, psets, quantities};
use ifcfast_core::indexer::{self, IndexedFile};
use ifcfast_core::lexer::{parse_field, split_top_level_args, Field};
use ifcfast_core::mesh::gltf::resolve_product_color;
use ifcfast_core::mesh::rebase::{global_shift_for, shift_world_in_place, shifted_world_positions};
use ifcfast_core::mesh::stats::ProductStats;
use ifcfast_core::mesh::{self, BakeFrame, ProductMesh, ProductSink};
use ifcfast_core::source::IfcSource;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// Per-product pset-derived attributes: (fire rating, load bearing, external) — see the sidecar generator.
pub type PsetAttrs = HashMap<String, (Option<Value>, Option<Value>, Option<Value>)>;
/// Per-product material rollup: (thickness, layers, area) as the sidecar generator computes it.
pub type Rollup = HashMap<String, (Option<f64>, Option<f64>, Option<f64>)>;

/// Mirrors `python/ifcfast/header.py::_CACHE_SCHEMA_VERSION`. Bump in
/// lockstep — it is hashed into `cache_key`, so a mismatch shows up as a
/// changed key rather than stale data.
const CACHE_SCHEMA_VERSION: u32 = 34;
const HASH_HEAD_BYTES: usize = 4 * 1024 * 1024;
const HASH_TAIL_BYTES: usize = 4 * 1024 * 1024;
/// `header.py::_HEADER_READ_BYTES` — the window FILE_SCHEMA is read from.
const HEADER_READ_BYTES: usize = 64 * 1024;

/// `pandas.DataFrame.to_json(double_precision=10)`.
///
/// Verified against the shipped `duplex.graph.json`: every drift measure
/// in the sidecar equals `float(f"{v:.10f}")` of the raw column value
/// (1716/1716 columns checked). `format!("{:.10}")` performs the same
/// correctly-rounded decimal conversion, so the round-trip agrees.
pub fn round10(v: f64) -> f64 {
    format!("{v:.10}").parse::<f64>().unwrap_or(v)
}

/// `round10` for a value that may be NaN / infinite — those reach the
/// Python JSON as `null`.
fn round10_opt(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(round10(v))
    } else {
        None
    }
}

/// `round10` guarded for NaN / infinity, which reach the Python JSON as
/// `null` (`DataFrame.to_json` writes non-finite doubles that way).
fn finite10(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(round10(v))
    } else {
        None
    }
}

fn jnum(v: Option<f64>) -> Value {
    match v {
        Some(x) => json!(x),
        None => Value::Null,
    }
}

fn jstr(v: &Option<String>) -> Value {
    match v {
        Some(s) => Value::String(s.clone()),
        None => Value::Null,
    }
}

/// Python truthiness for the `or` fallbacks the sidecar script relies on.
fn truthy(v: &Option<String>) -> bool {
    matches!(v, Some(s) if !s.is_empty())
}

// ---------------------------------------------------------------------
// Tier-1 rows
// ---------------------------------------------------------------------

/// One row of `model.products_df` — the fields the sidecars read. `mode`
/// is not carried: it never reaches a sidecar value, only the column list
/// in `summary()["tables"]["products"]`.
pub struct ProductRow {
    pub guid: String,
    pub entity: String,
    pub name: Option<String>,
    pub predefined_type: Option<String>,
    pub object_type: Option<String>,
    pub tag: Option<String>,
    pub storey_guid: Option<String>,
    pub parent_guid: Option<String>,
    /// GlobalId of the `IfcTypeObject` this occurrence is defined by
    /// through `IfcRelDefinesByType` — the wheel's
    /// `ProductRow.type_guid`, and the join key from a graph product row
    /// to a [`TypeObjectRow`].
    ///
    /// `None` whenever `type_source` is not `"ifctype"`: an
    /// `ObjectType` string names a type the file never declared as an
    /// object, so there is no GUID to point at. Never derived from
    /// `type_name` — two distinct `IfcWallType`s may share a name.
    pub type_guid: Option<String>,
    pub type_name: Option<String>,
    pub type_source: &'static str,
}

/// One `IfcTypeObject` (or any `IfcXxxType` subclass) DECLARED by the
/// file — `model.type_objects` / `TypeObjectRow` on the Python side.
///
/// Declared is not used. A Revit or MagiCAD export routinely carries
/// type objects no occurrence references, and telling the two apart is
/// the whole point of exposing this: `typesJson()` rolls USED types up
/// by name over occurrences and can only ever see what something points
/// at, while this is the roster the exporter actually wrote. Unused
/// types are `type_objects` minus the distinct `type_guid` over
/// products.
pub struct TypeObjectRow {
    pub guid: String,
    /// Proper-cased entity name, straight from the core indexer, e.g.
    /// `IfcTypeProduct`.
    ///
    /// The core's spelling map is backed by the PRODUCT whitelist, which
    /// by construction holds no `*Type` class, so most type classes take
    /// the first-letter-only fallback and arrive as `Ifcwalltype`, not
    /// `IfcWallType`. That is what the wheel reports for the same file
    /// (both read the same indexer field), so it is what the browser
    /// reports: a second, prettier implementation here would make the
    /// two disagree. Compare it case-insensitively until GH #186 gives
    /// the core a full-schema entity list.
    pub entity: String,
    pub name: Option<String>,
    pub step_id: u64,
}

pub struct StoreyRow {
    pub guid: String,
    pub name: Option<String>,
    /// Raw `IfcBuildingStorey.Elevation` — **file units**, not metres.
    /// Kept verbatim so a round-trip writes back the number the file
    /// declared (GH #180 / #181).
    pub elevation: Option<f64>,
    pub building_guid: Option<String>,
    /// [`StoreyRow::elevation`] in METRES — the view every other length
    /// surface in this module already speaks (`drift` `*_m`, QTO `*_m3`,
    /// streamed vertices). `None` when the elevation is absent or NaN,
    /// or when the file declared a length unit that could not be
    /// resolved: a fabricated metre value is worse than an absent one,
    /// and `unwrap_or(1.0)` would turn an unresolvable millimetre model
    /// into a plausible-looking 1000x error (GH #181).
    pub elevation_m: Option<f64>,
}

/// The drift columns the sidecars consume, already in SI (the Rust
/// `analyse_drift` scaling) and already rounded the way
/// `DataFrame.to_json` rounds them.
pub struct DriftRow {
    pub guid: String,
    pub surface_area_m2: Option<f64>,
    pub volume_abs_m3: Option<f64>,
    pub max_extent_m: Option<f64>,
    pub triangle_count: u32,
}

#[derive(Default)]
pub struct MeshCounters {
    pub products_seen: usize,
    pub products_meshed: usize,
    pub products_deferred: usize,
    pub triangles: usize,
    pub mesh_ms: f64,
    pub entity_table_ms: f64,
    pub by_source: Vec<(String, usize)>,
}

/// Which mesh pass, if any, has run against this model.
///
/// v2 (GH #172) splits [`Analysis::run`] — parse + index + extractors —
/// from the tessellation, so the browser can paint identity and the type
/// roster while the geometry is still being produced. Everything that
/// needs geometry pulls one of the two passes in on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshState {
    /// No tessellation yet. `drift` / `counters` / `meshes` are empty.
    Pending,
    /// [`Analysis::stream_mesh`] ran: per-product stats are complete,
    /// but the `ProductMesh`es were released batch by batch and are NOT
    /// retained (that is the whole point — bounded RAM on a 200 MB file).
    Streamed,
    /// [`Analysis::batch_mesh`] ran: stats complete AND `meshes` retained
    /// for the glTF writer. This is the v1 behaviour.
    Batched,
}

/// Everything the four JSON surfaces are derived from. Identity and the
/// tier-1 join are built by [`Analysis::run`]; the geometry-derived
/// fields (`drift`, `counters`, `segment_rows`, `meshes`) are filled by
/// whichever mesh pass runs first — see [`MeshState`].
pub struct Analysis {
    /// The decompressed STEP bytes, retained so a mesh pass can run
    /// after `run` returned. `.ifczip` input is already inflated here,
    /// so the mesh pass never re-decompresses.
    pub source: IfcSource,
    pub name: String,
    pub size_bytes: usize,
    pub header_schema: Option<String>,
    pub cache_key: String,
    pub parse_seconds: f64,

    pub idx: IndexedFile,
    pub unit_resolved: bool,
    pub duplicate_step_ids: usize,

    pub products: Vec<ProductRow>,
    pub storeys: Vec<StoreyRow>,
    /// `(step_id, guid)` sorted by step id. The core stores these in a
    /// `HashMap`, so the Python sidecar's order is whatever that
    /// iteration produced on the day it ran — not reproducible. Sorting
    /// makes the browser output deterministic.
    pub spaces: Vec<(u64, String)>,
    pub buildings: Vec<(u64, String)>,
    pub sites: Vec<(u64, String)>,
    pub projects: Vec<(u64, String)>,

    pub contained_in: Vec<(String, String, &'static str)>,
    pub aggregates: Vec<(String, String, &'static str)>,
    pub storey_building: Vec<(String, String)>,
    pub voids: Vec<(String, String)>,
    /// GUIDs on the `RelatedOpeningElement` side of an
    /// `IfcRelVoidsElement`. Subtracted geometry by definition — the
    /// viewer export drops them (see `IfcModel::to_glb`); every table
    /// and counter still reveals them.
    pub opening_guids: HashSet<String>,
    /// Every `IfcTypeObject` the file declares, in file order — the
    /// roster `typeObjectsJson()` serialises. Kept as rows rather than
    /// the bare count it used to be: the count could say 398 while
    /// nothing could name one of them, and `typesJson()`'s 84 used names
    /// are a different number about a different thing.
    pub type_objects: Vec<TypeObjectRow>,

    /// The four long-format data layers, retained verbatim so
    /// `psetsJson` / `quantitiesJson` / `materialsJson` /
    /// `classificationsJson` are a pure serialise (GH #183).
    ///
    /// Retaining them does not raise peak memory: every one of these is
    /// already fully materialised inside [`Analysis::run`], alongside the
    /// `EntityTable` they were built from. Only the steady state grows,
    /// and it grows by exactly what the caller is about to ask for.
    pub psets_t: psets::PsetTable,
    pub quantities_t: quantities::QuantityTable,
    pub materials_t: materials::MaterialTable,
    pub classifications_t: classifications::ClassificationTable,
    pub segment_rows: usize,

    /// guid → (materials in first-seen order, layer-set name)
    pub materials_by_guid: HashMap<String, Vec<String>>,
    pub layer_set_by_guid: HashMap<String, String>,
    /// guid → (IsExternal, LoadBearing, FireRating) as JSON values
    pub pset_attrs: PsetAttrs,

    pub drift: Vec<DriftRow>,
    pub drift_by_guid: HashMap<String, usize>,

    pub counters: MeshCounters,
    /// Metre-scaled, cutter-stripped product meshes, in emission order.
    /// Only populated by [`Analysis::batch_mesh`]; the streaming pass
    /// deliberately drops each mesh after emitting it.
    pub meshes: Vec<ProductMesh>,

    /// IFC length-unit → metres factor, hoisted out of the geometry
    /// block so a later mesh pass uses the same number `run` did.
    pub unit_scale_f64: f64,
    pub mesh_state: MeshState,
    /// Model-wide global shift in METRES, pinned by whichever mesh pass
    /// ran — [`Analysis::stream_mesh`] or [`Analysis::batch_mesh`] —
    /// from the first emitted product's `mesh_anchor`, the same rule
    /// `_core.extract_meshes` uses for its `global_shift`. Streamed
    /// positions and the `toGlb` GLB alike are world metres MINUS this;
    /// add it back for absolute coordinates. `[0, 0, 0]` before any mesh
    /// pass has run, and for every near-origin model.
    pub stream_shift_m: [f64; 3],
}

/// What [`Analysis::stream_mesh`] hands each finished batch to:
/// `(metaJson, positions, indices, progressJson)`. The `ifcfast-wasm`
/// layer wraps the JS callback in one of these so `analysis` stays free
/// of `wasm-bindgen` types (and unit-testable off the web target).
pub type BatchEmit<'e> = &'e mut dyn FnMut(&str, &[f32], &[u32], &str) -> Result<(), String>;

impl Analysis {
    pub fn run(bytes: &[u8], name: &str) -> Result<Analysis, String> {
        let t_total = Instant::now();
        let size_bytes = bytes.len();
        let cache_key = compute_cache_key(bytes, size_bytes);

        let source = ifcfast_core::source::open_bytes(bytes.to_vec())
            .map_err(|e| format!("ifcfast: {e}"))?;
        let buf = source.as_bytes();

        let idx = indexer::index(buf);
        if let Some(err) = &idx.parse_error {
            return Err(format!("ifcfast: refusing a truncated IFC ({name}): {err}"));
        }
        let header_schema = file_schema(buf);
        let unit_resolved = idx.unit_scale.is_some() || !lengthunit_declared(buf);

        // ----- tier-1 join (port of `model.py::_index_native`) --------
        let mut storey_step_to_guid: HashMap<u64, String> = HashMap::new();
        let mut storeys: Vec<StoreyRow> = Vec::with_capacity(idx.storey_guid.len());
        for i in 0..idx.storey_guid.len() {
            let sid = idx.storey_step_id[i];
            storey_step_to_guid.insert(sid, idx.storey_guid[i].clone());
            let elevation = idx.storey_elevation[i];
            storeys.push(StoreyRow {
                guid: idx.storey_guid[i].clone(),
                name: idx.storey_name[i].clone(),
                elevation,
                building_guid: None,
                // `model.py`'s rule verbatim: `None if elev is None or
                // elev != elev or st_scale is None`. Note it reads
                // `idx.unit_scale` (an `Option`), NOT the `unit_scale_f64`
                // this module defaults to 1.0 further down — that default
                // is right for geometry and wrong here.
                elevation_m: match (elevation, idx.unit_scale) {
                    (Some(e), Some(scale)) if !e.is_nan() => Some(e * scale),
                    _ => None,
                },
            });
        }

        let bldg_step_to_guid = &idx.building_step_id_to_guid;
        let mut storey_step_to_building: HashMap<u64, String> = HashMap::new();
        let mut storey_building: Vec<(String, String)> = Vec::new();
        for (child, building) in idx
            .storey_building_storey
            .iter()
            .zip(idx.storey_building_building.iter())
        {
            if let Some(sg) = storey_step_to_guid.get(child) {
                if let Some(bg) = bldg_step_to_guid.get(building) {
                    storey_step_to_building.insert(*child, bg.clone());
                    storey_building.push((sg.clone(), bg.clone()));
                }
            }
        }
        for (row, sid) in storeys.iter_mut().zip(idx.storey_step_id.iter()) {
            row.building_guid = storey_step_to_building.get(sid).cloned();
        }

        let mut product_step_to_guid: HashMap<u64, String> = HashMap::new();
        for (sid, guid) in idx.product_step_id.iter().zip(idx.product_guid.iter()) {
            product_step_to_guid.insert(*sid, guid.clone());
        }

        // step id → (guid, kind), most specific kind wins.
        let mut parent_guid_by_step: HashMap<u64, String> = HashMap::new();
        let mut parent_kind_by_step: HashMap<u64, &'static str> = HashMap::new();
        // Deliberate precedence, lowest first: the "most specific" kind
        // wins if a step id somehow appears in two tables.
        for (src, kind) in [
            (&idx.space_step_id_to_guid, "space"),
            (&idx.site_step_id_to_guid, "site"),
            (&idx.project_step_id_to_guid, "project"),
            (&idx.building_step_id_to_guid, "building"),
            (&storey_step_to_guid, "storey"),
            (&product_step_to_guid, "product"),
        ] {
            for (sid, guid) in src {
                parent_guid_by_step.insert(*sid, guid.clone());
                parent_kind_by_step.insert(*sid, kind);
            }
        }

        let mut parent_lookup: HashMap<u64, String> = HashMap::new();
        let mut aggregates: Vec<(String, String, &'static str)> = Vec::new();
        for (child, parent) in idx
            .aggregates_child
            .iter()
            .zip(idx.aggregates_parent.iter())
        {
            let (Some(pg), Some(cg)) = (
                parent_guid_by_step.get(parent),
                parent_guid_by_step.get(child),
            ) else {
                continue;
            };
            parent_lookup.insert(*child, pg.clone());
            aggregates.push((
                cg.clone(),
                pg.clone(),
                parent_kind_by_step
                    .get(parent)
                    .copied()
                    .unwrap_or("unknown"),
            ));
        }

        let mut contained_in: Vec<(String, String, &'static str)> = Vec::new();
        for (child, structure) in idx
            .contained_in_child
            .iter()
            .zip(idx.contained_in_structure.iter())
        {
            let (Some(container_guid), Some(kind), Some(cg)) = (
                parent_guid_by_step.get(structure),
                parent_kind_by_step.get(structure).copied(),
                product_step_to_guid.get(child),
            ) else {
                continue;
            };
            if kind == "product" {
                continue;
            }
            contained_in.push((cg.clone(), container_guid.clone(), kind));
        }

        // Transitive storey resolution — the `_GraphIndex` +
        // `_walk_to_storey` pair, ported so `storey_guid` agrees with the
        // graph walk (GH #88).
        let graph = GraphIndex::build(&contained_in, &aggregates, &storey_building, &storeys);
        let mut storey_guid_by_step: HashMap<u64, Option<String>> = HashMap::new();
        for (sid, guid) in &product_step_to_guid {
            storey_guid_by_step.insert(*sid, graph.walk_to_storey(guid));
        }

        // Type linkage (IfcRelDefinesByType → type guid/name), plus the
        // declared roster itself. Both come off the same four parallel
        // index vectors, so a type object either reaches BOTH surfaces
        // or neither — a product can never carry a `type_guid` that
        // `type_objects` cannot resolve.
        let mut type_objects: Vec<TypeObjectRow> =
            Vec::with_capacity(idx.type_object_step_id.len());
        let mut type_meta_by_step: HashMap<u64, (String, Option<String>)> = HashMap::new();
        for i in 0..idx.type_object_step_id.len() {
            let sid = idx.type_object_step_id[i];
            let guid = idx.type_object_guid[i].clone();
            let name = idx.type_object_name[i].clone();
            type_meta_by_step.insert(sid, (guid.clone(), name.clone()));
            type_objects.push(TypeObjectRow {
                guid,
                entity: idx.type_object_entity[i].clone(),
                name,
                step_id: sid,
            });
        }
        let mut product_type_by_step: HashMap<u64, (String, Option<String>)> = HashMap::new();
        for (psid, tsid) in idx
            .defines_by_type_product
            .iter()
            .zip(idx.defines_by_type_type.iter())
        {
            if let Some(meta) = type_meta_by_step.get(tsid) {
                product_type_by_step.insert(*psid, meta.clone());
            }
        }

        let mut products: Vec<ProductRow> = Vec::with_capacity(idx.product_guid.len());
        let mut index_by_step: HashMap<u64, usize> = HashMap::new();
        let mut duplicate_step_ids = 0usize;
        for i in 0..idx.product_guid.len() {
            let sid = idx.product_step_id[i];
            let object_type = idx.product_object_type[i].clone();
            // `model.py`'s three-way rule verbatim: a resolved
            // IfcRelDefinesByType wins and carries the type's GUID; an
            // ObjectType string is a name with no object behind it, so
            // no GUID; otherwise untyped.
            let (type_guid, type_name, type_source) = match product_type_by_step.get(&sid) {
                Some((tg, tn)) => (Some(tg.clone()), tn.clone(), "ifctype"),
                None if truthy(&object_type) => (None, object_type.clone(), "objecttype"),
                None => (None, None, "none"),
            };
            let row = ProductRow {
                guid: idx.product_guid[i].clone(),
                entity: idx.product_entity[i].clone(),
                name: idx.product_name[i].clone(),
                predefined_type: idx.product_predefined_type[i].clone(),
                object_type,
                tag: idx.product_tag[i].clone(),
                storey_guid: storey_guid_by_step.get(&sid).cloned().flatten(),
                parent_guid: parent_lookup.get(&sid).cloned(),
                type_guid,
                type_name,
                type_source,
            };
            match index_by_step.get(&sid) {
                None => {
                    index_by_step.insert(sid, products.len());
                    products.push(row);
                }
                Some(&prev) => {
                    duplicate_step_ids += 1;
                    products[prev] = row;
                }
            }
        }

        let mut voids: Vec<(String, String)> = Vec::new();
        let mut opening_guids: HashSet<String> = HashSet::new();
        for (opening, host) in idx.voids_opening.iter().zip(idx.voids_host.iter()) {
            if let (Some(og), Some(hg)) = (
                product_step_to_guid.get(opening),
                product_step_to_guid.get(host),
            ) {
                opening_guids.insert(og.clone());
                voids.push((og.clone(), hg.clone()));
            }
        }

        // ----- data layers -------------------------------------------
        let table = EntityTable::build(buf);
        if let Some(err) = table.scan_error() {
            return Err(format!("ifcfast: refusing a truncated IFC ({name}): {err}"));
        }
        let step_to_guid = build_guid_index(&table);
        let unit_scale_f64 = idx.unit_scale.unwrap_or(1.0);

        let psets_t = psets::build(&table, &step_to_guid);
        let quantities_t = quantities::build(&table, &step_to_guid);
        let materials_t = materials::build(&table, &step_to_guid, unit_scale_f64);
        let classifications_t = classifications::build(&table, &step_to_guid);
        // `EntityTable<'a>` borrows `buf`, which borrows `source`; the
        // struct below takes `source` by value, so the borrow has to be
        // over first. Every extractor above returns owned columns.
        drop(table);

        let mut materials_by_guid: HashMap<String, Vec<String>> = HashMap::new();
        let mut layer_set_by_guid: HashMap<String, String> = HashMap::new();
        for i in 0..materials_t.guid.len() {
            let guid = &materials_t.guid[i];
            let role = materials_t.role[i];
            let mname = materials_t.material_name[i].clone().unwrap_or_default();
            match role {
                "layer" | "single" => {
                    if role == "single" && mname.is_empty() {
                        continue;
                    }
                    let bucket = materials_by_guid.entry(guid.clone()).or_default();
                    if !mname.is_empty() && !bucket.contains(&mname) {
                        bucket.push(mname);
                    }
                }
                "set" if !mname.is_empty() => {
                    layer_set_by_guid.insert(guid.clone(), mname);
                }
                _ => {}
            }
        }

        let mut pset_attrs: PsetAttrs = HashMap::new();
        for i in 0..psets_t.guid.len() {
            let slot = match psets_t.prop_name[i].as_str() {
                "IsExternal" => 0,
                "LoadBearing" => 1,
                "FireRating" => 2,
                _ => continue,
            };
            let entry = pset_attrs
                .entry(psets_t.guid[i].clone())
                .or_insert((None, None, None));
            let taken = match slot {
                0 => entry.0.is_some(),
                1 => entry.1.is_some(),
                _ => entry.2.is_some(),
            };
            if taken {
                continue;
            }
            let raw = &psets_t.value[i];
            let value = match (slot, raw) {
                (2, Some(s)) => Value::String(s.clone()),
                (2, None) => Value::Null,
                (_, Some(s)) => Value::Bool(matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "true" | "t" | "1" | ".t."
                )),
                (_, None) => Value::Null,
            };
            match slot {
                0 => entry.0 = Some(value),
                1 => entry.1 = Some(value),
                _ => entry.2 = Some(value),
            }
        }

        // ----- geometry ----------------------------------------------
        // NOT run here (GH #172 v2). `fromBytes` is parse + index +
        // extractors only; the tessellation is pulled in on demand by
        // `batch_mesh` (v1 behaviour, retains meshes for glTF) or driven
        // incrementally by `stream_mesh`. Both fill exactly the fields
        // left empty below, with identical numbers.
        let spaces = sorted_pairs(&idx.space_step_id_to_guid);
        let buildings = sorted_pairs(&idx.building_step_id_to_guid);
        let sites = sorted_pairs(&idx.site_step_id_to_guid);
        let projects = sorted_pairs(&idx.project_step_id_to_guid);

        Ok(Analysis {
            source,
            name: name.to_string(),
            size_bytes,
            header_schema,
            cache_key,
            parse_seconds: t_total.elapsed().as_secs_f64(),
            unit_resolved,
            duplicate_step_ids,
            type_objects,
            products,
            storeys,
            spaces,
            buildings,
            sites,
            projects,
            contained_in,
            aggregates,
            storey_building,
            voids,
            opening_guids,
            psets_t,
            quantities_t,
            materials_t,
            classifications_t,
            segment_rows: 0,
            materials_by_guid,
            layer_set_by_guid,
            pset_attrs,
            drift: Vec::new(),
            drift_by_guid: HashMap::new(),
            counters: MeshCounters::default(),
            meshes: Vec::new(),
            unit_scale_f64,
            mesh_state: MeshState::Pending,
            stream_shift_m: [0.0, 0.0, 0.0],
            idx,
        })
    }

    // -----------------------------------------------------------------
    // Mesh passes
    // -----------------------------------------------------------------

    /// Make the geometry-derived numbers available, cheapest way first:
    /// if a stream already produced them, keep them; otherwise run the
    /// batch pass. Called by `graphJson` / `qtoJson` / `bySourceJson` /
    /// `statsJson`.
    pub fn ensure_stats(&mut self) {
        if self.mesh_state == MeshState::Pending {
            self.batch_mesh();
        }
    }

    /// Make the retained `ProductMesh`es available for the glTF writer.
    /// A streamed model dropped them, so this re-runs the batch pass —
    /// the numbers are identical (same entry point, same order), only
    /// the meshes are new. A viewer that streams never calls `toGlb`.
    pub fn ensure_meshes(&mut self) {
        if self.mesh_state != MeshState::Batched {
            self.batch_mesh();
        }
    }

    /// The batch pass: a Local-frame streaming pass collected into a
    /// `Vec`, the synthetic half-space stand-in slabs stripped before any
    /// measure is taken (GH #66), then the same shifted-world-metres
    /// reposition `write_gltf`'s sink does.
    ///
    /// Local, not World (GH #188): a World bake casts to f32 at the
    /// absolute magnitude, which on an NTM-georeferenced millimetre model
    /// is an 8 mm / 128 mm vertex lattice — round ducts wobble ±4 mm and
    /// one face in ten collapses to zero area. The stats are taken from
    /// the Local mesh (surface area / volume / max extent are
    /// translation-invariant, and more accurate here), and the retained
    /// meshes are repositioned through `mesh::rebase` so `toGlb` writes a
    /// precise GLB.
    pub fn batch_mesh(&mut self) {
        let unit_scale = self.unit_scale_f64 as f32;
        let us = self.unit_scale_f64;
        let (meshes, drift, segment_rows, counters, shift) = {
            let buf = self.source.as_bytes();
            let (mut meshes, mesh_stats) = mesh::mesh_ifc_framed(buf, BakeFrame::Local);
            for m in &mut meshes {
                mesh::strip_synthetic_cutters(m);
            }
            let mut drift: Vec<DriftRow> = Vec::with_capacity(meshes.len());
            let mut segment_rows = 0usize;
            for m in &meshes {
                segment_rows += m.segments.len();
                drift.push(drift_row(m, us, unit_scale));
            }

            // Pin the model-wide shift on the first product that has
            // drawable geometry — the same rule, in the same order, the
            // streaming sink uses, so `streamShiftJson()` is one value
            // whichever pass produced it.
            //
            // `us` is the f64 unit factor, not `unit_scale as f64`: the
            // shift is reported in metres through the same factor, and
            // 0.001 is not representable in f32 — at NTM magnitudes the
            // f32 factor puts `position + shift` 4 mm / 59 mm off the
            // absolute coordinate it is supposed to reconstruct.
            // The model-level pin (GH #188), decided from the placement
            // chains before any product was emitted. `[0, 0, 0]` leaves
            // the first-emitted-anchor fallback below to fire.
            let mut shift: Option<[f64; 3]> = if mesh_stats.global_shift == [0.0, 0.0, 0.0] {
                None
            } else {
                Some(mesh_stats.global_shift)
            };
            for m in &mut meshes {
                if m.vertices.is_empty() || m.indices.is_empty() {
                    continue;
                }
                let s = *shift.get_or_insert_with(|| global_shift_for(&m.mesh_anchor, us));
                shift_world_in_place(m, &s, us);
            }
            (
                meshes,
                drift,
                segment_rows,
                counters_from(&mesh_stats),
                shift.unwrap_or([0.0, 0.0, 0.0]),
            )
        };

        self.stream_shift_m = [shift[0] * us, shift[1] * us, shift[2] * us];
        self.meshes = meshes;
        self.set_geometry(drift, segment_rows, counters);
        self.mesh_state = MeshState::Batched;
    }

    /// Streaming pass (GH #172 v2). Drives the core's sink ONCE and hands
    /// `emit` a batch every `products_per_batch` emitted products, plus a
    /// final partial batch.
    ///
    /// The per-product stats are computed from the same Local-frame,
    /// cutter-stripped mesh [`Analysis::batch_mesh`] measures, in the same
    /// emission order, so `qto_json` / `graph_json` after a stream are
    /// byte-identical to the batch path's.
    ///
    /// Positions are world METRES minus [`Analysis::stream_shift_m`],
    /// repositioned from the Local bake in f64 so a georeferenced
    /// millimetre model keeps sub-micron vertex precision (GH #188);
    /// indices are batch-local (already offset by the product's `v0`).
    pub fn stream_mesh(
        &mut self,
        products_per_batch: usize,
        emit: BatchEmit<'_>,
    ) -> Result<(), String> {
        let unit_scale = self.unit_scale_f64 as f32;
        // Owned before the `source` borrow opens — the sink cannot hold a
        // reference into `self` while `self.source` is borrowed.
        let meta_of: HashMap<String, (Value, Value)> = self
            .products
            .iter()
            .map(|p| {
                let type_name = if truthy(&p.type_name) {
                    &p.type_name
                } else {
                    &p.object_type
                };
                (p.guid.clone(), (jstr(&p.storey_guid), jstr(type_name)))
            })
            .collect();
        let total = self.products.len();

        let (drift, segment_rows, counters, shift, err) = {
            let buf = self.source.as_bytes();
            let mut sink = StreamSink {
                unit_scale,
                // f64 factor, deliberately not `unit_scale as f64` — see
                // `batch_mesh`: the reported shift uses the same one, so
                // `position + shift` reconstructs the absolute coordinate.
                us: self.unit_scale_f64,
                per_batch: products_per_batch.max(1),
                total,
                meta_of,
                shift: None,
                meta: Vec::new(),
                positions: Vec::new(),
                indices: Vec::new(),
                in_batch: 0,
                drift: Vec::new(),
                segment_rows: 0,
                seen: 0,
                meshed: 0,
                emit,
                err: None,
            };
            let mesh_stats = mesh::mesh_ifc_streaming_framed(buf, &mut sink, BakeFrame::Local);
            sink.flush();
            (
                sink.drift,
                sink.segment_rows,
                counters_from(&mesh_stats),
                sink.shift.unwrap_or([0.0, 0.0, 0.0]),
                sink.err,
            )
        };

        let us = self.unit_scale_f64;
        self.stream_shift_m = [shift[0] * us, shift[1] * us, shift[2] * us];
        // The stats are complete even if the JS callback threw partway —
        // record them before surfacing the error, so a caller that
        // recovers still gets consistent tables.
        self.set_geometry(drift, segment_rows, counters);
        self.meshes = Vec::new();
        self.mesh_state = MeshState::Streamed;
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn set_geometry(&mut self, drift: Vec<DriftRow>, segment_rows: usize, counters: MeshCounters) {
        let mut drift_by_guid: HashMap<String, usize> = HashMap::with_capacity(drift.len());
        for (i, d) in drift.iter().enumerate() {
            drift_by_guid.insert(d.guid.clone(), i);
        }
        self.drift = drift;
        self.drift_by_guid = drift_by_guid;
        self.segment_rows = segment_rows;
        self.counters = counters;
    }
}

/// One `drift` row from one cutter-stripped, Local-frame product mesh.
///
/// The rescale is done in f64 on f32 inputs, and `analyse_drift` does
/// exactly the same, so the 10-decimal rounding lands on the same value
/// on both sides (GH #188). `unit_scale_f32` is only the tolerance
/// parameter `ProductStats` needs (drift floor, weld epsilon).
fn drift_row(m: &ProductMesh, unit_scale: f64, unit_scale_f32: f32) -> DriftRow {
    let s = ProductStats::from_mesh(m, unit_scale_f32);
    let us_len = unit_scale;
    let us_area = unit_scale * unit_scale;
    let us_vol = unit_scale * unit_scale * unit_scale;
    DriftRow {
        guid: s.guid.clone(),
        surface_area_m2: round10_opt(s.surface_area as f64 * us_area),
        volume_abs_m3: round10_opt(s.volume.abs() as f64 * us_vol),
        max_extent_m: round10_opt(s.max_extent as f64 * us_len),
        triangle_count: s.triangle_count,
    }
}

fn counters_from(mesh_stats: &mesh::MeshStats) -> MeshCounters {
    let mut by_source: Vec<(String, usize)> = mesh_stats
        .by_source
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    by_source.sort_by(|a, b| a.0.cmp(&b.0));
    MeshCounters {
        products_seen: mesh_stats.products_seen,
        products_meshed: mesh_stats.products_meshed,
        products_deferred: mesh_stats.products_deferred,
        triangles: mesh_stats.triangles,
        mesh_ms: mesh_stats.elapsed_ms,
        entity_table_ms: mesh_stats.entity_table_build_ms,
        by_source,
    }
}

/// The streaming [`ProductSink`]: accumulates products into a batch,
/// hands the batch to the JS callback, then releases the buffers.
///
/// Batch geometry is one merged buffer pair — the whole point on the
/// viewer side is ~one draw call per batch instead of one per product —
/// with the per-product spans carried in `meta` so picking and per-
/// product colour still work.
struct StreamSink<'e> {
    /// Tolerance parameter for `ProductStats` (drift floor, weld eps).
    unit_scale: f32,
    /// Linear-unit-to-metres factor, f64 — the one every metre cast and
    /// the reported shift go through (GH #188).
    us: f64,
    per_batch: usize,
    total: usize,
    /// guid → (storey_guid, type_name) as the graph resolves them.
    meta_of: HashMap<String, (Value, Value)>,
    /// Model-wide shift in MODEL UNITS, pinned from the first emitted
    /// product's `mesh_anchor` (the `extract_meshes` rule —
    /// [`mesh::rebase::global_shift_for`]).
    shift: Option<[f64; 3]>,

    meta: Vec<Value>,
    positions: Vec<f32>,
    indices: Vec<u32>,
    in_batch: usize,

    drift: Vec<DriftRow>,
    segment_rows: usize,
    seen: usize,
    meshed: usize,

    emit: BatchEmit<'e>,
    err: Option<String>,
}

impl StreamSink<'_> {
    fn flush(&mut self) {
        if self.meta.is_empty() {
            return;
        }
        let meta = Value::Array(std::mem::take(&mut self.meta)).to_string();
        if self.err.is_none() {
            let progress = json!({
                "seen": self.seen,
                "meshed": self.meshed,
                "total": self.total,
            })
            .to_string();
            if let Err(e) = (self.emit)(&meta, &self.positions, &self.indices, &progress) {
                self.err = Some(e);
            }
        }
        self.positions.clear();
        self.indices.clear();
        self.in_batch = 0;
    }
}

impl ProductSink for StreamSink<'_> {
    /// GH #188: the model-level pin, decided from the placement chains
    /// before any emission. Non-zero wins over the first-emitted-anchor
    /// fallback in `on_product`, which only exists for files whose
    /// georeference is baked into the representation geometry.
    fn on_global_shift(&mut self, shift: [f64; 3]) {
        if shift != [0.0, 0.0, 0.0] {
            self.shift = Some(shift);
        }
    }

    fn on_product(&mut self, mut mesh: ProductMesh) {
        self.seen += 1;
        // Same order as the batch pass: strip the synthetic half-space
        // stand-in slabs FIRST, then measure (GH #66).
        mesh::strip_synthetic_cutters(&mut mesh);
        self.segment_rows += mesh.segments.len();
        let row = drift_row(&mesh, self.us, self.unit_scale);
        let (m3, m2, tri) = (row.volume_abs_m3, row.surface_area_m2, row.triangle_count);
        self.drift.push(row);

        // A product whose only geometry was cutter slabs still gets its
        // drift row (triangle_count 0) — that is what the batch pass
        // does, and `products_with_mesh` counts it — but there is
        // nothing to draw, so it never reaches a batch.
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return;
        }
        self.meshed += 1;

        let shift = *self
            .shift
            .get_or_insert_with(|| global_shift_for(&mesh.mesh_anchor, self.us));

        let v0 = self.positions.len() / 3;
        let vn = mesh.vertices.len() / 3;
        // Local frame → shifted world metres, repositioned in f64 with
        // the f32 cast last (GH #188). Subtracting the shift AFTER an
        // absolute f32 bake — what this did before — recovers nothing:
        // the residual still carries the 8 mm / 128 mm lattice.
        shifted_world_positions(&mesh, &shift, self.us, &mut self.positions);
        let i0 = self.indices.len();
        let i_n = mesh.indices.len();
        let base = v0 as u32;
        self.indices.extend(mesh.indices.iter().map(|i| i + base));

        let rgba = resolve_product_color(&mesh);
        let (storey_guid, type_name) = self
            .meta_of
            .get(&mesh.guid)
            .cloned()
            .unwrap_or((Value::Null, Value::Null));
        self.meta.push(json!({
            "guid": mesh.guid,
            "entity": mesh.entity,
            "storey_guid": storey_guid,
            "type_name": type_name,
            "m3": jnum(m3),
            "m2": jnum(m2),
            "tri": tri,
            "v0": v0,
            "vn": vn,
            "i0": i0,
            "in": i_n,
            "rgba": [rgba[0], rgba[1], rgba[2], rgba[3]],
        }));

        self.in_batch += 1;
        if self.in_batch >= self.per_batch {
            self.flush();
        }
    }
}

fn sorted_pairs(m: &HashMap<u64, String>) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = m.iter().map(|(k, v)| (*k, v.clone())).collect();
    out.sort_by_key(|(sid, _)| *sid);
    out
}

/// Port of the shared GUID index every extractor in `lib.rs` builds.
fn build_guid_index(table: &EntityTable) -> HashMap<u64, String> {
    let mut out: HashMap<u64, String> = HashMap::with_capacity(64_000);
    for (sid, type_name, args) in table.iter() {
        if !type_name.starts_with(b"IFC") {
            continue;
        }
        let fields = split_top_level_args(args);
        if let Some(first) = fields.first() {
            if let Field::String(s) = parse_field(first) {
                if s.len() == 22 {
                    out.insert(sid, s);
                }
            }
        }
    }
    out
}

fn compute_cache_key(bytes: &[u8], size: usize) -> String {
    let mut h = Sha256::new();
    h.update(CACHE_SCHEMA_VERSION.to_le_bytes());
    h.update((size as u64).to_le_bytes());
    let head_n = HASH_HEAD_BYTES.min(size);
    let tail_n = HASH_TAIL_BYTES.min(size.saturating_sub(head_n));
    h.update(&bytes[..head_n]);
    if tail_n > 0 {
        h.update(&bytes[size - tail_n..]);
    }
    let digest = h.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..16].to_string()
}

/// `header.py::_extract_block(text, "FILE_SCHEMA")` + first quoted item.
fn file_schema(buf: &[u8]) -> Option<String> {
    let window = &buf[..HEADER_READ_BYTES.min(buf.len())];
    let text = String::from_utf8_lossy(window);
    let idx = text.find("FILE_SCHEMA")?;
    let rest = &text[idx + "FILE_SCHEMA".len()..];
    let open = rest.find('(')?;
    let end = rest.find(");")?;
    if end < open {
        return None;
    }
    let body = &rest[open + 1..end];
    let start = body.find('\'')?;
    let after = &body[start + 1..];
    let stop = after.find('\'')?;
    Some(after[..stop].to_string())
}

/// `model.py::_lengthunit_declared` — only consulted when `unit_scale`
/// is `None`, to tell "no LENGTHUNIT at all" from "declared but broken".
fn lengthunit_declared(buf: &[u8]) -> bool {
    let up = buf.to_ascii_uppercase();
    let data_at = find_sub(&up, b"\nDATA;").or_else(|| find_sub(&up, b"DATA;"));
    let body = match data_at {
        Some(i) => &up[i..],
        None => &up[..],
    };
    find_sub(body, b".LENGTHUNIT.").is_some()
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------
// Spatial graph index (port of `_GraphIndex` + `_walk_to_storey`)
// ---------------------------------------------------------------------

struct GraphIndex {
    parent_of: HashMap<String, String>,
    storey_of: HashMap<String, String>,
    container_of: HashMap<String, String>,
    storey_guids: HashSet<String>,
}

impl GraphIndex {
    fn build(
        contained_in: &[(String, String, &'static str)],
        aggregates: &[(String, String, &'static str)],
        storey_building: &[(String, String)],
        storeys: &[StoreyRow],
    ) -> GraphIndex {
        let mut g = GraphIndex {
            parent_of: HashMap::new(),
            storey_of: HashMap::new(),
            container_of: HashMap::new(),
            storey_guids: storeys.iter().map(|s| s.guid.clone()).collect(),
        };
        for (child, parent, _) in aggregates {
            g.parent_of.insert(child.clone(), parent.clone());
        }
        for (product, container, kind) in contained_in {
            g.container_of.insert(product.clone(), container.clone());
            if *kind == "storey" {
                g.storey_of.insert(product.clone(), container.clone());
                g.storey_guids.insert(container.clone());
            }
        }
        for (storey, _building) in storey_building {
            g.storey_guids.insert(storey.clone());
        }
        g
    }

    fn walk_to_storey(&self, guid: &str) -> Option<String> {
        if let Some(direct) = self.storey_of.get(guid) {
            return Some(direct.clone());
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut cur = guid.to_string();
        for _ in 0..16 {
            if !seen.insert(cur.clone()) {
                return None;
            }
            if let Some(container) = self.container_of.get(&cur) {
                if self.storey_guids.contains(container) {
                    return Some(container.clone());
                }
                cur = container.clone();
                continue;
            }
            let parent = self.parent_of.get(&cur)?;
            if self.storey_guids.contains(parent) {
                return Some(parent.clone());
            }
            cur = parent.clone();
        }
        None
    }
}

// ---------------------------------------------------------------------
// JSON surfaces
// ---------------------------------------------------------------------

/// Column lists advertised by `Model.summary()["tables"]`. Static
/// contracts on the Python side (dataclass fields / `_LAYER_DTYPES`), so
/// they are static here too.
const COLS: &[(&str, &[&str])] = &[
    (
        "products",
        &[
            "guid",
            "entity",
            "name",
            "predefined_type",
            "object_type",
            "tag",
            "storey_guid",
            "storey_name",
            "parent_guid",
            "mode",
            "step_id",
            "type_guid",
            "type_name",
            "type_source",
        ],
    ),
    // `elevation_m` is LAST because Python's column list is
    // `StoreyRow.__dataclass_fields__` order and it is the defaulted
    // field (GH #181).
    (
        "storeys",
        &["guid", "name", "elevation", "building_guid", "elevation_m"],
    ),
    (
        "spaces",
        &["guid", "step_id", "name", "storey_guid", "storey_name"],
    ),
    ("type_objects", &["guid", "entity", "name", "step_id"]),
    (
        "contained_in",
        &["product_guid", "container_guid", "container_kind"],
    ),
    ("aggregates", &["child_guid", "parent_guid", "parent_kind"]),
    ("storey_building", &["storey_guid", "building_guid"]),
    ("voids", &["opening_guid", "host_guid"]),
    (
        "psets",
        &[
            "guid",
            "pset_name",
            "prop_name",
            "value",
            "value_type",
            "source",
        ],
    ),
    (
        "quantities",
        &[
            "guid",
            "qto_name",
            "quantity_name",
            "value",
            "quantity_type",
            "unit_step_id",
            "source",
        ],
    ),
    (
        "materials",
        &[
            "guid",
            "role",
            "layer_index",
            "material_name",
            "layer_thickness_mm",
            "category",
            "fraction",
            "source",
        ],
    ),
    (
        "classifications",
        &[
            "guid",
            "system_name",
            "edition",
            "identification",
            "name",
            "location",
            "source",
            "assignment_source",
        ],
    ),
    (
        "drift",
        &[
            "guid",
            "entity",
            "source",
            "triangle_count",
            "surface_area_m2",
            "volume_abs_m3",
            "aabb_volume_m3",
            "placement_x_m",
            "placement_y_m",
            "placement_z_m",
            "centroid_x_m",
            "centroid_y_m",
            "centroid_z_m",
            "drift_distance_m",
            "max_extent_m",
            "drift_ratio",
            "drift_severity",
            "mesh_quality",
        ],
    ),
    (
        "segments",
        &[
            "guid",
            "product_index",
            "segment_index",
            "source",
            "triangle_count",
            "index_start",
        ],
    ),
];

fn table_meta(name: &str, rows: usize) -> Value {
    table_meta_loaded(name, rows, true)
}

/// `loaded` is a lie worth avoiding: the geometry-derived tables
/// (`drift`, `segments`) are genuinely absent until a mesh pass has run,
/// and `summaryJson()` is deliberately mesh-free in v2 so the browser can
/// show identity the moment parsing finishes (GH #172).
fn table_meta_loaded(name: &str, rows: usize, loaded: bool) -> Value {
    let cols = COLS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .unwrap_or(&[]);
    json!({ "rows": rows, "columns": cols, "loaded": loaded })
}

impl Analysis {
    fn schema(&self) -> Option<String> {
        if !self.idx.schema.is_empty() {
            Some(self.idx.schema.clone())
        } else {
            self.header_schema.clone()
        }
    }

    fn length_unit(&self) -> String {
        let Some(scale) = self.idx.unit_scale else {
            return "unknown".to_string();
        };
        for (name, factor) in [
            ("mm", 0.001f64),
            ("cm", 0.01),
            ("dm", 0.1),
            ("m", 1.0),
            ("in", 0.0254),
            ("ft", 0.3048),
        ] {
            if (scale - factor).abs() < 1e-9 {
                return name.to_string();
            }
        }
        format!("{scale}m-per-unit")
    }

    pub fn summary_json(&self) -> Value {
        let mut top: Vec<(&String, &u32)> = self.idx.type_counts.iter().collect();
        // Python sorts by `-count` over a dict whose order came from a
        // Rust HashMap; ties there are luck. Break them on the entity
        // name so the browser output is stable.
        top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let mut top_types = Map::new();
        for (k, v) in top.into_iter().take(20) {
            top_types.insert(k.clone(), json!(v));
        }

        let mut tables = Map::new();
        tables.insert(
            "products".into(),
            table_meta("products", self.products.len()),
        );
        tables.insert("storeys".into(), table_meta("storeys", self.storeys.len()));
        tables.insert("spaces".into(), table_meta("spaces", self.spaces.len()));
        tables.insert(
            "type_objects".into(),
            table_meta("type_objects", self.type_objects.len()),
        );
        tables.insert(
            "contained_in".into(),
            table_meta("contained_in", self.contained_in.len()),
        );
        tables.insert(
            "aggregates".into(),
            table_meta("aggregates", self.aggregates.len()),
        );
        tables.insert(
            "storey_building".into(),
            table_meta("storey_building", self.storey_building.len()),
        );
        tables.insert("voids".into(), table_meta("voids", self.voids.len()));
        tables.insert("psets".into(), table_meta("psets", self.psets_t.guid.len()));
        tables.insert(
            "quantities".into(),
            table_meta("quantities", self.quantities_t.guid.len()),
        );
        tables.insert(
            "materials".into(),
            table_meta("materials", self.materials_t.guid.len()),
        );
        tables.insert(
            "classifications".into(),
            table_meta("classifications", self.classifications_t.guid.len()),
        );
        let meshed = self.mesh_state != MeshState::Pending;
        tables.insert(
            "drift".into(),
            table_meta_loaded("drift", self.drift.len(), meshed),
        );
        tables.insert(
            "segments".into(),
            table_meta_loaded("segments", self.segment_rows, meshed),
        );

        // GH #184: entities with the `IfcProduct` attribute shape whose
        // class no whitelist entry claimed. A file made entirely of such
        // a class indexes to ZERO products, and without this the drop
        // zone renders an empty model with no explanation.
        //
        // Keys are the STEP tokens the indexer counted — `IFCTUBEBUNDLE`,
        // not `IfcTubeBundle`. The wheel title-cases them through
        // `ifcfast.data.schema_supertypes.ALL_ENTITIES`, the full
        // IFC2X3/IFC4/IFC4X3 entity list generated into the Python
        // package; the wasm crate has no such list and the core's
        // `type_name_uppercase_with_proper_case` is (a) `pub(crate)` and
        // (b) backed by the 153-entry PRODUCT whitelist spelling map,
        // which by definition never contains a SKIPPED class — its
        // fallback would produce `Ifctubebundle`, a wrong answer that
        // looks right. The STEP spelling is what the file actually says,
        // so that is what the browser reports; `crates/wasm/test/
        // parity.mjs` normalises the key case (and only the case) when
        // diffing against the wheel's summary. See the report on #184.
        let mut skipped: Vec<(&String, &u32)> =
            self.idx.skipped_product_type_counts.iter().collect();
        skipped.sort_by(|a, b| a.0.cmp(b.0));
        let mut skipped_types = Map::new();
        for (name, count) in skipped {
            skipped_types.insert(name.clone(), json!(count));
        }

        json!({
            "path": self.name,
            "size_bytes": self.size_bytes,
            "schema": self.schema(),
            "project_name": self.idx.project_name,
            "authoring_app": self.idx.authoring_app,
            "unit_scale": self.idx.unit_scale,
            "unit_resolved": self.unit_resolved,
            "length_unit": self.length_unit(),
            "cache_key": self.cache_key,
            "products": self.products.len(),
            "storeys": self.storeys.len(),
            "type_counts_total": self.idx.type_counts.len(),
            "top_types": Value::Object(top_types),
            "tables": Value::Object(tables),
            "parse_seconds": self.parse_seconds,
            "duplicate_step_ids": self.duplicate_step_ids,
            "warnings": self.idx.warnings,
            "skipped_product_types": Value::Object(skipped_types),
        })
    }

    /// Aggregate rollup — `m3`/`m2`/`lm` summed over every descendant of
    /// a product that carries no body of its own (IfcRoof, IfcStair,
    /// IfcCurtainWall …). Port keeps the Python traversal order because
    /// float addition is not associative.
    fn rollup(&self) -> Rollup {
        let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut parents: HashSet<&str> = HashSet::new();
        for (child, parent, _) in &self.aggregates {
            children_of
                .entry(parent.as_str())
                .or_default()
                .push(child.as_str());
            parents.insert(parent.as_str());
        }
        let mut seeds: HashSet<&str> = HashSet::new();
        for d in &self.drift {
            seeds.insert(d.guid.as_str());
        }
        seeds.extend(parents.iter().copied());
        seeds.extend(children_of.keys().copied());

        let mut out = HashMap::new();
        for g in seeds {
            let mut descendants: Vec<&str> = Vec::new();
            let mut stack: Vec<&str> = children_of.get(g).cloned().unwrap_or_default();
            while let Some(c) = stack.pop() {
                descendants.push(c);
                if let Some(kids) = children_of.get(c) {
                    stack.extend(kids.iter().copied());
                }
            }
            if descendants.is_empty() {
                continue;
            }
            let (mut m3, mut m2, mut lm) = (0.0f64, 0.0f64, 0.0f64);
            let (mut m3c, mut m2c, mut lmc) = (0usize, 0usize, 0usize);
            for d in descendants {
                let Some(&i) = self.drift_by_guid.get(d) else {
                    continue;
                };
                let row = &self.drift[i];
                if let Some(v) = row.volume_abs_m3 {
                    m3 += v;
                    m3c += 1;
                }
                if let Some(v) = row.surface_area_m2 {
                    m2 += v;
                    m2c += 1;
                }
                if let Some(v) = row.max_extent_m {
                    lm += v;
                    lmc += 1;
                }
            }
            if m3c > 0 || m2c > 0 || lmc > 0 {
                out.insert(
                    g.to_string(),
                    (
                        if m3c > 0 { Some(m3) } else { None },
                        if m2c > 0 { Some(m2) } else { None },
                        if lmc > 0 { Some(lm) } else { None },
                    ),
                );
            }
        }
        out
    }

    pub fn graph_json(&self) -> Value {
        let rollup = self.rollup();
        let mut products = Vec::with_capacity(self.products.len());
        for p in &self.products {
            let ms = self.drift_by_guid.get(&p.guid).map(|&i| &self.drift[i]);
            let m3_direct = ms.and_then(|d| d.volume_abs_m3);
            let m2_direct = ms.and_then(|d| d.surface_area_m2);
            let lm_direct = ms.and_then(|d| d.max_extent_m);
            let roll = rollup.get(&p.guid);
            let (m_source, m3, m2, lm) =
                if m3_direct.is_some() || m2_direct.is_some() || lm_direct.is_some() {
                    ("direct", m3_direct, m2_direct, lm_direct)
                } else if let Some((r3, r2, rl)) = roll {
                    ("aggregate-rollup", *r3, *r2, *rl)
                } else {
                    ("none", None, None, None)
                };
            let attrs = self.pset_attrs.get(&p.guid);
            type Triple = (Option<Value>, Option<Value>, Option<Value>);
            let pick = |f: fn(&Triple) -> &Option<Value>| {
                attrs.and_then(|a| f(a).clone()).unwrap_or(Value::Null)
            };
            let type_name = if truthy(&p.type_name) {
                p.type_name.clone()
            } else {
                p.object_type.clone()
            };
            products.push(json!({
                "guid": p.guid,
                "entity": p.entity,
                "name": jstr(&p.name),
                "predefined_type": jstr(&p.predefined_type),
                "object_type": jstr(&p.object_type),
                "tag": jstr(&p.tag),
                "storey_guid": jstr(&p.storey_guid),
                "parent_guid": jstr(&p.parent_guid),
                "typed": p.type_source == "ifctype",
                // The join key into `typeObjectsJson()`. `type_name`
                // above falls back to `object_type` when the file only
                // named a type; `type_guid` has no fallback and is null
                // there, which is exactly what makes "typed by a real
                // type object" checkable.
                "type_guid": jstr(&p.type_guid),
                "type_name": jstr(&type_name),
                "type_source": p.type_source,
                "materials": self.materials_by_guid.get(&p.guid).cloned().unwrap_or_default(),
                "layer_set": self.layer_set_by_guid.get(&p.guid).cloned().map(Value::String).unwrap_or(Value::Null),
                "m3": jnum(m3),
                "m2": jnum(m2),
                "lm": jnum(lm),
                "m_source": m_source,
                "m3_direct": jnum(m3_direct),
                "m2_direct": jnum(m2_direct),
                "lm_direct": jnum(lm_direct),
                "is_external": pick(|a| &a.0),
                "load_bearing": pick(|a| &a.1),
                "fire_rating": pick(|a| &a.2),
            }));
        }

        let storeys: Vec<Value> = self
            .storeys
            .iter()
            .map(|s| {
                json!({
                    "guid": s.guid,
                    "name": jstr(&s.name),
                    "elevation": s.elevation,
                    "building_guid": jstr(&s.building_guid),
                    "elevation_m": jnum(s.elevation_m),
                })
            })
            .collect();
        let spaces: Vec<Value> = self
            .spaces
            .iter()
            .map(|(sid, g)| json!({"step_id": sid, "guid": g, "entity": "IfcSpace"}))
            .collect();
        let coll = |v: &Vec<(u64, String)>| -> Vec<Value> {
            v.iter()
                .map(|(_, g)| json!({"guid": g, "name": Value::Null}))
                .collect()
        };

        json!({
            // `getattr(model.header, "project_name", None)` — IFCHeader
            // has no such field, so the shipped sidecar carries null.
            "project_name": Value::Null,
            "schema": self.header_schema,
            "products": products,
            "storeys": storeys,
            "spaces": spaces,
            "buildings": coll(&self.buildings),
            "sites": coll(&self.sites),
            "projects": coll(&self.projects),
            "contained_in": self.contained_in.iter()
                .filter(|(_, _, kind)| *kind == "storey")
                .map(|(p, c, _)| json!({"product_guid": p, "storey_guid": c}))
                .collect::<Vec<_>>(),
            "aggregates": self.aggregates.iter()
                .map(|(c, p, k)| json!({"child_guid": c, "parent_guid": p, "parent_kind": k}))
                .collect::<Vec<_>>(),
            "storey_building": self.storey_building.iter()
                .map(|(s, b)| json!({"storey_guid": s, "building_guid": b}))
                .collect::<Vec<_>>(),
            "voids": self.voids.iter()
                .map(|(o, h)| json!({"opening_guid": o, "host_guid": h}))
                .collect::<Vec<_>>(),
            // The materials extractor exposes no `set_name` column, so
            // the Python builder's `if not set_name: continue` drops every
            // row and the map is always empty. Ported as-is.
            "material_layer_sets": Value::Object(Map::new()),
        })
    }

    pub fn qto_json(&self) -> Value {
        struct Row {
            entity: String,
            count: usize,
            storeys: Vec<String>,
            area: f64,
            volume: f64,
            triangles: u64,
            with_mesh: usize,
            without_mesh: usize,
        }
        let storey_name_by_guid: HashMap<&str, &str> = self
            .storeys
            .iter()
            .filter_map(|s| s.name.as_deref().map(|n| (s.guid.as_str(), n)))
            .collect();

        let mut order: Vec<String> = Vec::new();
        let mut rows: HashMap<String, Row> = HashMap::new();
        for p in &self.products {
            let row = rows.entry(p.entity.clone()).or_insert_with(|| {
                order.push(p.entity.clone());
                Row {
                    entity: p.entity.clone(),
                    count: 0,
                    storeys: Vec::new(),
                    area: 0.0,
                    volume: 0.0,
                    triangles: 0,
                    with_mesh: 0,
                    without_mesh: 0,
                }
            });
            row.count += 1;
            if let Some(sg) = &p.storey_guid {
                if let Some(name) = storey_name_by_guid.get(sg.as_str()) {
                    if !row.storeys.iter().any(|s| s == name) {
                        row.storeys.push((*name).to_string());
                    }
                }
            }
            match self.drift_by_guid.get(&p.guid) {
                None => row.without_mesh += 1,
                Some(&i) => {
                    let d = &self.drift[i];
                    row.with_mesh += 1;
                    if let Some(v) = d.surface_area_m2 {
                        row.area += v;
                    }
                    if let Some(v) = d.volume_abs_m3 {
                        row.volume += v;
                    }
                    row.triangles += d.triangle_count as u64;
                }
            }
        }

        let mut out: Vec<Value> = Vec::with_capacity(order.len());
        let mut keys: Vec<&String> = order.iter().collect();
        keys.sort_by(|a, b| {
            let (ra, rb) = (&rows[*a], &rows[*b]);
            rb.count.cmp(&ra.count).then(ra.entity.cmp(&rb.entity))
        });
        for k in keys {
            let r = &rows[k];
            let mut storeys = r.storeys.clone();
            storeys.sort();
            let (area, volume, source) = if r.with_mesh == 0 {
                (Value::Null, Value::Null, "none")
            } else {
                (json!(r.area), json!(r.volume), "mesh")
            };
            out.push(json!({
                "entity": r.entity,
                "count": r.count,
                "storeys": storeys,
                "area_m2": area,
                "volume_m3": volume,
                "triangles": r.triangles,
                "products_with_mesh": r.with_mesh,
                "products_without_mesh": r.without_mesh,
                "source": source,
            }));
        }

        json!({
            "schema": self.header_schema,
            "products": self.products.len(),
            "rows": out,
        })
    }

    /// `types/manifest.json`. `glb` / `bytes` are empty in v1 — no
    /// per-type mini-glb export in the browser (it needs `subset` +
    /// a second glTF write per type); the instrument degrades to the
    /// count/name view.
    pub fn types_json(&self, version: &str) -> Value {
        let mut order: Vec<&str> = Vec::new();
        let mut groups: HashMap<&str, (usize, &str, &str)> = HashMap::new();
        for p in &self.products {
            if p.entity == "IfcSpace" {
                continue;
            }
            let Some(tn) = p.type_name.as_deref() else {
                continue;
            };
            match groups.get_mut(tn) {
                Some(g) => g.0 += 1,
                None => {
                    order.push(tn);
                    groups.insert(tn, (1, p.entity.as_str(), p.guid.as_str()));
                }
            }
        }
        // `groupby` yields keys ascending; the outer `sorted` is stable
        // and keys on `(-count, type_name)`, so the order is total.
        order.sort();
        order.sort_by(|a, b| {
            let (ca, cb) = (groups[a].0, groups[b].0);
            cb.cmp(&ca).then(a.cmp(b))
        });

        let mut seen: HashSet<String> = HashSet::new();
        let mut types = Vec::with_capacity(order.len());
        for tn in order {
            let (count, entity, guid) = groups[tn];
            let mut slug = slugify(&format!("{}-{}", &entity[3..], tn));
            while seen.contains(&slug) {
                slug.push_str("-x");
            }
            seen.insert(slug.clone());
            types.push(json!({
                "slug": slug,
                "type_name": tn,
                "entity": entity,
                "count": count,
                "guid": guid,
                "glb": "",
                "bytes": 0,
            }));
        }
        json!({
            "source": self.name,
            "generated_with": version,
            "types": types,
        })
    }

    pub fn by_source_json(&self) -> Value {
        let mut m = Map::new();
        for (k, v) in &self.counters.by_source {
            m.insert(k.clone(), json!(v));
        }
        Value::Object(m)
    }

    // -----------------------------------------------------------------
    // Long-format data layers (GH #183)
    // -----------------------------------------------------------------
    //
    // `summaryJson().tables.<name>` has always reported these loaded,
    // with their row counts and column lists, while nothing could read a
    // row. These four are the rows — the same columns, in the same
    // order, with the same normalisation as the Python DataFrames
    // (`model.psets` / `.quantities` / `.materials` /
    // `.classifications`), because both sides serialise the output of
    // the SAME `extractors::*::build` call. Row order is therefore
    // identical by construction, not by luck: the extractors emit in
    // `EntityTable` order over a sorted inheritance list, no `HashMap`
    // iteration in the emission path.
    //
    // Missing values are `null`, never `""` — a property whose value the
    // authoring tool left as `$` is absent, not empty. That is the
    // pandas `None` the sidecar's `to_json` writes.
    //
    // `psets.value` and `quantities.value` are STRINGS (or `null`).
    // The extractor keeps the STEP literal verbatim and names its type
    // in the sibling column (`value_type` / `quantity_type`); the Python
    // layer marshals the same `Option<String>` into an `object` column
    // and never coerces it either. Parsing `"3.0"` to `3.0` here would
    // make the browser disagree with the wheel, so a consumer that wants
    // a number reads `value_type` and parses.

    /// `[{guid, pset_name, prop_name, value, value_type, source}]` —
    /// `model.psets` as row objects.
    pub fn psets_json(&self) -> Value {
        let t = &self.psets_t;
        let mut rows = Vec::with_capacity(t.guid.len());
        for i in 0..t.guid.len() {
            let mut r = Map::new();
            r.insert("guid".into(), Value::String(t.guid[i].clone()));
            r.insert("pset_name".into(), Value::String(t.pset_name[i].clone()));
            r.insert("prop_name".into(), Value::String(t.prop_name[i].clone()));
            r.insert("value".into(), jstr(&t.value[i]));
            r.insert("value_type".into(), jstr(&t.value_type[i]));
            r.insert("source".into(), Value::String(t.source[i].clone()));
            rows.push(Value::Object(r));
        }
        Value::Array(rows)
    }

    /// `[{guid, qto_name, quantity_name, value, quantity_type,
    /// unit_step_id, source}]` — `model.quantities` as row objects.
    ///
    /// `unit_step_id` is a JSON number (the STEP id of the
    /// `IfcNamedUnit` overriding the project unit) or `null`. Python
    /// carries the column as `float64` because it is nullable, so the
    /// sidecar writes `1234.0`; both parse to the same JS number.
    pub fn quantities_json(&self) -> Value {
        let t = &self.quantities_t;
        let mut rows = Vec::with_capacity(t.guid.len());
        for i in 0..t.guid.len() {
            let mut r = Map::new();
            r.insert("guid".into(), Value::String(t.guid[i].clone()));
            r.insert("qto_name".into(), Value::String(t.qto_name[i].clone()));
            r.insert(
                "quantity_name".into(),
                Value::String(t.quantity_name[i].clone()),
            );
            r.insert("value".into(), jstr(&t.value[i]));
            r.insert(
                "quantity_type".into(),
                Value::String(t.quantity_type[i].clone()),
            );
            r.insert(
                "unit_step_id".into(),
                match t.unit_step_id[i] {
                    Some(id) => json!(id),
                    None => Value::Null,
                },
            );
            r.insert("source".into(), Value::String(t.source[i].clone()));
            rows.push(Value::Object(r));
        }
        Value::Array(rows)
    }

    /// `[{guid, role, layer_index, material_name, layer_thickness_mm,
    /// category, fraction, source}]` — `model.materials` as row objects.
    ///
    /// `layer_index` is `-1` on every non-layered role (that is the
    /// extractor's encoding, not a missing value, so it stays a number).
    /// `layer_thickness_mm` and `fraction` go through [`round10`]: the
    /// sidecar's `DataFrame.to_json` writes doubles at
    /// `double_precision=10` even for `object`-dtype columns, so without
    /// it a thickness would differ from the wheel's in the 11th decimal.
    pub fn materials_json(&self) -> Value {
        let t = &self.materials_t;
        let mut rows = Vec::with_capacity(t.guid.len());
        for i in 0..t.guid.len() {
            let mut r = Map::new();
            r.insert("guid".into(), Value::String(t.guid[i].clone()));
            r.insert("role".into(), Value::String(t.role[i].to_string()));
            r.insert("layer_index".into(), json!(t.layer_index[i]));
            r.insert("material_name".into(), jstr(&t.material_name[i]));
            r.insert(
                "layer_thickness_mm".into(),
                jnum(t.layer_thickness_mm[i].and_then(finite10)),
            );
            r.insert("category".into(), jstr(&t.category[i]));
            r.insert("fraction".into(), jnum(t.fraction[i].and_then(finite10)));
            r.insert("source".into(), Value::String(t.source[i].to_string()));
            rows.push(Value::Object(r));
        }
        Value::Array(rows)
    }

    /// `[{guid, system_name, edition, identification, name, location,
    /// source, assignment_source}]` — `model.classifications` as row
    /// objects.
    ///
    /// `identification` is the normalised column: IFC4
    /// `IfcClassificationReference.Identification` and IFC2x3
    /// `.ItemReference` land in the same place, done once in the core
    /// extractor so the browser inherits it. `source` here is
    /// `IfcClassification.Source` (the publishing body); the
    /// instance-vs-type provenance every other layer calls `source` is
    /// `assignment_source`.
    pub fn classifications_json(&self) -> Value {
        let t = &self.classifications_t;
        let mut rows = Vec::with_capacity(t.guid.len());
        for i in 0..t.guid.len() {
            let mut r = Map::new();
            r.insert("guid".into(), Value::String(t.guid[i].clone()));
            r.insert("system_name".into(), jstr(&t.system_name[i]));
            r.insert("edition".into(), jstr(&t.edition[i]));
            r.insert("identification".into(), jstr(&t.identification[i]));
            r.insert("name".into(), jstr(&t.name[i]));
            r.insert("location".into(), jstr(&t.location[i]));
            r.insert("source".into(), jstr(&t.source[i]));
            r.insert(
                "assignment_source".into(),
                Value::String(t.assignment_source[i].to_string()),
            );
            rows.push(Value::Object(r));
        }
        Value::Array(rows)
    }

    /// `[{guid, entity, name, step_id}]` — `model.type_objects`, every
    /// `IfcTypeObject` the file DECLARES, in file order.
    ///
    /// The companion to `graphJson()`'s per-product `type_guid`. Those
    /// two together are what `typesJson()` cannot answer: it groups the
    /// USED types by name over occurrences, so on a Revit export that
    /// declares 398 type objects of which 339 are referenced under 84
    /// distinct names, it reports 84 entries — and each one's `guid` is
    /// a representative OCCURRENCE's GlobalId, not the type's. Unused
    /// types are this roster minus the distinct non-null `type_guid`
    /// over `graphJson().products`.
    ///
    /// Mesh-free, like the four GH #183 layers: the roster was built in
    /// `fromBytes`, so this is a serialise.
    pub fn type_objects_json(&self) -> Value {
        let mut rows = Vec::with_capacity(self.type_objects.len());
        for t in &self.type_objects {
            let mut r = Map::new();
            r.insert("guid".into(), Value::String(t.guid.clone()));
            r.insert("entity".into(), Value::String(t.entity.clone()));
            r.insert("name".into(), jstr(&t.name));
            r.insert("step_id".into(), json!(t.step_id));
            rows.push(Value::Object(r));
        }
        Value::Array(rows)
    }

    pub fn stats_json(&self) -> Value {
        json!({
            "products_seen": self.counters.products_seen,
            "products_meshed": self.counters.products_meshed,
            "products_deferred": self.counters.products_deferred,
            "triangles": self.counters.triangles,
            "mesh_ms": self.counters.mesh_ms,
            "entity_table_ms": self.counters.entity_table_ms,
            "parse_seconds": self.parse_seconds,
            "size_bytes": self.size_bytes,
        })
    }
}

/// `scripts/generate_sample_sidecars.py::_slug`.
fn slugify(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut in_run = false;
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let cut: String = trimmed.chars().take(48).collect();
    if cut.is_empty() {
        "type".to_string()
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Explicit, so the tests do not lean on the parent module's private
    // `use` leaking through the glob.
    use serde_json::{json, Value};

    // ----- GH #188: far-origin precision --------------------------
    //
    // `far_origin_duct_mm.ifc` is a millimetre model whose site sits at
    // a Norwegian NTM georeference (x ~ 9.2e7 mm, y ~ 1.25e9 mm), where
    // the f32 ulp is 8 mm / 128 mm. Two IfcDuctSegments share one
    // extruded annulus, outer r = 200 mm, inner r = 198 mm, depth
    // 7386.5 mm. Baked in the World frame that circle came back with a
    // 105 mm radius spread and 51% of its triangles at zero area; the
    // Local bake + f64 reposition is what these bounds check.
    //
    // A near-origin fixture cannot catch any of this: there the shift is
    // [0, 0, 0] and every frame agrees.

    /// Duct axis (metres, absolute) and the extrusion's z span, straight
    /// from the fixture's placement numbers: site (92200000, 1247000000,
    /// 0) + duct (319899.3, 315537.2, 127250) mm, duct B 2000 mm east.
    const DUCT_A: (&str, f64, f64) = ("1VluSltEX0WftNcGXN68O9", 92_519.899_3, 1_247_315.537_2);
    const DUCT_B: (&str, f64, f64) = ("1VluSltEX0WftNcGXN68P0", 92_521.899_3, 1_247_315.537_2);
    const Z_BOTTOM_M: f64 = 127.25;
    const Z_TOP_M: f64 = 134.636_5;
    /// `200 mm * arc_area_scale(pi, 16)` — the area-preserving radius the
    /// adaptive tessellator (GH #170) samples a 16-chord semicircle at.
    const OUTER_R_MM: f64 = 200.644_4;

    fn axis_of(guid: &str) -> (f64, f64) {
        if guid == DUCT_A.0 {
            (DUCT_A.1, DUCT_A.2)
        } else if guid == DUCT_B.0 {
            (DUCT_B.1, DUCT_B.2)
        } else {
            panic!("unknown fixture guid {guid}");
        }
    }

    /// Radius spread (mm) of the outer-loop vertices about the duct
    /// axis, the absolute XY bbox centre (m), and the absolute z span.
    /// `positions` are shifted world metres; `shift` puts them back.
    struct Probe {
        outer_n: usize,
        outer_mean_mm: f64,
        outer_spread_mm: f64,
        centre: (f64, f64),
        z_min: f64,
        z_max: f64,
    }

    fn probe(positions: &[f32], shift: [f64; 3], guid: &str) -> Probe {
        let (ax, ay) = axis_of(guid);
        let (mut x0, mut x1) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut z0, mut z1) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut outer: Vec<f64> = Vec::new();
        for c in positions.as_chunks::<3>().0 {
            let x = c[0] as f64 + shift[0];
            let y = c[1] as f64 + shift[1];
            let z = c[2] as f64 + shift[2];
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            z0 = z0.min(z);
            z1 = z1.max(z);
            let r_mm = ((x - ax).powi(2) + (y - ay).powi(2)).sqrt() * 1000.0;
            if r_mm > 199.5 {
                outer.push(r_mm);
            }
        }
        let mn = outer.iter().cloned().fold(f64::INFINITY, f64::min);
        let mx = outer.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        Probe {
            outer_n: outer.len(),
            outer_mean_mm: outer.iter().sum::<f64>() / outer.len() as f64,
            outer_spread_mm: mx - mn,
            centre: ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
            z_min: z0,
            z_max: z1,
        }
    }

    fn zero_area_triangles(positions: &[f32], indices: &[u32], from: usize, count: usize) -> usize {
        let mut n = 0;
        for t in indices[from..from + count].as_chunks::<3>().0 {
            let p = |i: u32| {
                let i = i as usize * 3;
                [
                    positions[i] as f64,
                    positions[i + 1] as f64,
                    positions[i + 2] as f64,
                ]
            };
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let cr = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            if (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt() == 0.0 {
                n += 1;
            }
        }
        n
    }

    fn check_probe(tag: &str, pr: &Probe, guid: &str) {
        let (ax, ay) = axis_of(guid);
        assert!(pr.outer_n >= 32, "{tag}: only {} outer verts", pr.outer_n);
        assert!(
            pr.outer_spread_mm < 0.05,
            "{tag} {guid}: outer radius spread {:.4} mm (mean {:.4}) — f32 quantisation at NTM magnitude",
            pr.outer_spread_mm,
            pr.outer_mean_mm
        );
        assert!(
            (pr.outer_mean_mm - OUTER_R_MM).abs() < 0.01,
            "{tag} {guid}: outer radius {:.4} mm, expected {OUTER_R_MM}",
            pr.outer_mean_mm
        );
        // positions + shift reconstruct absolute world metres: the XY
        // bbox centre is the placement point and the z span is the
        // extrusion, both to 0.1 mm.
        assert!(
            (pr.centre.0 - ax).abs() < 1.0e-4 && (pr.centre.1 - ay).abs() < 1.0e-4,
            "{tag} {guid}: absolute centre ({:.6}, {:.6}) vs placement ({ax}, {ay})",
            pr.centre.0,
            pr.centre.1
        );
        assert!(
            (pr.z_min - Z_BOTTOM_M).abs() < 1.0e-4 && (pr.z_max - Z_TOP_M).abs() < 1.0e-4,
            "{tag} {guid}: absolute z span {:.6}..{:.6} vs {Z_BOTTOM_M}..{Z_TOP_M}",
            pr.z_min,
            pr.z_max
        );
    }

    #[test]
    fn far_origin_duct_streams_precise_positions_and_reports_its_shift() {
        let mut a = fixture("far_origin_duct_mm.ifc");
        let mut batches: Vec<(Vec<Value>, Vec<f32>, Vec<u32>)> = Vec::new();
        {
            let mut emit = |meta: &str, pos: &[f32], idx: &[u32], _progress: &str| {
                batches.push((
                    serde_json::from_str(meta).expect("meta is JSON"),
                    pos.to_vec(),
                    idx.to_vec(),
                ));
                Ok(())
            };
            a.stream_mesh(16, &mut emit).expect("stream runs");
        }

        let shift = a.stream_shift_m;
        assert!(
            shift[0] != 0.0 && shift[1] != 0.0,
            "a georeferenced model must report its shift, got {shift:?}"
        );
        // The shift is the rounded anchor of the first product, in metres.
        assert!((shift[0] - 92_519.899).abs() < 1.0e-9, "{shift:?}");
        assert!((shift[1] - 1_247_315.537).abs() < 1.0e-9, "{shift:?}");

        let mut seen = 0;
        for (meta, positions, indices) in &batches {
            for row in meta {
                let guid = row["guid"].as_str().unwrap();
                let v0 = row["v0"].as_u64().unwrap() as usize;
                let vn = row["vn"].as_u64().unwrap() as usize;
                let i0 = row["i0"].as_u64().unwrap() as usize;
                let in_ = row["in"].as_u64().unwrap() as usize;
                let span = &positions[v0 * 3..(v0 + vn) * 3];
                check_probe("stream", &probe(span, shift, guid), guid);
                assert_eq!(
                    zero_area_triangles(positions, indices, i0, in_),
                    0,
                    "stream {guid}: zero-area triangles"
                );
                seen += 1;
            }
        }
        assert_eq!(seen, 2, "both duct segments stream");
    }

    #[test]
    fn far_origin_duct_batch_pass_retains_precise_meshes() {
        // `toGlb` reads `inner.meshes`, which only the batch pass fills.
        let mut a = fixture("far_origin_duct_mm.ifc");
        a.ensure_meshes();
        let shift = a.stream_shift_m;
        assert!(
            shift[0] != 0.0 && shift[1] != 0.0,
            "the batch pass pins the shift too, got {shift:?}"
        );
        assert_eq!(a.meshes.len(), 2);
        for m in &a.meshes {
            check_probe("batch", &probe(&m.vertices, shift, &m.guid), &m.guid);
            assert_eq!(
                zero_area_triangles(&m.vertices, &m.indices, 0, m.indices.len()),
                0,
                "batch {}: zero-area triangles",
                m.guid
            );
        }
    }

    /// GH #188 review item 1: the shift is a property of the FILE, not
    /// of whichever product the pass emits first. The fixture leads with
    /// a `$`-placement product, whose `mesh_anchor` is the origin.
    #[test]
    fn an_unplaced_first_product_does_not_zero_the_shift() {
        for streamed in [false, true] {
            let mut a = fixture("far_origin_unplaced_first_mm.ifc");
            if streamed {
                let mut emit = |_m: &str, _p: &[f32], _i: &[u32], _g: &str| Ok(());
                a.stream_mesh(16, &mut emit).expect("stream runs");
            } else {
                a.ensure_meshes();
                // The premise: the unplaced box is emitted first.
                assert_eq!(a.meshes[0].guid, "0UnplacedFirstProd00__");
            }
            let s = a.stream_shift_m;
            assert!(
                (s[0] - 92_519.899).abs() < 1.0e-9 && (s[1] - 1_247_315.537).abs() < 1.0e-9,
                "streamed={streamed}: {s:?}"
            );
        }
    }

    /// The same fixture keeps its ducts round on the batch path, which is
    /// what `toGlb` writes from.
    #[test]
    fn an_unplaced_first_product_does_not_break_the_ducts() {
        let mut a = fixture("far_origin_unplaced_first_mm.ifc");
        a.ensure_meshes();
        let shift = a.stream_shift_m;
        let mut checked = 0;
        for m in &a.meshes {
            if m.guid != DUCT_A.0 && m.guid != DUCT_B.0 {
                continue;
            }
            check_probe(
                "unplaced-first",
                &probe(&m.vertices, shift, &m.guid),
                &m.guid,
            );
            checked += 1;
        }
        assert_eq!(checked, 2);
    }

    #[test]
    fn near_origin_models_still_report_a_zero_shift() {
        let mut a = fixture("geom_box.ifc");
        a.ensure_meshes();
        assert_eq!(a.stream_shift_m, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn round10_matches_pandas_double_precision() {
        assert_eq!(round10(40.24124908447266), 40.2412490845);
        assert_eq!(round10(9.307999610900879), 9.3079996109);
    }

    // ----- GH #183: the four data-layer accessors ------------------
    //
    // `tests/fixtures/minimal.ifc` is the smallest fixture that carries
    // all four layers at once (one wall with two Pset_WallCommon
    // properties, a Qto_WallBaseQuantities pair, one IfcMaterial and one
    // NS 3451 IfcClassificationReference), and it is already the one
    // `crates/wasm/test/parity.mjs` smoke-tests.

    fn fixture(name: &str) -> Analysis {
        let path = format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        Analysis::run(&bytes, name).unwrap_or_else(|e| panic!("{name} parses: {e}"))
    }

    fn minimal() -> Analysis {
        fixture("minimal.ifc")
    }

    /// Every row is an object, the rows are exactly as many as
    /// `summaryJson().tables.<table>.rows` promised, and every row's key
    /// set is exactly the advertised column list — no extra key, no
    /// silently dropped one.
    fn check_layer(a: &Analysis, table: &str, rows: &Value, columns: &[&str]) {
        let promised = a.summary_json()["tables"][table]["rows"]
            .as_u64()
            .expect("summary reports a row count") as usize;
        let rows = rows.as_array().expect("an array of row objects");
        assert_eq!(rows.len(), promised, "{table}: rows vs summaryJson");
        assert!(promised > 0, "{table}: fixture carries no rows to check");

        let want: std::collections::BTreeSet<&str> = columns.iter().copied().collect();
        for (i, row) in rows.iter().enumerate() {
            let obj = row
                .as_object()
                .unwrap_or_else(|| panic!("{table}[{i}] is not an object"));
            let got: std::collections::BTreeSet<&str> = obj.keys().map(|k| k.as_str()).collect();
            assert_eq!(got, want, "{table}[{i}]: column key set");
        }
    }

    #[test]
    fn psets_json_matches_summary_and_columns() {
        let a = minimal();
        let rows = a.psets_json();
        check_layer(
            &a,
            "psets",
            &rows,
            &[
                "guid",
                "pset_name",
                "prop_name",
                "value",
                "value_type",
                "source",
            ],
        );
        let r = &rows[0];
        assert_eq!(r["pset_name"], json!("Pset_WallCommon"));
        assert_eq!(r["source"], json!("instance"));
        // The STEP literal is kept verbatim as a string; `value_type`
        // names the type. Never coerced to a JSON boolean (GH #183).
        assert!(r["value"].is_string(), "pset value stays a string");
    }

    #[test]
    fn quantities_json_matches_summary_and_columns() {
        let a = minimal();
        let rows = a.quantities_json();
        check_layer(
            &a,
            "quantities",
            &rows,
            &[
                "guid",
                "qto_name",
                "quantity_name",
                "value",
                "quantity_type",
                "unit_step_id",
                "source",
            ],
        );
        let r = &rows[0];
        assert_eq!(r["qto_name"], json!("Qto_WallBaseQuantities"));
        assert!(r["value"].is_string(), "quantity value stays a string");
        // `IfcQuantityLength('Length',$,$,3.0,$)` has no per-quantity
        // unit override, so the extractor falls back to the project's
        // LENGTHUNIT — `#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.)` in this
        // fixture — exactly as the wheel does (`m.quantities.unit_step_id`
        // is 3.0 there). The Area row has no AREAUNIT to fall back to and
        // is null, which is what the second assertion pins.
        assert_eq!(r["unit_step_id"], json!(3));
        let area = rows
            .as_array()
            .expect("quantities_json is an array")
            .iter()
            .find(|x| x["quantity_name"] == json!("NetSideArea"))
            .expect("NetSideArea row");
        assert_eq!(area["unit_step_id"], Value::Null);
    }

    #[test]
    fn materials_json_matches_summary_and_columns() {
        let a = minimal();
        let rows = a.materials_json();
        check_layer(
            &a,
            "materials",
            &rows,
            &[
                "guid",
                "role",
                "layer_index",
                "material_name",
                "layer_thickness_mm",
                "category",
                "fraction",
                "source",
            ],
        );
        let r = &rows[0];
        assert_eq!(r["material_name"], json!("Concrete"));
        // Non-layered role: -1 is the extractor's encoding, a number.
        assert_eq!(r["layer_index"], json!(-1));
        // A single material has no thickness and no fraction — null, not 0.
        assert_eq!(r["layer_thickness_mm"], Value::Null);
        assert_eq!(r["fraction"], Value::Null);
    }

    #[test]
    fn classifications_json_matches_summary_and_columns() {
        let a = minimal();
        let rows = a.classifications_json();
        check_layer(
            &a,
            "classifications",
            &rows,
            &[
                "guid",
                "system_name",
                "edition",
                "identification",
                "name",
                "location",
                "source",
                "assignment_source",
            ],
        );
        let r = &rows[0];
        // IFC4 `Identification` and IFC2x3 `ItemReference` normalise onto
        // this one column in the core extractor — the whole reason a
        // browser-side IDS ClassificationFacet is reachable (GH #183).
        assert_eq!(r["identification"], json!("232.1"));
        assert_eq!(r["name"], json!("Yttervegger"));
        assert_eq!(r["assignment_source"], json!("instance"));
    }

    // ----- type objects: declared roster + per-product type_guid ---
    //
    // `tests/fixtures/type_objects.ifc` declares three IfcTypeObjects
    // and references exactly one, so declared (3) ≠ used (1) ≠ distinct
    // used NAMES (1 of the 2 'Ubrukt' names) — the three numbers that
    // collapsed into `typesJson()`'s single roster before this. Two of
    // the walls are typed through the relation, one carries only an
    // ObjectType string, one is untyped.

    fn typed() -> Analysis {
        fixture("type_objects.ifc")
    }

    #[test]
    fn type_objects_json_matches_summary_and_columns() {
        let a = typed();
        let rows = a.type_objects_json();
        check_layer(
            &a,
            "type_objects",
            &rows,
            &["guid", "entity", "name", "step_id"],
        );
        let rows = rows.as_array().expect("an array").clone();
        assert_eq!(rows.len(), 3, "three declared type objects");
        // File order, not HashMap order.
        assert_eq!(rows[0]["step_id"], json!(80));
        assert_eq!(rows[0]["guid"], json!("TYP0000000000000000001"));
        assert_eq!(rows[0]["name"], json!("Yttervegg 300"));
        assert_eq!(rows[2]["step_id"], json!(82));
        // The core's spelling map is backed by the PRODUCT whitelist, so
        // a `*Type` class takes the first-letter-only fallback. Pinned
        // as the wheel reports it rather than prettified here (GH #186);
        // when the core gains a full-schema entity list this assertion
        // is the one that says so.
        assert_eq!(rows[0]["entity"], json!("IfcWalltype"));
        assert_eq!(rows[2]["entity"], json!("IfcDoortype"));
    }

    #[test]
    fn product_type_guid_points_into_the_declared_roster() {
        let mut a = typed();
        let graph = a.graph_json();
        let products = graph["products"].as_array().expect("graph.products");
        let by_name = |n: &str| -> Value {
            products
                .iter()
                .find(|p| p["name"] == json!(n))
                .unwrap_or_else(|| panic!("no product named {n}"))
                .clone()
        };

        let w1 = by_name("Wall-001");
        assert_eq!(w1["type_source"], json!("ifctype"));
        assert_eq!(w1["type_guid"], json!("TYP0000000000000000001"));
        assert_eq!(w1["type_name"], json!("Yttervegg 300"));

        // An ObjectType string names a type the file never declared as
        // an object: `type_name` falls back to it, `type_guid` must NOT
        // be invented from the name.
        let w4 = by_name("Wall-004");
        assert_eq!(w4["type_source"], json!("objecttype"));
        assert_eq!(w4["type_name"], json!("Yttervegg 250"));
        assert_eq!(w4["type_guid"], Value::Null);

        // Every non-null type_guid resolves — the two surfaces are built
        // from the same index vectors, so a dangling one is a bug, not
        // model data.
        let declared: std::collections::BTreeSet<String> = a
            .type_objects_json()
            .as_array()
            .expect("roster")
            .iter()
            .map(|t| t["guid"].as_str().unwrap().to_string())
            .collect();
        let used: std::collections::BTreeSet<String> = products
            .iter()
            .filter_map(|p| p["type_guid"].as_str().map(str::to_string))
            .collect();
        assert!(used.is_subset(&declared), "{used:?} ⊄ {declared:?}");
        assert_eq!(used.len(), 1, "one type object is referenced");
        assert_eq!(declared.len() - used.len(), 2, "two declared types unused");
    }

    /// The reason the roster had to be exposed: `typesJson()` answers a
    /// different question, in a different key space. Its `guid` is an
    /// occurrence's, and it cannot see a type nothing points at.
    #[test]
    fn types_json_is_the_used_roster_keyed_by_an_occurrence() {
        let a = typed();
        let types = a.types_json("test");
        let entries = types["types"].as_array().expect("types");
        // Two NAMES over the occurrences: the declared type's, and the
        // bare ObjectType string on Wall-004 — which is not a type
        // object at all. Neither count is the declared roster's 3, and
        // the second entry has no type object behind it whatsoever.
        assert_eq!(entries.len(), 2);
        let used = entries
            .iter()
            .find(|e| e["type_name"] == json!("Yttervegg 300"))
            .expect("the declared type's group");
        assert_eq!(used["count"], json!(3));
        // An occurrence GlobalId, not the IfcWallType's — the trap this
        // whole pair of surfaces exists to close.
        assert_eq!(used["guid"], json!("7XvctVUKr0kugbFTf53O9L"));
        assert_ne!(used["guid"], json!("TYP0000000000000000001"));
        assert_eq!(a.type_objects.len(), 3);
    }

    /// A file with no type objects at all (KNM_RIB on Mottakskontroll is
    /// the real one) must serialise an empty array, not `null` and not a
    /// row of nulls.
    #[test]
    fn type_objects_json_is_empty_not_null_without_types() {
        let mut a = minimal();
        assert_eq!(a.type_objects_json(), json!([]));
        assert_eq!(a.summary_json()["tables"]["type_objects"]["rows"], json!(0));
        for p in a.graph_json()["products"].as_array().expect("products") {
            assert_eq!(p["type_guid"], Value::Null);
        }
    }

    // ----- GH #181: StoreyRow.elevation_m -------------------------
    //
    // The 1000x trap. `elevation` is the raw file-unit attribute and
    // must stay raw; `elevation_m` is the metres view. The fixtures are
    // GH #180's: `storey_mm.ifc` declares `.MILLI. .METRE.` and a storey
    // at `3000.`, `broken_conversion_unit.ifc` declares a LENGTHUNIT
    // whose conversion chain does not resolve.

    fn storeys_of(a: &mut Analysis) -> Vec<Value> {
        a.ensure_stats();
        a.graph_json()["storeys"]
            .as_array()
            .expect("graph.storeys")
            .clone()
    }

    #[test]
    fn storey_elevation_m_is_metres_not_file_units() {
        let mut a = fixture("storey_mm.ifc");
        let storeys = storeys_of(&mut a);
        let upper = storeys
            .iter()
            .find(|s| s["name"] == json!("Plan 02"))
            .expect("Plan 02");
        // Raw attribute kept verbatim, in the file's own millimetres.
        assert_eq!(upper["elevation"], json!(3000.0));
        // …and the metres view next to it. 3000 mm = 3 m.
        assert_eq!(upper["elevation_m"], json!(3.0));
    }

    #[test]
    fn storey_elevation_m_is_null_when_the_unit_is_unresolved() {
        let mut a = fixture("broken_conversion_unit.ifc");
        let storeys = storeys_of(&mut a);
        let s = &storeys[0];
        // The elevation itself is 0.0, which is exactly why this case
        // needs its own test: `unwrap_or(1.0)` on the unit scale would
        // yield a perfectly plausible `0.0` instead of "unknown".
        assert_eq!(s["elevation"], json!(0.0));
        assert_eq!(s["elevation_m"], Value::Null);
    }

    #[test]
    fn storey_columns_carry_elevation_m_last() {
        let a = minimal();
        // Python's list is `StoreyRow.__dataclass_fields__` order, and
        // `elevation_m` is the defaulted field, so it comes last.
        assert_eq!(
            a.summary_json()["tables"]["storeys"]["columns"],
            json!(["guid", "name", "elevation", "building_guid", "elevation_m"])
        );
    }

    // ----- GH #184: skipped_product_types --------------------------

    #[test]
    fn skipped_product_types_explains_a_silent_zero() {
        let a = fixture("unlisted_product.ifc");
        let summary = a.summary_json();
        // The whole point: zero products, and a reason.
        assert_eq!(summary["products"], json!(0));
        assert_eq!(
            summary["skipped_product_types"],
            json!({ "IFCTUBEBUNDLE": 1 }),
            "STEP spelling, not the wheel's title case — see the comment \
             in summary_json()"
        );
    }

    #[test]
    fn skipped_product_types_is_an_empty_object_on_a_covered_file() {
        let a = minimal();
        // Empty OBJECT, not null and not absent: a consumer can branch on
        // `Object.keys(...).length` without a presence check.
        assert_eq!(a.summary_json()["skipped_product_types"], json!({}));
    }

    #[test]
    fn slug_matches_python_regex() {
        assert_eq!(slugify("OpeningElement-Opening"), "openingelement-opening");
        assert_eq!(
            slugify("WallStandardCase-Basic Wall:Interior - Partition (92mm Stud):128360"),
            "wallstandardcase-basic-wall-interior-partition-9"
        );
        assert_eq!(slugify("---"), "type");
    }
}
