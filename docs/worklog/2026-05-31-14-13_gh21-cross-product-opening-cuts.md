## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `26c79ec` → `26c79ec` (no commits made this session)
- **Session scope**: GH #21 — cross-product `IfcRelVoidsElement` cuts wired into `m.meshes(cut_openings=True)`
- **Touched paths**: `crates/core/src/mesh/cut_openings.rs`, `crates/core/src/lib.rs`, `crates/core/tests/cut_openings_integration.rs`, `AGENTS.md`, `python/ifcfast/model.py`
- **Parallel sessions observed**: none (only commit on origin/main during this session window was the pre-session `26c79ec` from edkjo)
- **Supersedes / superseded by**: none

## Summary
Closed the second half of GH #20 P0 #1: `m.meshes(cut_openings=True)` now folds **cross-product** openings (separate `IfcOpeningElement` linked to a solid host via `IfcRelVoidsElement`) in addition to the in-representation `IfcBooleanClippingResult` case that shipped in `046e196`. Both authoring patterns now produce a single net-solid `cut_openings` segment per host; in reveal-all (default), both the host and opening still emit verbatim. Closes GH #21.

## Changes
- **`crates/core/src/mesh/cut_openings.rs`** — added `CrossProductCut` struct + `Routed` enum. `from_indexer(voids_opening, voids_host)` builds the host/opening membership index; `route(mesh)` classifies an incoming `ProductMesh` as `Suppressed` (opening — held as cutter), `Held` (host — buffered), or `PassThrough` (forward to in-rep `apply` as before). `flush()` runs in-rep `apply` on each buffered host first (so a host with BOTH an `IfcBooleanClippingResult` AND cross-product openings folds cleanly), then translates each opening's vertices by `(opening.mesh_anchor - host.mesh_anchor)` into the host's local frame and calls `geom::csg::subtract_many`. Combined outcome: `Cut` if any subtraction succeeded, `Fallback` if cutters existed but all failed, `Passthrough` if no openings ever arrived (host kept verbatim).
- **`crates/core/src/lib.rs`** — `MeshSink` in `extract_meshes` gained an optional `cross: Option<CrossProductCut>` field (cfg-gated on `csg`). Constructed from `idx.voids_opening` / `idx.voids_host` when `cut_openings=true` and the file has at least one void relation. Hot path stays identical when the buffer is empty. After `mesh_ifc_streaming_framed` returns, `sink.cross.take()` drives a flush loop that runs each folded host through the existing encode path. The byte-encoding logic was extracted into `MeshSink::encode(mesh)` so streaming and flush share one code path. Stats accumulate per host via a new `bump_outcome` helper.
- **`crates/core/tests/cut_openings_integration.rs`** — 4 new tests: `cross_product_voids_indexer_captures_relation` (host=arg4=#50, opening=arg5=#51), `cross_product_voids_reveal_all_emits_both_products` (reveal-all unchanged), `cross_product_voids_fold_subtracts_opening_volume` (full end-to-end fold via `mesh_ifc_streaming` + `CrossProductCut::route/flush` + manifold volume check ≈ 0.4 m³), and `cross_product_buffer_is_empty_when_no_voids_in_file` (short-circuit guard). All 7 tests in the file pass.
- **`AGENTS.md`** — dropped the "Cross-product openings... are NOT cut by this path yet" disclaimer; replaced with a paragraph documenting both patterns as covered, including the suppression semantics for openings in cut mode.
- **`python/ifcfast/model.py`** — `cut_openings` docstring on `meshes()` rewritten to describe both patterns; removed the "NOT cut by this path yet" note.

## Technical Details
**Frame alignment was the only delicate piece.** `extract_meshes` uses `BakeFrame::Local`: vertices live near origin and the true world position is carried on `ProductMesh.mesh_anchor` as f64. To CSG an opening into its host, both meshes need to be in the same frame. The fold computes `off = (opening.mesh_anchor - host.mesh_anchor) as f32` (typically <10 m between a wall and its door, so f32-safe), translates the opening's vertex buffer by that offset, then calls `subtract_many`. The result lives in the host's local frame and inherits the host's `mesh_anchor` — drops straight into the existing `encode()` shift logic with no special-casing.

**Stats semantics for combined in-rep + cross-product hosts** are simplified to a single per-host outcome: `Cut` if any subtraction succeeded, `Fallback` only when cutters existed but every attempt failed, `Passthrough` only when no cutters of any kind. Hosts get held BEFORE the in-rep `apply()` runs (apply runs at flush time on the buffered mesh), so the stats counters never get double-counted on the same product.

**Why the buffer wraps `MeshSink` rather than living inside `mesh_ifc_streaming_framed`**: the streaming function is the generic facility used by every consumer (substrate, OBJ, glTF, drift). Threading cut policy through it would pollute every caller. The wrapper-side approach lets `extract_meshes` carry the cut buffer while every other consumer keeps reveal-all reveal-all unchanged. Reveal-all stance preserved: substrate writer never touches the buffer, so `instances.parquet` / `representations.parquet` still carry operand-by-operand fidelity.

**Edge cases handled**: openings that have no body items never reach the buffer (their host falls back to `Passthrough` cleanly via the in-rep `apply` call at flush); opening products whose hosts are never meshed (host outside the meshable product set) are silently dropped — they're cutters, not visible products. Self-voids (`opening == host`) are skipped at indexer-read time.

## Next
1. **Work GH #22** — `--features csg` smoke test job in `release.yml` across all 5 wheel platforms; once green, flip default features to include `csg` and ship v0.4.20 with both opening-cut paths active.
2. **GH #20 P0 #2 — rayon** parallelization of `mesh_ifc_streaming` (no `rayon` in `crates/` today). Independent of (1).
3. After v0.4.20 ships, consider whether GH #20 should be closed (cut-openings P0 fully done) or split — the remaining glTF instancing / quantization / `m.to_gltf()` items deserve their own issues.

## Notes
- The buffer holds full `ProductMesh` buffers for every void-host until end-of-stream. For a building with hundreds of walls-with-doors that's tens of MB. Fine for `extract_meshes` (which accumulates everything anyway), would NOT be fine for a bounded-RAM streaming substrate path — but the substrate writer never enables `cut_openings`, so this is by design.
- The in-rep `apply()` is now called from two sites: the wrapper's `on_product` for non-host products, and `CrossProductCut::flush` for buffered hosts. Both paths share the same `Outcome` accounting via `MeshSink::bump_outcome`.
- AGENTS.md upkeep stayed clean — single paragraph edit, no link drift in README.
