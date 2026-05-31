# Session: f32 precision Stages 2-3 + ifczip transparent open & caching

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `951dfbe` → `e2e09e7` (5 commits)
- **Session scope**: ship the remaining f32-precision fixes for far-from-origin geometry (placement chain + faceset baked coords) and make ifczip files distributed under a plain `.ifc` extension open transparently and cheaply
- **Touched paths**: `crates/core/src/lib.rs`, `crates/core/src/mesh/{mod.rs,placement.rs,faceset.rs,extrusion.rs,qto.rs}`, `crates/core/src/indexer.rs`, `crates/core/tests/mesh_reveal.rs`, `python/ifcfast/{__init__.py,header.py,model.py,cache.py}`, `tests/test_smoke.py`, `Cargo.toml`, `crates/core/Cargo.toml`, `pyproject.toml`
- **Parallel sessions observed**: none on `origin/main` during the session window
- **Supersedes / superseded by**: none

## Summary
Closed the f32 large-coordinate precision gap in two stages and made ifczip handling
genuinely transparent. Shipped four PyPI releases (v0.4.15 → v0.4.18) covering: (a) f64
placement chain + CloudCompare-style global shift, (b) faceset f64 rebase by bbox-min
through a per-fragment f64 anchor, (c) Python header parser now decompresses ZIP via
stdlib `zipfile` so `.ifc`-labelled archives no longer error as "Not an ISO-10303-21 STEP
file", and (d) a process-scoped tempfile cache so multi-call pipelines on an ifczip don't
re-inflate the archive per `_core.*` call.

## Changes

**v0.4.15 — `feat(geometry): precise far-from-origin point_cloud/meshes via f64 placement + global shift`** (`f0e5445`)
- `PlacementResolver` now resolves the `IfcLocalPlacement` chain in **f64** (`DMat4`); a parallel f64 axis-placement builder (`axis_placement_3d_f64`) parses location in f64. The existing f32 `axis_placement_3d_from_id` is left alone (used by mapped / boolean / csg / revolved / extrusion for local geometry — small values, f32-safe).
- `ProductMesh.world_origin: [f64; 3]` carries the precise authoring placement.
- `BakeFrame::Local` (already in from Stage 1 for QTO) now drives `point_cloud()` and `meshes()`. Per-product positioning uses an f64 **global shift** (CloudCompare contract). Threshold-gated at 10 km in metres — near-origin models stay byte-identical (`shift = [0, 0, 0]`).
- Exposed as `df.attrs["global_shift"]` (point_cloud) and `MeshList.global_shift` (meshes); `MeshList` is a tiny `list` subclass so the iterator contract is preserved.
- Synthetic test: 0.1 m box at `(5e7, 5e7, 5e7) m` — World frame collapses (surface_count 0), Local + shift reconstructs 6 faces / 0.001 m³.
- Empirically validated against a synthesized G55_RIV-at-UTM-Oslo (5.97e8 / 6.643e9 mm): 0.4.14 vertex error up to **0.94 m** (median 280 mm, 98% > 50 mm); 0.4.15 precise.

**`fix(ci): compile clean without the mesh feature under -D warnings`** (`0c7f3b5`)
- Pre-existing CI red on `cargo test --release` (default features = no mesh, `-D warnings`). Four cascading errors:
  - `analyse_drift` was `#[pyfunction]` but missing `#[cfg(feature = "mesh")]` even though its registration was already gated.
  - `PyBytes` import only used inside mesh code.
  - `is_meshable_product` dead without mesh feature.
  - Unused `let v` in a `qto::edge_pairing` test.
- All fixed. Now 34 no-mesh + 57 + 13 with-mesh tests pass under `-D warnings`.

**v0.4.16 — `feat(geometry): f64 rebase in faceset kernel`** (`f033fb8`)
- `faceset.rs` parses `IfcCartesianPointList3D` in **f64** and rebases each face set by its bbox-min before downcasting to f32. Handles the case Stage 2 couldn't reach: when a transform bakes huge world coords directly **into** the representation (not the placement). Tester reported v0.4.15 on NTM-transformed G55_RIV cut empties 508 → 291; remaining ~234 were Layer-2 (baked-coord) collapses.
- `LocalMesh.rep_origin: [f64; 3]` carries the offset; default `[0, 0, 0]` for kernels that don't rebase (extrusion / profile / csg / revolved / brep — their inputs are dimensions or sweep params, not absolute world coords).
- Bake loop computes per-fragment `precise_anchor_f64 = effective_f64 * rep_origin_f64` and splits the matrix-multiply: rotation*v in f32 (small after rebase) + anchor in f64-downcast.
- `ProductMesh.mesh_anchor` (f64) pins to the first geometry fragment's precise anchor; Stage 2 sinks use it instead of `world_origin` for the global shift. For non-rebased kernels `mesh_anchor == world_origin`, so behaviour for normal models is identical.
- New mesh-reveal test (`far_faceset_with_baked_world_coords_meshes_intact`): 0.1 m `IfcPolygonalFaceSet` cube at 5e7 m. Local frame meshes intact. World-frame assertion deliberately dropped + documented: f32 absolute output is mathematically capped at ~16.7 M.

**v0.4.17 — `fix(open): transparent ifczip in Python header`** (`84ad474`)
- Rust `source::open` already handled ZIP via magic-byte dispatch — but `ifcfast.open()` runs Python's `header.py:header()` **first**, which read raw bytes and validated the STEP prefix before any Rust call. Result: every `.ifc`-labelled-but-secretly-zipped file (the ACC / Dalux / Trimble Connect convention) errored as "Not an ISO-10303-21 STEP file". Most tools mis-read those as corrupted STEP.
- `header.py:_read_step_prefix(p, n)` now peeks 4 bytes; if `PK\x03\x04`, uses stdlib `zipfile` to read the prefix from the **largest** `.ifc`/`.step`/`.stp` member (same dispatch rule as Rust so both paths agree on which member).
- Regression test (`test_zip_disguised_as_ifc`) wraps the minimal fixture in a ZIP under an `.ifc` filename.
- Validated end-to-end on two real Sannergata RIV files (Skiplum ACC tree + Dalux export, both ~128 MB ifczip-as-`.ifc`, IFC2X3): opens in ~4 s, parses 143k / 144k products, meshes 142,624 with 0 empty.

**v0.4.18 — `perf(ifczip): cache the decompressed tempfile across _core.* calls`** (`e2e09e7`)
- Before: every Python → Rust call (`_core.index_ifc`, `_core.mesh_qto`, ...) went through `source::open` and re-decompressed the entire archive. For Sannergata RIV (128 MB → ~500 MB inflated), that's ~1.4 s of inflate per call.
- `header.py:native_path_for(p)` decompresses an ifczip's largest STEP member **once** to a process-scoped tempfile keyed by `(canonical path, mtime_ns)`. Tempfiles cleaned on `atexit`. All six `_core.*` call sites (`index_ifc`, `mesh_qto`, `sample_point_cloud`, `extract_meshes`, `extract_all`, `analyse_drift`) route through it. Plain `.ifc` short-circuits on the magic check.
- Measured: `index_ifc` on Sannergata 2.35 s → 0.98 s per call (~2.4×). Typical 4-step pipeline saves ~4 s per ifczip.

## Technical Details

**Why threshold-gated shift (10 km in metres):** the global shift is genuinely useful only when f32 starts losing physical precision. f32 holds ~16.7 M exact integers; below ~10 km, an mm-authored model's quantum is sub-mm. Above, the shift is necessary. Gating means normal building models return absolute coords (byte-identical to pre-fix), and only georeferenced models get shifted. The shift is exposed in metres regardless of the file's authoring unit.

**Why the `mesh_anchor` introduction:** my first Stage 3 cut used `world_origin` (placement) as the Local-frame reference. For the rebased-faceset case the rep_origin contribution put `frag_off = precise_anchor - world_origin = ~5e7` — re-collapses the shape under f32. Pinning `mesh_anchor` to the first fragment's `precise_anchor` makes `frag_off = 0` for that fragment and small for siblings. Stage 2 sinks now use `mesh_anchor` instead of `world_origin`; for the no-rebase case the two are equal so no regression.

**World frame is inherently capped, not a bug.** f32 absolute coords at 5e7 m have a quantum of ~6 m. No bake-loop math can fix that — the only way to deliver precise far-origin geometry is to return shifted coords (Local frame). Documented in the mesh-reveal test and the project memory. Legacy World-frame consumers (OBJ / glTF / drift / substrate) accept the ceiling.

**ifczip dispatch is extension-agnostic on both sides.** The Rust `source::open` and Python `header._read_step_prefix` / `native_path_for` all peek the first 4 bytes (`PK\x03\x04`) and ignore the extension entirely. So `Sannergata_RIV.ifc` (secretly zipped) and `foo.ifczip` (might be plain text) both hit the right path. Both pick the **largest** member with a STEP extension, which is robust to archives carrying thumbnails / change-history XML / sidecar metadata.

**Why Python-side caching for the perf fix instead of Rust LRU:** Python-side `tempfile` + `atexit` is ~30 lines of stdlib code and decouples from the Rust ABI. Rust-side would have required `IfcSource::Owned(Arc<Vec<u8>>)` or similar, plus a `Mutex<HashMap>` static. The Python approach also makes the tempfile location debuggable (`/tmp/ifcfast_unzip_*`) and easy to override per-process if needed later.

## Next
- **Tester re-runs** on the NTM-transformed file: did v0.4.16 + v0.4.17 + v0.4.18 close the remaining 234 empties? Open empirical loop, no code-side action.
- **`IfcConversionBasedUnit`** parsing in the indexer — imperial composites (FOOT-INCH-1/64). Real coverage gap for US imperial Revit exports; not in current corpus.
- **`brep.rs` f64 rebase** — same `LocalMesh.rep_origin` pattern as faceset, applied to `IfcCartesianPoint` parsing in `polyloop_vertices`. Speculative until brep-baked-coord empties surface.
- **AREAUNIT / VOLUMEUNIT / PLANEANGLEUNIT** parsing — currently we derive area = length², volume = length³, angles in radians (IFC default). Only wrong on inconsistently-declared files.
- **Repo hygiene**: `node_modules/` (2561 files) and `.local-samples/out/*.json` were committed by the worklog-hook's auto-checkpoint (`951dfbe`) and are on origin. Want a `.gitignore` + `git rm --cached` pass.
- **MEMORY.md bump**: index pointer says v0.4.16 — update to v0.4.18.

## Notes
- The auto-checkpoint hook (`worklog-hooks/scripts/write-worklog.py`) silently committed in-flight work + a lot of unrelated tracked files (`node_modules`, `.local-samples`) in commit `951dfbe` before this session started. That's how `placement.rs` Stage 2 work ended up landing without my own commit. Worth knowing — it makes "what did this session do" harder to read from `git log` alone.
- The Rust CI workflow (`ci.yml`) runs `cargo test --release` with default features (no `mesh`) and `RUSTFLAGS: -D warnings`. The release workflow (`release.yml`) builds wheels with `python + mesh`. Tags on the version (`vX.Y.Z`) trigger releases; main pushes trigger CI. Confirmed working through this session.
- Tester is on Windows; their classifier project lives at `C:\Users\edkjo\.claude\projects\c--workspace-toolkit-pointcloud-classifier\` — they noted v0.4.15 progress in their own project memory.
- All `/tmp/G55_RIV_utm.ifc`, `/tmp/v0414` venv, `/tmp/d0414.{npz,pkl}` test artifacts can stay (`/tmp` is ephemeral).
- One untracked file remains in working tree: `docs/worklog/2026-05-29-13-28_Auto-Checkpoint.md` (from the hook). Decide commit-or-delete next session.
