# ifcfast in the browser — client-side "drop your IFC" (GH #172)

Owner decision (Ed, 2026-09-07): client-side only, no upload backend.
The dropped file never leaves the tab.

## Deliverable

`crates/wasm` → npm-consumable `pkg/` (wasm-bindgen, `--target web`)
exposing exactly what the ifcfast.com instrument (chapter 06 of the
landing, `app/mockups/ab/page.tsx`) already consumes from the baked
sample: the JSON shapes `scripts/generate_sample_sidecars.py` writes
plus a `.glb`. Same shapes ⇒ the same instrument renders a dropped file
with zero UI rework.

## JS contract (TypeScript, `ifcfast-site/lib/ifcfast-wasm.d.ts` mirrors this)

```ts
export default function init(module?: URL | Request | Response | BufferSource): Promise<void>;

export class IfcModel {
  /** Parse from bytes (plain STEP or .ifczip — magic-byte dispatch). Throws Error with the core's message on failure. */
  static fromBytes(bytes: Uint8Array, name: string): IfcModel;
  /** duplex.summary.json shape: path (= `name`), size_bytes, schema, project_name, authoring_app,
      unit_scale, length_unit, cache_key (content hash), products, storeys, type_counts_total, parse_seconds. */
  summaryJson(): string;
  /** duplex.graph.json shape: products[{guid, entity, name, storey_guid, storey_name, type_name,
      m3, m2, m3_direct, m2_direct, lm, lm_direct, materials[]|null, predefined_type, tag, …}],
      storeys[{guid,name,elevation}], contained_in[{product_guid,storey_guid}], spaces[], buildings[], sites[], projects[] */
  graphJson(): string;
  /** duplex.qto.json shape: {products, rows[{entity,count,storeys[],area_m2,volume_m3,triangles,
      products_with_mesh,products_without_mesh,source}]} — from the per-product mesh stats. */
  qtoJson(): string;
  /** types/manifest.json shape: {generated_with, source, types[{slug,type_name,entity,count,glb,bytes}]}.
      v1: glb = "" and bytes = 0 (no per-type mini-glbs in the browser); the instrument must degrade. */
  typesJson(): string;
  /** glTF binary, same writer as m.to_gltf(): KHR_mesh_quantization, EXT_mesh_gpu_instancing,
      node.extras.guid, GUID-named materials (GH #146). cut_openings is NOT available in v1 (manifold-csg is C++). */
  toGlb(perProductMaterials?: boolean, instancing?: boolean): Uint8Array;
  /** {tag: count} — GH #166. */
  bySourceJson(): string;
  /** Rust-side counters for the UI: products_seen/meshed/deferred, triangles, mesh_ms. */
  statsJson(): string;
  free(): void;
}
```

Parity gate: on `.local-samples/Duplex_A_20110907.ifc`, `summaryJson` /
`graphJson` / `qtoJson` / `typesJson` must equal
`ifcfast-site/public/sample/duplex.*.json` / `types/manifest.json`
modulo `path`, `parse_seconds`, `generated_with`, `glb`/`bytes` in the
manifest; `toGlb()` must have 227 nodes and 426 GUID-named materials
like the shipped `duplex.glb`.

## Core portability (known blockers, all cheap)

- `std::time::Instant` panics on `wasm32-unknown-unknown` → a tiny
  `clock` shim (`web-time` crate on wasm, `std::time` elsewhere) used by
  `mesh/mod.rs` and the other timers.
- `memmap2` does not build on wasm → put the `Mmap` variant and
  `source::open(path)` behind a default-on `mmap` feature; the
  `Owned(Vec<u8>)` variant is what the browser uses.
- `rayon`: no threads on the web target. `mesh_ifc_streaming_framed`
  already has a T=1 serial path; on `target_arch = "wasm32"` take it
  without ever touching `rayon::current_num_threads()` (which would
  try to spawn), and run the 1a/1c phases with plain iterators.
  `dashmap` / `crossbeam-channel` compile fine.
- Feature set for the wasm crate: `ifcfast-core` with
  `default-features = false, features = ["mesh"]` — no pyo3, no
  arrow/parquet, no parry, no manifold. `prism-csg-fast` (pure Rust)
  can come later for cut openings.

## Site (ifcfast-site)

- Drop zone in chapter 06 (the instrument): "drop your IFC — it stays in
  this tab". File → Web Worker → `IfcModel.fromBytes` → the four JSONs
  + glb → the instrument's existing `summary/qto/graph/manifest`
  state and a blob URL for the viewport. Progress + `by_source` shown;
  files > ~300 MB refused with a clear message (browser memory).
- The film (chapters 01–05) keeps the Duplex; only the instrument
  swaps to the dropped model.

## Toolchain on Omarchy

System Rust (Arch) has no wasm target; rustup is installed user-locally
(`~/.cargo/bin`, `--no-modify-path`). Build with
`PATH=$HOME/.cargo/bin:$PATH cargo build -p ifcfast-wasm --target wasm32-unknown-unknown --release`
then `wasm-bindgen --target web`. Builds stay serialized on this box.

## v2 — streaming geometry (Ed, 2026-09-07 evening)

Observed on the live site: a 210 MB RIV took 2 minutes and the viewport
was choppy. Cause: the worker meshes everything, writes ONE baked GLB,
the main thread re-parses it in model-viewer, and every product is a
separate draw call. Decision: **stream the geometry** — the instrument
plays the golden-section interlude until the first batch lands, then the
model builds up in front of the user, in a three.js viewer we own
(merged batch geometry, ~200 products per draw call, raycast picking).

### wasm API additions (keep the v1 methods working)

```ts
class IfcModel {
  /** v2: parse + index + extractors ONLY (fast). Mesh runs on demand. */
  static fromBytes(bytes: Uint8Array, name: string): IfcModel;
  /** Run the mesh pass once, streaming batches through `cb` as they are
      tessellated (synchronous callback from inside the pass — the worker
      postMessages from it, which reaches the main thread immediately).
      World frame, metres, minus the model's global shift (the same shift
      `m.meshes().global_shift` reports; get it from streamShiftJson()). */
  streamMeshes(productsPerBatch: number, cb: (metaJson: string, positions: Float32Array, indices: Uint32Array, progressJson: string) => void): void;
  /** `[sx, sy, sz]` in metres — set once streamMeshes has started. */
  streamShiftJson(): string;
  // after streamMeshes has run, graphJson()/qtoJson() carry the mesh stats
  // (m3/m2/triangles) exactly as v1 computed them.
}
```

`metaJson` per batch: `[{guid, entity, storey_guid, type_name, m3, m2, tri, v0, vn, i0, in, rgba:[r,g,b,a]}]`
where `v0`/`vn` are the product's vertex offset/count inside `positions`
(xyz triples), `i0`/`in` its index offset/count inside `indices`, and the
indices are batch-local (already offset by `v0`). `rgba` is the resolved
surface colour (same cascade as the glTF writer). `progressJson`:
`{seen, meshed, total}` where `total` is the product count from the index.

Contract on the numbers: after the stream, `qtoJson()` must equal v1's
`qtoJson()` byte-for-byte on the Duplex sample; the concatenation of all
batches' triangle counts must equal `statsJson().triangles`.
