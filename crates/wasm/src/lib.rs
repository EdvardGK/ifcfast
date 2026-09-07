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

mod analysis;

use analysis::Analysis;
use ifcfast_core::mesh::gltf::{self, WriteOptions};
use ifcfast_core::mesh::ProductMesh;
use wasm_bindgen::prelude::*;

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
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(bytes: &[u8], name: &str) -> Result<IfcModel, JsError> {
        match Analysis::run(bytes, name) {
            Ok(inner) => Ok(IfcModel { inner }),
            Err(msg) => Err(JsError::new(&msg)),
        }
    }

    /// `<prefix>.summary.json` — identity, counts, top types, and the
    /// shape + loaded-state of every table.
    #[wasm_bindgen(js_name = summaryJson)]
    pub fn summary_json(&self) -> String {
        self.inner.summary_json().to_string()
    }

    /// `<prefix>.graph.json` — per-product rows (measures joined from the
    /// mesh pass) plus the spatial graph.
    #[wasm_bindgen(js_name = graphJson)]
    pub fn graph_json(&self) -> String {
        self.inner.graph_json().to_string()
    }

    /// `<prefix>.qto.json` — per-entity-class aggregates over the same
    /// per-product mesh stats.
    #[wasm_bindgen(js_name = qtoJson)]
    pub fn qto_json(&self) -> String {
        self.inner.qto_json().to_string()
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
        &self,
        per_product_materials: Option<bool>,
        instancing: Option<bool>,
    ) -> Result<Vec<u8>, JsError> {
        let options = WriteOptions {
            instancing: instancing.unwrap_or(true),
            per_product_materials: per_product_materials.unwrap_or(true),
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
    pub fn by_source_json(&self) -> String {
        self.inner.by_source_json().to_string()
    }

    /// Engine counters for the UI: products seen / meshed / deferred,
    /// triangles, mesh milliseconds.
    #[wasm_bindgen(js_name = statsJson)]
    pub fn stats_json(&self) -> String {
        self.inner.stats_json().to_string()
    }
}
