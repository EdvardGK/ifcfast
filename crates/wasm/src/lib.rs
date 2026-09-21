//! `ifcfast-wasm` — ifcfast in the browser (GH #172).
//!
//! One class, `IfcModel`, mirroring what `ifcfast.com`'s instrument
//! already consumes from the baked Duplex sample: the four sidecar JSON
//! shapes `scripts/generate_sample_sidecars.py` writes, plus a `.glb`.
//! Same shapes ⇒ a dropped file renders through the existing UI.
//!
//! The dropped file never leaves the tab: everything here runs on bytes
//! the caller already holds.
//!
//! v1 limits, stated rather than hidden:
//!
//!   * **No `cut_openings`.** The net-boolean path is `manifold-csg`
//!     (C++) and does not cross-compile to wasm, so doors / windows are
//!     revealed as their own solids instead of cut out of the host.
//!     `m.to_gltf()` on the desktop wheel defaults to `cut_openings=True`,
//!     so a browser `.glb` has more nodes than the baked `duplex.glb`.
//!   * **No per-type mini-glbs.** `typesJson()` carries the type roster
//!     with `glb: ""` / `bytes: 0`; the gallery degrades to names+counts.
//!   * **Single-threaded.** No rayon on the web target; the mesh pass
//!     takes the core's existing T=1 serial path.
//!
//! GH #183 adds the four long-format data layers — [`IfcModel::psets_json`],
//! [`IfcModel::quantities_json`], [`IfcModel::materials_json`],
//! [`IfcModel::classifications_json`] — as row-object JSON with the same
//! columns and the same normalisation as the Python DataFrames. They were
//! parsed all along (`summaryJson().tables` reported their row counts);
//! they simply had no accessor. A browser-side IDS `PropertyFacet` /
//! `ClassificationFacet` reads them.
//!
//! v2 (GH #172, streaming geometry): [`IfcModel::from_bytes`] no longer
//! meshes. It parses, indexes and runs the extractors — everything the
//! identity + roster surfaces need — and the tessellation is either
//! streamed batch-by-batch through [`IfcModel::stream_meshes`] (what the
//! ifcfast.com instrument does: the model builds up in front of the user)
//! or pulled in on demand by the first geometry-derived surface, exactly
//! as v1 computed it.

mod analysis;

use analysis::Analysis;
use ifcfast_core::mesh::gltf::{self, WriteOptions};
use ifcfast_core::mesh::ProductMesh;
use wasm_bindgen::prelude::*;

/// Render a thrown JS value as a message for the Rust-side error path.
fn describe(err: JsValue) -> String {
    if let Some(s) = err.as_string() {
        return s;
    }
    js_sys::Reflect::get(&err, &JsValue::from_str("message"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| format!("{err:?}"))
}

/// Version string reported in `typesJson().generated_with`. Tracks the
/// workspace crate version, which is what `ifcfast.__version__` reports
/// on the Python side.
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[wasm_bindgen]
pub struct IfcModel {
    inner: Analysis,
}

#[wasm_bindgen]
impl IfcModel {
    /// Parse from bytes — plain STEP or `.ifczip`, dispatched on magic
    /// bytes exactly like the native `source::open`. Throws an `Error`
    /// carrying the core's message (truncated file, no STEP trailer,
    /// broken zip) rather than serving a partial model.
    ///
    /// v2: parse + index + extractors only. No tessellation — call
    /// [`IfcModel::stream_meshes`] for the incremental geometry, or just
    /// touch any geometry-derived surface and the v1 batch pass runs
    /// itself.
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(bytes: &[u8], name: &str) -> Result<IfcModel, JsError> {
        match Analysis::run(bytes, name) {
            Ok(inner) => Ok(IfcModel { inner }),
            Err(msg) => Err(JsError::new(&msg)),
        }
    }

    /// `<prefix>.summary.json` — identity, counts, top types, and the
    /// shape + loaded-state of every table.
    ///
    /// The one surface that never triggers a mesh pass: it is what the
    /// drop zone shows the instant parsing finishes. Its `drift` and
    /// `segments` tables therefore report `loaded: false` / `rows: 0`
    /// until geometry has actually run.
    #[wasm_bindgen(js_name = summaryJson)]
    pub fn summary_json(&self) -> String {
        self.inner.summary_json().to_string()
    }

    /// `<prefix>.graph.json` — per-product rows (measures joined from the
    /// mesh pass) plus the spatial graph.
    ///
    /// Runs the batch mesh pass if no geometry has been produced yet;
    /// after `streamMeshes` it reuses the streamed per-product stats.
    #[wasm_bindgen(js_name = graphJson)]
    pub fn graph_json(&mut self) -> String {
        self.inner.ensure_stats();
        self.inner.graph_json().to_string()
    }

    /// `<prefix>.qto.json` — per-entity-class aggregates over the same
    /// per-product mesh stats.
    #[wasm_bindgen(js_name = qtoJson)]
    pub fn qto_json(&mut self) -> String {
        self.inner.ensure_stats();
        self.inner.qto_json().to_string()
    }

    /// `[{guid, pset_name, prop_name, value, value_type, source}]` —
    /// every property row, long format, exactly `model.psets` (GH #183).
    ///
    /// Mesh-free: the extractors already ran in `fromBytes`, so this is
    /// a serialise, not a computation.
    ///
    /// `value` is the STEP literal as a **string** (or `null`), with
    /// `value_type` naming the IFC type — the wheel does not coerce it
    /// either, and a browser that parsed `"3.0"` into `3.0` would
    /// disagree with the desktop for the same file. `source` is
    /// `"instance"` or `"type"`: type-inherited properties are included,
    /// with instance winning on a name collision.
    ///
    /// This is the payload an IDS `PropertyFacet` needs. On a large
    /// model it is also the biggest string this API hands out — one JSON
    /// document for every property of every product.
    #[wasm_bindgen(js_name = psetsJson)]
    pub fn psets_json(&self) -> String {
        self.inner.psets_json().to_string()
    }

    /// `[{guid, qto_name, quantity_name, value, quantity_type,
    /// unit_step_id, source}]` — `model.quantities` (GH #183).
    ///
    /// Authored quantities, i.e. what the exporter wrote into
    /// `Qto_*`. Not to be confused with `qtoJson()`, which is ifcfast's
    /// own per-class aggregate over the mesh pass. `value` is a string
    /// for the same reason as in `psetsJson`.
    #[wasm_bindgen(js_name = quantitiesJson)]
    pub fn quantities_json(&self) -> String {
        self.inner.quantities_json().to_string()
    }

    /// `[{guid, role, layer_index, material_name, layer_thickness_mm,
    /// category, fraction, source}]` — `model.materials` (GH #183).
    ///
    /// The long-format layer table. `graphJson()`'s per-product
    /// `materials` array is a name rollup of these rows; this is the
    /// rows themselves, with per-layer thickness in millimetres
    /// (unit-normalised by the extractor) and `layer_index` ordering
    /// them through the wall.
    #[wasm_bindgen(js_name = materialsJson)]
    pub fn materials_json(&self) -> String {
        self.inner.materials_json().to_string()
    }

    /// `[{guid, system_name, edition, identification, name, location,
    /// source, assignment_source}]` — `model.classifications` (GH #183).
    ///
    /// `identification` is the normalised code: IFC4
    /// `IfcClassificationReference.Identification` and IFC2x3
    /// `.ItemReference` both land here, so an NS 3451 lookup is one
    /// column regardless of schema. Watch the two provenance columns:
    /// `source` is `IfcClassification.Source` (the publishing body),
    /// `assignment_source` is the `"instance"` / `"type"` flag the other
    /// three layers call `source`.
    #[wasm_bindgen(js_name = classificationsJson)]
    pub fn classifications_json(&self) -> String {
        self.inner.classifications_json().to_string()
    }

    /// `types/manifest.json` — the type roster. `glb` / `bytes` are empty
    /// in v1; see the module docs.
    #[wasm_bindgen(js_name = typesJson)]
    pub fn types_json(&self) -> String {
        self.inner.types_json(VERSION).to_string()
    }

    /// glTF binary, same writer as `m.to_gltf()`:
    /// `KHR_mesh_quantization`, `EXT_mesh_gpu_instancing` (when
    /// `instancing` is on), `node.extras.guid`, and GUID-named materials
    /// when `perProductMaterials` is on (GH #146).
    ///
    /// Two product classes are held back, matching what the desktop
    /// `to_gltf()` default produces:
    ///
    ///   * `IfcSpace` — translucent space volumes envelop the building
    ///     and read as clutter in a viewport. The sidecar generator
    ///     carves a space-free subset before exporting for the same
    ///     reason.
    ///   * products on the `RelatedOpeningElement` side of an
    ///     `IfcRelVoidsElement` — subtracted geometry, never element
    ///     geometry. `cut_openings` folds them into the host; with no
    ///     boolean kernel on wasm the honest approximation is to drop
    ///     them rather than render a door-shaped solid inside the wall.
    ///     The hole itself is therefore NOT cut in v1.
    ///
    /// Nothing is hidden from the data surfaces: openings and spaces
    /// keep their rows in `graphJson` / `qtoJson` and their counts in
    /// `statsJson`.
    #[wasm_bindgen(js_name = toGlb)]
    pub fn to_glb(
        &mut self,
        per_product_materials: Option<bool>,
        instancing: Option<bool>,
    ) -> Result<Vec<u8>, JsError> {
        // The writer needs the retained meshes, which only the batch
        // pass keeps — a streamed model released them product by product.
        self.inner.ensure_meshes();
        // The retained meshes are shifted world metres (GH #188); the
        // writer needs the shift both to self-describe the frame in
        // `asset.extras.ifcfast.global_shift` and to place instanced
        // nodes, and `unit_scale` because `parts[].local_vertices` —
        // what an instance group shares — are still in native units.
        let options = WriteOptions {
            instancing: instancing.unwrap_or(true),
            per_product_materials: per_product_materials.unwrap_or(true),
            global_shift: self.inner.stream_shift_m,
            unit_scale: self.inner.unit_scale_f64,
        };
        // `write_with_options` takes a slice of owned meshes, so the
        // clone is confined to what is actually emitted. Empty meshes are
        // dropped for the same reason `write_gltf`'s sink drops them:
        // a stripped-cutter product can end up with no triangles.
        let emitted: Vec<ProductMesh> = self
            .inner
            .meshes
            .iter()
            .filter(|m| {
                m.entity != "IfcSpace"
                    && !self.inner.opening_guids.contains(&m.guid)
                    && !m.vertices.is_empty()
                    && !m.indices.is_empty()
            })
            .cloned()
            .collect();
        let mut out: Vec<u8> = Vec::with_capacity(1 << 20);
        gltf::write_with_options(&emitted, &options, &mut out)
            .map_err(|e| JsError::new(&format!("ifcfast: glTF write failed: {e}")))?;
        Ok(out)
    }

    /// `{tag: count}` — what the mesh pass saw, including
    /// `unhandled:IFCXXX` markers for representations it could not
    /// tessellate (GH #166).
    #[wasm_bindgen(js_name = bySourceJson)]
    pub fn by_source_json(&mut self) -> String {
        self.inner.ensure_stats();
        self.inner.by_source_json().to_string()
    }

    /// Engine counters for the UI: products seen / meshed / deferred,
    /// triangles, mesh milliseconds.
    #[wasm_bindgen(js_name = statsJson)]
    pub fn stats_json(&mut self) -> String {
        self.inner.ensure_stats();
        self.inner.stats_json().to_string()
    }

    /// Run the mesh pass once, streaming merged batches through `cb` as
    /// products are tessellated (GH #172 v2).
    ///
    /// `cb(metaJson, positions, indices, progressJson)` is called
    /// synchronously from inside the pass, every `productsPerBatch`
    /// drawable products and once more for the tail. A Web Worker
    /// `postMessage`s from it, so the main thread paints the model as it
    /// builds instead of waiting for one baked GLB.
    ///
    ///   * `positions` — `Float32Array`, world METRES minus
    ///     [`IfcModel::stream_shift_json`], repositioned from the Local
    ///     bake in f64 so a georeferenced millimetre model keeps its
    ///     round MEP round (GH #188). A **copy** into JS memory,
    ///     not a view: a view into the wasm heap would be detached by the
    ///     next allocation the pass makes, and the callback is free to
    ///     keep (or transfer) what it is handed.
    ///   * `indices` — `Uint32Array`, batch-local (already offset by each
    ///     product's `v0`), so a batch uploads as one merged
    ///     `BufferGeometry`.
    ///   * `metaJson` — `[{guid, entity, storey_guid, type_name, m3, m2,
    ///     tri, v0, vn, i0, in, rgba}]`. `v0`/`vn` are the product's
    ///     vertex offset/count inside `positions` (xyz triples),
    ///     `i0`/`in` its index offset/count inside `indices`, and `rgba`
    ///     is `mesh::gltf::resolve_product_color` — the same cascade the
    ///     glTF writer paints with, not a second implementation of it.
    ///     `m3`/`m2`/`tri` are the v1 per-product measures.
    ///   * `progressJson` — `{seen, meshed, total}`: products handed to
    ///     the sink so far, of those the ones that had drawable geometry
    ///     after the cutter strip, and the index's product count.
    ///
    /// Products with no geometry left after the synthetic half-space
    /// cutters are stripped (GH #66) still get their QTO row; they just
    /// never reach a batch. Nothing is filtered by entity — `IfcSpace`
    /// and opening solids stream like everything else, tagged in `meta`,
    /// so the viewer decides what to draw. (`toGlb` still holds them
    /// back; that is a glTF-export choice, not a data one.)
    ///
    /// Throwing from `cb` aborts the stream and surfaces as an `Error`
    /// here; the per-product tables stay consistent for whatever ran.
    #[wasm_bindgen(js_name = streamMeshes)]
    pub fn stream_meshes(
        &mut self,
        products_per_batch: usize,
        cb: &js_sys::Function,
    ) -> Result<(), JsError> {
        let mut emit = |meta: &str,
                        positions: &[f32],
                        indices: &[u32],
                        progress: &str|
         -> Result<(), String> {
            let pos = js_sys::Float32Array::from(positions);
            let idx = js_sys::Uint32Array::from(indices);
            cb.call4(
                &JsValue::NULL,
                &JsValue::from_str(meta),
                &pos,
                &idx,
                &JsValue::from_str(progress),
            )
            .map(|_| ())
            .map_err(|e| format!("ifcfast: streamMeshes callback threw: {}", describe(e)))
        };
        self.inner
            .stream_mesh(products_per_batch, &mut emit)
            .map_err(|m| JsError::new(&m))
    }

    /// `[sx, sy, sz]` in METRES — the model-wide global shift every
    /// position handed out was reduced by. Add it back for absolute world
    /// coordinates.
    ///
    /// Valid after **either** mesh pass: `streamMeshes()` positions and
    /// the `toGlb()` GLB share one value, and any surface that triggers
    /// the batch pass (`graphJson` / `qtoJson` / `statsJson` / `toGlb`)
    /// pins it too. `[0, 0, 0]` before any mesh pass has run and for
    /// every model within 10 km of the origin; same rule (and same
    /// value) as `_core.extract_meshes`' `global_shift` and
    /// `m.to_gltf()`'s `global_shift` stat.
    ///
    /// A near-origin model cannot exercise this — there the shift is
    /// zero and every frame agrees (GH #188).
    #[wasm_bindgen(js_name = streamShiftJson)]
    pub fn stream_shift_json(&self) -> String {
        let s = self.inner.stream_shift_m;
        format!("[{},{},{}]", s[0], s[1], s[2])
    }

    /// Frame-neutral alias for [`IfcModel::stream_shift_json`] — the
    /// same three metres, for callers that never streamed (`toGlb` /
    /// `qtoJson` pin it just as well). Both names stay.
    #[wasm_bindgen(js_name = shiftJson)]
    pub fn shift_json(&self) -> String {
        self.stream_shift_json()
    }
}
