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
use ifcfast_core::mesh::stats::ProductStats;
use ifcfast_core::mesh::{self, ProductMesh, ProductSink};
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
const CACHE_SCHEMA_VERSION: u32 = 31;
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
fn round10_opt(v: f32) -> Option<f64> {
    let v = v as f64;
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
    pub type_name: Option<String>,
    pub type_source: &'static str,
}

pub struct StoreyRow {
    pub guid: String,
    pub name: Option<String>,
    pub elevation: Option<f64>,
    pub building_guid: Option<String>,
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
    pub type_object_count: usize,

    pub pset_rows: usize,
    pub quantity_rows: usize,
    pub material_rows: usize,
    pub classification_rows: usize,
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
    /// Model-wide global shift in METRES, pinned by the streaming pass
    /// from the first emitted product's `mesh_anchor` — the same rule
    /// `_core.extract_meshes` uses for its `global_shift`. Streamed
    /// positions are world metres MINUS this; add it back for absolute
    /// coordinates. `[0, 0, 0]` until the stream has started, and for
    /// every near-origin model.
    pub stream_shift_m: [f64; 3],
}

/// What [`Analysis::stream_mesh`] hands each finished batch to:
/// `(metaJson, positions, indices, progressJson)`. The `ifcfast-wasm`
/// layer wraps the JS callback in one of these so `analysis` stays free
/// of `wasm-bindgen` types (and unit-testable off the web target).
pub type BatchEmit<'e> = &'e mut dyn FnMut(&str, &[f32], &[u32], &str) -> Result<(), String>;

/// Port of `lib.rs::python::global_shift_for` (that one is behind the
/// `python` feature, so it is not reachable from the wasm build).
///
/// Returns the rounded world origin, in MODEL UNITS, only when the model
/// genuinely sits far from the origin (> 10 km); below that f32 already
/// resolves the coordinate finely enough and near-origin models keep
/// absolute coordinates unchanged.
fn global_shift_for(world_origin: &[f64; 3], unit_scale: f32) -> [f64; 3] {
    const THRESHOLD_M: f64 = 1.0e4;
    let us = unit_scale as f64;
    let max_m = world_origin
        .iter()
        .map(|c| (c * us).abs())
        .fold(0.0_f64, f64::max);
    if max_m > THRESHOLD_M {
        [
            world_origin[0].round(),
            world_origin[1].round(),
            world_origin[2].round(),
        ]
    } else {
        [0.0, 0.0, 0.0]
    }
}

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
            storeys.push(StoreyRow {
                guid: idx.storey_guid[i].clone(),
                name: idx.storey_name[i].clone(),
                elevation: idx.storey_elevation[i],
                building_guid: None,
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

        // Type linkage (IfcRelDefinesByType → type guid/name).
        let mut type_meta_by_step: HashMap<u64, Option<String>> = HashMap::new();
        for (tsid, tname) in idx
            .type_object_step_id
            .iter()
            .zip(idx.type_object_name.iter())
        {
            type_meta_by_step.insert(*tsid, tname.clone());
        }
        let mut product_type_by_step: HashMap<u64, Option<String>> = HashMap::new();
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
            let (type_name, type_source) = match product_type_by_step.get(&sid) {
                Some(tn) => (tn.clone(), "ifctype"),
                None if truthy(&object_type) => (object_type.clone(), "objecttype"),
                None => (None, "none"),
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
            type_object_count: idx.type_object_step_id.len(),
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
            pset_rows: psets_t.guid.len(),
            quantity_rows: quantities_t.guid.len(),
            material_rows: materials_t.guid.len(),
            classification_rows: classifications_t.guid.len(),
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

    /// v1's pass, verbatim: the same call `_core.analyse_drift` makes —
    /// a World-frame streaming pass collected into a `Vec`, then the
    /// synthetic half-space stand-in slabs stripped before any measure is
    /// taken (GH #66) — plus the metre rescale `write_gltf`'s sink does.
    pub fn batch_mesh(&mut self) {
        let unit_scale = self.unit_scale_f64 as f32;
        let (meshes, drift, segment_rows, counters) = {
            let buf = self.source.as_bytes();
            let (mut meshes, mesh_stats) = mesh::mesh_ifc(buf);
            for m in &mut meshes {
                mesh::strip_synthetic_cutters(m);
            }
            let mut drift: Vec<DriftRow> = Vec::with_capacity(meshes.len());
            let mut segment_rows = 0usize;
            for m in &meshes {
                segment_rows += m.segments.len();
                drift.push(drift_row(m, unit_scale));
            }

            // Scale the retained meshes into metres — exactly what
            // `write_gltf`'s sink does before handing them to the writer.
            let us = unit_scale as f64;
            for m in &mut meshes {
                for v in m.vertices.iter_mut() {
                    *v = (*v as f64 * us) as f32;
                }
            }
            (meshes, drift, segment_rows, counters_from(&mesh_stats))
        };

        self.meshes = meshes;
        self.set_geometry(drift, segment_rows, counters);
        self.mesh_state = MeshState::Batched;
    }

    /// Streaming pass (GH #172 v2). Drives the core's sink ONCE and hands
    /// `emit` a batch every `products_per_batch` emitted products, plus a
    /// final partial batch.
    ///
    /// The per-product stats are computed from the same World-frame,
    /// cutter-stripped mesh [`Analysis::batch_mesh`] measures, in the same
    /// emission order, so `qto_json` / `graph_json` after a stream are
    /// byte-identical to the batch path's.
    ///
    /// Positions are world METRES minus [`Analysis::stream_shift_m`];
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
                us: unit_scale as f64,
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
            let mesh_stats = mesh::mesh_ifc_streaming(buf, &mut sink);
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

/// One `drift` row from one cutter-stripped, World-frame product mesh.
///
/// `analyse_drift` does the unit rescale in f32 and Python widens the
/// result; keeping the arithmetic in f32 here means the 10-decimal
/// rounding lands on the same value.
fn drift_row(m: &ProductMesh, unit_scale: f32) -> DriftRow {
    let s = ProductStats::from_mesh(m, unit_scale);
    let us_len = unit_scale;
    let us_area = unit_scale * unit_scale;
    let us_vol = unit_scale * unit_scale * unit_scale;
    DriftRow {
        guid: s.guid.clone(),
        surface_area_m2: round10_opt(s.surface_area * us_area),
        volume_abs_m3: round10_opt(s.volume.abs() * us_vol),
        max_extent_m: round10_opt(s.max_extent * us_len),
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
    unit_scale: f32,
    us: f64,
    per_batch: usize,
    total: usize,
    /// guid → (storey_guid, type_name) as the graph resolves them.
    meta_of: HashMap<String, (Value, Value)>,
    /// Model-wide shift in MODEL UNITS, pinned from the first emitted
    /// product's `mesh_anchor` (the `extract_meshes` rule).
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
    fn on_product(&mut self, mut mesh: ProductMesh) {
        self.seen += 1;
        // Same order as the batch pass: strip the synthetic half-space
        // stand-in slabs FIRST, then measure (GH #66).
        mesh::strip_synthetic_cutters(&mut mesh);
        self.segment_rows += mesh.segments.len();
        let row = drift_row(&mesh, self.unit_scale);
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
            .get_or_insert_with(|| global_shift_for(&mesh.mesh_anchor, self.unit_scale));

        let v0 = self.positions.len() / 3;
        let vn = mesh.vertices.len() / 3;
        self.positions.reserve(mesh.vertices.len());
        for c in mesh.vertices.as_chunks::<3>().0 {
            for k in 0..3 {
                self.positions
                    .push(((c[k] as f64 - shift[k]) * self.us) as f32);
            }
        }
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
    ("storeys", &["guid", "name", "elevation", "building_guid"]),
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
            table_meta("type_objects", self.type_object_count),
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
        tables.insert("psets".into(), table_meta("psets", self.pset_rows));
        tables.insert(
            "quantities".into(),
            table_meta("quantities", self.quantity_rows),
        );
        tables.insert(
            "materials".into(),
            table_meta("materials", self.material_rows),
        );
        tables.insert(
            "classifications".into(),
            table_meta("classifications", self.classification_rows),
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

    #[test]
    fn round10_matches_pandas_double_precision() {
        assert_eq!(round10(40.24124908447266), 40.2412490845);
        assert_eq!(round10(9.307999610900879), 9.3079996109);
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
