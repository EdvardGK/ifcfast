# Session: Streaming GeoParquet substrate writer — vertical slice

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `9e74f0e` → `49ab208` (one commit this session)
- **Session scope**: Pivoted from the queued bit-identical-writer spike to a streaming GeoParquet substrate writer; built and validated the vertical slice end-to-end.
- **Touched paths**: `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `crates/core/src/mesh/mod.rs`, `crates/core/src/bundle/mod.rs` (new), `crates/core/src/bundle/record.rs` (new), `crates/core/src/bundle/parquet_sink.rs` (new), `crates/core/src/bin/bundle.rs` (new)
- **Parallel sessions observed**: none on origin/main during this window
- **Supersedes / superseded by**: none

## Summary
Session opened on the queued "writer spike — bit-identical no-op round-trip" plan. User redirected twice with high-leverage reframes: first that bit-identical was for the analysis pipeline, not editing (clarifying the OOM + semantic-loss problem in `ifcfast-mesh`); then that IFC5 abandons STEP entirely, so over-optimizing for the STEP physical format is the wrong bet. Both reframes killed the writer-spike thread and pivoted the session to a **streaming GeoParquet substrate writer** — input-format-agnostic, source-class-normalized, DuckDB-queryable. Vertical slice shipped, committed, and pushed (`49ab208`). All 7 CI jobs green.

## Changes

### New cargo feature: `bundle` (default off)
Pulls `arrow = "53"` + `parquet = "53"` (heavy crates intentionally gated off the python-extension build).

### `crates/core/src/mesh/mod.rs` — sink refactor
- New `ProductSink` trait (`fn on_product(&mut self, mesh: ProductMesh)`).
- New `VecSink` for in-memory accumulation (used by batch glTF/OBJ writers).
- New `mesh_ifc_streaming<S: ProductSink>(buf, &mut sink) -> MeshStats` entry.
- Legacy `mesh_ifc(buf) -> (Vec<ProductMesh>, MeshStats)` now wraps the streaming entry via `VecSink`. All 9 reveal tests still green — refactor is invisible to existing callers.

### `crates/core/src/bundle/` (new module)
- `mod.rs` — `Bundle::build(buf)` does the one-time pre-pass: EntityTable → IndexedFile (87K-product columns) → 4 extractors (psets/materials/quantities/classifications, GUID-keyed long-format) → regroup into per-product `HashMap<String, Vec<...>>` maps + storey/type/aggregate lookups. Exposes `semantics_for(guid: &str) -> ProductSemantics` — O(1) per product.
- `record.rs` — `ProductRecord::pair(mesh, semantics)` consumes a `ProductMesh` + its semantic snapshot, encodes vertices/indices to LE byte buffers in-place, computes AABB. Owns its data so the source mesh drops immediately.
- `parquet_sink.rs` — `ParquetSink<'a>: ProductSink` with row-group buffering (default 1024 products), zstd compression + dictionary encoding. Schema: 26 columns including 5 List-of-Struct nested columns (segments, materials, psets, quantities, classifications) + Binary blobs for vertex/index geometry + FixedSizeList[3] for placement/bbox.

### `crates/core/src/bin/bundle.rs` (new binary)
`ifcfast-bundle <file.ifc> [out.parquet]` — orchestrates Bundle::build → ParquetSink::create → mesh_ifc_streaming → sink.finish(). Reports per-phase timings and semantic-pre-pass stats.

### Class normalization
Source-format-agnostic schema: `class: "Wall"` (stripped `Ifc` prefix + `StandardCase`/`ElementedCase` suffix), `source_class: "IfcWallStandardCase"` kept for trace. Survives the IFC4→IFC5 transition the user flagged — substrate doesn't carry STEP-isms in its primary key.

## Technical Details

**The fork moment (user reframe #1).** Bit-identical writer thread was based on a misread of the user's "convert and pair geometry with rels/properties" — I read it as a writer-side question about non-destructive editing. The user clarified it was about the **analysis pipeline OOM + semantic loss**: `ifcfast-mesh` accumulates every `ProductMesh` in `Vec<ProductMesh>` until the whole walk returns (OOM at 1 GB IFC on 16 GB host), AND the OBJ output strips every IFC semantic (storey, pset, material, type) so any downstream analyser has to re-parse the IFC alongside the geometry.

**The fork moment (user reframe #2).** I proposed "COG for IFC" framing. User flipped it: "IFC5 abandons STEP entirely — don't over-optimize for it. We're dealing in geometry analysis + data + relational data. STEP/IFC is whatever." That reframed the project: ifcfast isn't an IFC-derivative format; it's the canonical streaming converter from any AEC dialect into a stable open substrate. Output is the moat; input parsers are commodity.

**Cross-industry pattern adopted: GeoParquet from GIS.** Column-major, predicate-pushdown, dictionary-encoded, spatial-index-in-footer, DuckDB-queryable. The "COG moment" for AEC analysis. Future 3D-lane companion: USD (Pixar's composition layers give us non-destructive editing for free) — out of scope for this session.

**Architecture in three layers:**
1. **Pre-pass** (bounded by entity count, not file size): EntityTable + indexer::index() + 4 extractors. Heaviest cost: pset map at 2.57M rows on ST28_RIV, cloned twice (extractor long-format + bundle GUID-keyed regrouping). This is the next optimization target.
2. **Streaming mesh pass** (bounded by row-group buffer, ≈1024 products): `mesh_ifc_streaming` walks products, builds `ProductMesh`, hands to `ParquetSink::on_product` which pairs via `Bundle::semantics_for(guid)`, encodes to `ProductRecord`, buffers, flushes row groups at 1024.
3. **Parquet output**: zstd + dict, ~38× compression on geometry strings, 180 MB out from ~490 MB raw vertex+index bytes on ST28.

**Arrow nullability gotcha (twice).** `FixedSizeListBuilder` and `ListBuilder` both materialize their inner `item` field as `nullable: true` regardless of the schema. RecordBatch validation rejects the batch with a nullability mismatch if the schema declares `nullable: false`. Fix: declare schema inner item fields as nullable. Caught at first write attempt on each, total ~5 min lost.

**FixedSizeList for 3-tuples.** Used `DataType::FixedSizeList(Float32, 3)` for `placement_xyz`/`bbox_min_xyz`/`bbox_max_xyz`. DuckDB exposes these as `FLOAT[]` — indexable as `placement_xyz[1]` (1-based in DuckDB). Spatial bbox filter `WHERE bbox_min_xyz[3] >= 0 AND bbox_max_xyz[3] <= 3000` works as a one-liner.

**Validation:**
- **Duplex** (2.4 MB, 286 products): 22ms pre-pass + 15ms streaming, 141 KB Parquet. UNNEST on nested `psets`/`materials` works. Bbox spatial filter works.
- **ST28_RIV** (834 MB, 87K products indexed, 86K meshed, 27M triangles): 12.2s pre-pass + 20s streaming, 180 MB Parquet. **Peak RSS 2.75 GB** — the prior batch pipeline would have OOM'd. DuckDB queries: class histogram 441ms, UNNEST(psets) + GROUP BY on 2.57M nested rows 234ms, classifications join 8.6ms (dict-encoded perfectly).

**Honest scope gap.** The pre-pass is *not* bounded — it still scales with entity count. On a true 1 GB+ Sannergata at ~144K products with proportionally larger pset count, the pre-pass alone could need 3-5 GB. The streaming refactor solved the dominant cost (mesh accumulator) but not the pre-pass O(entities) allocation.

**Validation-on-real-1GB blocker.** Both local `Sannergata_RIV.ifc` files (123 MB and 143 MB) are `.ifczip` — exactly the silent-drop bug from issue #19. They parse to zero products. The true 1 GB OOM victim isn't on this host. ST28_RIV (796 MB STEP) was the available proxy.

**Side observation: pre-existing units bug.** Material thickness column is named `layer_thickness_mm` but values come back in metres on Duplex (Plasterboard 0.016 = 16mm). Pre-existing in `extractors::materials::build` — not introduced by this session, not in scope. Worth filing.

## Next

1. **String interning + zero-clone regrouping** in `Bundle::build`. The 2.57M pset map cloning is the dominant pre-pass RSS cost. `Arc<str>` for repeating set names ("Pset_WallCommon", "MagiCAD Pset_Pipe") should compress hundreds of MB to single-digit MB. Consume extractor output rather than copy. Expected: halve peak RSS on big files.
2. **Close `.ifczip` silent-drop (#19)** so we can validate on the real 1 GB Sannergata. Without this, every Norwegian Revit/Dalux export is invisible to the converter.
3. **3D lane**: streaming glTF or USD spike. Current batch glTF writer accumulates JSON metadata + binary buffer in RAM before writing — same OOM-class issue as the old mesh path. Two-pass write (binary tmpfile, JSON header last) or pivot to USD (Pixar composition layers — non-destructive edit story).
4. **Material units bug** in `extractors::materials::build` — column named `layer_thickness_mm` but value is metres. Either rename the column or apply `unit_scale` conversion. File an issue.

## Notes

- **User reframes drove the architecture, not my initial plan.** Two redirects (mesh-pipeline OOM scope, then IFC5-abandons-STEP framing) shifted the session from a writer-side spike to a streaming-converter substrate. Worth remembering: when the user pushes hard on framing, take it seriously — the resulting work was substantially better than the original plan.
- **The "be proud" prompt prevented an NDJSON-blob default.** First answer to "streaming format?" was NDJSON + binary sidecar — defensible but uninspiring. User's "only awesome solutions we can be proud of, cross-learn from other industries" forced the GeoParquet / USD framing, which is structurally the right call.
- **arrow + parquet are heavy (~3 min cold compile).** Feature-gated correctly. Don't pull into the default python build.
- **CI matrix still hits Windows + macOS even though `bundle` feature is Linux-developed.** All 7 jobs (cargo-test + pytest × 3 OS × 2 Python) passed on the commit.
- **`Cargo.lock` is gitignored.** Library-crate convention; downstream workspaces resolve their own.
