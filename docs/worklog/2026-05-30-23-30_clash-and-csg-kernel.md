## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `e2e09e7` → `2420423` (four commits this session)
- **Session scope**: Land v0.4.19 substrate fingerprint columns + geom kernel foundation; ship `ifcfast.clash()` end-to-end on the substrate; lay the CSG kernel via `manifold3d` for the cut-openings work the viewer integrator asked for in GH #20.
- **Touched paths**: `crates/core/Cargo.toml`, `crates/core/src/bundle/parquet_sink.rs`, `crates/core/src/bundle/record.rs`, `crates/core/src/lib.rs`, `crates/core/src/geom/mod.rs`, `crates/core/src/geom/csg.rs` (new), `crates/core/src/clash/` (new — mod/source/engine/sink), `crates/core/src/bin/clash.rs` (new), `crates/core/tests/clash_integration.rs` (new), `python/ifcfast/__init__.py`, `python/ifcfast/clash.py` (new), `python/ifcfast/header.py`, `AGENTS.md`, `CHANGELOG.md`, `CLAUDE.md` (new), `Cargo.toml`, `pyproject.toml`, and the four prior-session worklogs
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: continues `2026-05-30-22-36_geometry-kernel-foundation.md` — that session left the geom kernel unstaged together with the fingerprint-column work; this session committed both, then built `ifcfast.clash()` on top of the kernel, then added the CSG primitive

## Summary

The previous session left a big unstaged batch (substrate fingerprint
columns, geom kernel scaffolding, AGENTS.md update, four worklog
files). This session committed all of it, bumped the workspace from
0.4.18 to 0.4.19, and then plowed forward into two more kernel-
adjacent features:

- **`ifcfast.clash()`** — substrate-aware narrow-phase clash engine.
  Reads `instances.parquet` + `representations.parquet`, broad-phases
  via the new `geom` kernel, narrow-phases candidate pairs as
  parry3d mesh-mesh intersection / distance, writes
  `clashes.parquet`. End-to-end: substrate reader → engine → parquet
  sink → PyO3 binding → Python wrapper → CLI bin → 4 integration
  tests + 7 engine unit tests. Includes a substrate-level fix:
  parquet schema metadata now carries `ifcfast.unit_scale` so the
  clash engine can convert source-unit vertex / bbox values to
  metres at load time. Tolerance and output are always in metres
  regardless of the source IFC's linear unit.

- **CSG kernel via `manifold3d`** — `geom::csg` module providing
  `subtract` / `subtract_many` / `build_manifold` / `manifold_to_buffers`
  for net booleans. Lays the foundation for cutting `IfcOpeningElement`
  geometry out of host walls / slabs (the viewer integrator's P0 #1
  ask in GH #20). 8 unit tests covering contained openings,
  boundary-straddling openings, multi-opening batches, and the
  no-cutter round-trip. Behind a `csg` Cargo feature, off by
  default in the wheel until the C++ Manifold build dep is
  exercised across all wheel platforms.

Mid-session, the user dropped feedback from a viewer integrator
(GH #20) and a forward-looking ask about geometry hot-swap (profile
recognition + centerline extraction + parametric replacement). Both
captured in memory; the cut-openings direction was chosen as the
natural next-up after clash, and the hot-swap ask was filed as a
separate roadmap arc (`[[roadmap-geometry-hotswap]]`) since the
algorithms don't overlap.

## Changes

**Commit `26f967b` — release: v0.4.19 — fingerprint columns + geom kernel foundation (15 files, +890)**

Bundled into one atomic commit because the new `geom` Cargo feature
must ship alongside its module (splitting would leave a broken
intermediate `cargo build --features geom`).

- `instances.parquet` gains `centroid_xyz` + `vertex_count` +
  `triangle_count`. Cache schema bumped 4→5.
- New `geom` feature, off-by-default — `parry3d 0.17` + `nalgebra
  0.33`. Module exposes `build_trimesh`, `pairs_overlapping`,
  `intersects`, `min_distance`.
- AGENTS.md substrate section + CLI quick reference; repo CLAUDE.md
  mirrors the AGENTS.md upkeep rule from auto-memory.
- Workspace + crate + pyproject all bumped 0.4.18 → 0.4.19.

**Commit `0976bd3` — docs(worklog): session entries (4 files, +1340)**

Four prior-session worklogs that were untracked.

**Commit `48e83a8` — feat(clash): ifcfast.clash() end-to-end (13 files, +1770)**

- `crates/core/src/clash/source.rs` — substrate reader; decodes
  binary `vertices_le` / `indices_le` blobs; converts source units
  to metres using the new `ifcfast.unit_scale` parquet schema
  metadata so the rest of the engine is metre-native.
- `crates/core/src/clash/engine.rs` — orchestrates broad → narrow.
  Bakes world-coord TriMeshes per instance (apply 4x4 transform for
  `shared_or_direct` reps; passthrough for `composite` reps where
  vertices are already world-baked). Arc-caches built meshes so
  repeated lookups in the candidate pair loop are O(1).
  Class-filter knobs: `include_classes` (at least one side must
  match) + `exclude_self_class` (suppress wall-vs-wall noise).
- `crates/core/src/clash/sink.rs` — writes `clashes.parquet` with
  zstd via arrow/parquet, mirroring the bundle sink conventions.
- `crates/core/src/bin/clash.rs` — `ifcfast-clash BUNDLE_DIR
  [--tolerance N] [--out file.parquet]` CLI mirror.
- `crates/core/src/lib.rs` — PyO3 `_core.clash(bundle_dir,
  tolerance_m, write_parquet, include_classes, exclude_self_class)`
  binding behind the `clash` feature.
- `python/ifcfast/clash.py` — top-level `ifcfast.clash(bundle_dir,
  tolerance_m=0.0, ...)` returning a `pandas.DataFrame` with the
  same column shape as `clashes.parquet`.
- `crates/core/src/bundle/parquet_sink.rs` — schemas now carry
  `ifcfast.unit_scale` + `ifcfast.version` parquet schema metadata.
  Backwards-compatible.
- `crates/core/Cargo.toml` — `clash` Cargo feature stacking
  `bundle` + `geom`. **Default features bumped from `["python"]` to
  `["python", "bundle", "geom", "clash"]`** so the published Python
  wheel ships the full substrate + clash flow out of the box. Adds
  arrow + parquet + parry3d + nalgebra (~+5 MB) to the wheel.
- AGENTS.md — new "Narrow-phase clash (`ifcfast.clash()`)" section
  with the worked DuckDB join example. CLI quick reference updated.
- CHANGELOG — clash + unit_scale metadata + feature defaults change.

**Commit `2420423` — feat(geom): CSG kernel via manifold3d (3 files, +327)**

- `crates/core/src/geom/csg.rs` — `build_manifold` / `subtract` /
  `subtract_many` / `manifold_to_buffers`. `CsgKernelError` enum
  with shape-validation variants up front + a `ManifoldRejected`
  passthrough for the kernel's own validity check.
- `crates/core/Cargo.toml` — `csg` feature pulling in
  `manifold-csg 0.2` with the `parallel` C++ backend. Off by
  default in the wheel pending wheel-build verification across
  platforms.
- `crates/core/src/geom/mod.rs` — module + public re-exports gated
  behind `csg`.

## Technical Details

**Unit-scale inconsistency in the substrate, and the fix.** When
the clash engine first ran against the mm-authored test fixture,
the near-miss test failed because the bbox columns turned out to be
in source units (mm) while `tolerance_m` was being interpreted as
metres. Confirmed via inspection: vertex / bbox columns are stored
in whatever the file's linear unit is, only the explicitly
`*_m2` / `*_m3` QTO columns get the `unit_scale` factor applied at
write time. Fix: record `ifcfast.unit_scale` as parquet schema
metadata on both substrate files; clash engine reads it and scales
bbox / vertex values to metres at load time. The fix is localized
in `clash/source.rs` — the engine sees metre-native inputs
throughout. Documented in [[substrate-unit-scale]] for future
substrate consumers.

**Broad-phase candidate id stability.** The broad phase
(`geom::pairs_overlapping`) emits pairs as `(id_a, id_b)` where
each id is the caller's choice — we feed the index into the
`instances` slice. The narrow-phase loop then re-looks-up the
`InstanceRow` by that index, so the order of `instances` matters.
Document that on the source-reader entry — it stays stable across
the broad/narrow boundary by design.

**Arc<TriMesh> in the mesh cache, not bare `Option<TriMesh>`.**
First pass used `HashMap<u32, Option<TriMesh>>` and hit a borrow-
checker block: needed two mutable borrows of the cache (`ensure_mesh`
for a, then for b) before either returned reference was used.
Switched to `Arc<TriMesh>` so `ensure_mesh` returns an owned `Arc`
clone (constant-time) instead of a borrow. Cleaner pattern; no
runtime cost beyond the Arc ref-count.

**manifold-csg 0.2 API.** `Manifold::from_mesh_f32(vert_props,
n_props=3, tri_indices)` accepts our flat-buffer form directly
when `n_props=3` (XYZ only, no per-vertex attributes). `Manifold::
batch_difference(&[host, c0, c1, …])` is the right primitive for
"host minus N cutters" rather than N sequential `difference` calls
— manifold folds them into one BVH walk and produces a cleaner
output topology. `Manifold::to_mesh_f32() -> (Vec<f32>, n_props,
Vec<u32>)` round-trips back; we always get `n_props=3` since we
never request normals.

**Why the wheel-default change.** Previously `default = ["python"]`
meant `cargo install` or a `pip install ifcfast` user got only the
PyO3 bindings — the substrate writer (`bundle`) and geom kernel
(`geom`) were `cargo build --features bundle,geom` opt-ins. That's
fine for power users but breaks the "agents shouldn't have to know
about extras for first-class promises" stance (the
`[[feedback-highest-value-route]]` rule applied). Bumped default to
`["python", "bundle", "geom", "clash"]`. CSG stays off-default
because the Manifold C++ build adds wheel-platform risk that
hasn't been exercised yet.

**Test inventory:** 92 lib + 4 bundle integration + 4 clash
integration + 13 mesh_reveal = **113 tests green**. Adds 7 clash
engine unit tests (in `engine.rs`), 4 clash integration tests
(synthetic two-wall IFC fixtures), and 8 CSG tests.

## Next

The viewer-feedstock arc (per `[[viewer-feedback-2026-05-30]]`) is
the natural continuation. With clash + CSG kernel done, sequenced
remaining work:

1. **Cut-openings mesh-extraction integration.** Wire `geom::csg::
   subtract_many` into the mesh extractor. Add `cut_openings: bool`
   opt-in to `m.meshes()` / `m.to_gltf()`. Build a host→cutters map
   from the indexer's `voids_opening` / `voids_host` parallel
   arrays. Suppress the opening's own mesh from the output when
   cut. Reveal-all stays default. Touches the hot mesh-extraction
   path — deserves a fresh session and its own commit.
2. **Rayon parallelization** of `mesh_ifc_streaming`. No `rayon`
   anywhere in `crates/` today; per-product tessellation is
   embarrassingly parallel. 4-16× on multicore. Independent of (1).
3. **`EXT_mesh_gpu_instancing` in `mesh/gltf.rs`** — reuses the
   `bundle.rs` rep/instance dedup that's already done. 10-50× fewer
   bytes + draw-call setup on repetitive models. Biggest viewer
   win after cut-openings.
4. **`KHR_mesh_quantization`** (16-bit positions relative to local
   origin + octahedral normals) — composes with the existing f64
   rebase + global shift.
5. **`meshopt` + zstd** on geometry buffers.
6. **`m.to_gltf(path, *, cut_openings=True, instancing=True)`** —
   one-call viewer export with optimal defaults (closes the loop
   on (1)-(5)).
7. **Geometry benchmark + bigger fixtures** + glTF/substrate
   output contract doc.

Parallel / unrelated to viewer arc:

- **Geometry hot-swap roadmap** (`[[roadmap-geometry-hotswap]]`) —
  profile recognition + centerline extraction + system-level
  collapse + parametric mesh swap for ducts/pipes. The user
  surfaced this mid-session as a future need. NOT folded into
  cut-openings (different algorithms). Sequenced after the viewer
  arc unless reprioritised.

Substrate-level follow-on (was queued before this session, still
open):

- **OOBB + principal-axis fingerprint columns** via `nalgebra`
  PCA. Bumps cache schema v5 → v6. Lets agents match rotation-
  variant duplicates that AABB-overlap misses. Also pre-requisite
  for the hot-swap profile-recognition step.

## Notes

- The `dead_code` warning on `indexer::extract_unit_scale` is
  preexisting and CI doesn't enforce `-D warnings` (only the
  release.yml does for the wheel build, and that's `--release`
  with default features which don't trip this path). Not blocking;
  fix when convenient.
- v0.4.19 not yet tagged. The release flow rule
  (`[[release-flow]]`) requires a `git push origin v0.4.19` tag to
  trigger the maturin CI publish. **Recommend tagging this batch
  before the next feature session so users can `pip install
  ifcfast==0.4.19` and use `ifcfast.clash()` on a real model.**
  The CSG kernel doesn't need its own release tag — it's off by
  default in the wheel.
- Memory updates this session: new `[[clash-engine]]`,
  `[[substrate-unit-scale]]`, `[[viewer-feedback-2026-05-30]]`,
  `[[roadmap-geometry-hotswap]]`; MEMORY.md index extended.
