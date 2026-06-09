## Agent signature
- **Agent**: `claude-opus-4-8`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `ee840db` → `16609af` (2 commits: `76a4d37` fix, `16609af` release v0.4.36); then `feat/reroute-primitives` @ `a3de181` (branched off `16609af`, 2 commits: `04f5f80`, `a3de181`)
- **Session scope**: Fix + ship the v0.4.35 half-space mm-unit over-report regression (GH #65); then prototype the GH #63 reroute MVP (voxel occupancy + MEP-aware A* router).
- **Touched paths**: `crates/core/src/mesh/cut_validate.rs`, `crates/core/tests/cut_openings_integration.rs`, `AGENTS.md`, `CHANGELOG.md`, `Cargo.toml`, `pyproject.toml`, `Cargo.lock`, `crates/core/src/mesh/voxel.rs`, `crates/core/src/mesh/axis.rs`, `crates/core/src/occupancy/mod.rs`, `crates/core/src/routing/mod.rs`, `crates/core/src/mesh/mod.rs`, `crates/core/src/lib.rs`, project memory (`omarchy-oom-multiagent.md`, `MEMORY.md`, `next-steps.md`)
- **Parallel sessions observed**: none (only my own commits landed on origin/main during the window)
- **Supersedes / superseded by**: none. Follows `docs/worklog/2026-06-07-15-34_w6-hardening-gh64-correctness-robustness.md`.

# Session: GH #65 half-space regression fix + v0.4.36 release, then reroute MVP

## Summary
Two threads. **(1)** Fixed and shipped GH #65 — a high-severity regression in the *just-released* v0.4.35 where `IfcHalfSpaceSolid` cuts over-reported volume on millimetre-unit models (re-opening the #39 failure class). Root cause was W3's `on_plane_eps` reframing the clip's numerical round-off guard as a *physical 1 mm*, which is `1.0` source-units in an mm file — coarse enough to drop near-plane cut-cap faces and over-integrate. Fixed to a source-unit guard, validated on Sannergata (383/389 within ±1 %), bundled with the unreleased #64 work into **v0.4.36** (published to PyPI). **(2)** With #65 shipped, built the GH #63 constraint-aware MEP rerouting MVP end-to-end: the `voxelize → go/nogo → find-paths` pipeline the user wants, with their MEP routing rules (≤90° bends, prefer <45°, per-system Z discipline, stay-at-elevation default) baked into a classic A* router. Mid-session the box OOM-crashed on `maturin --release` builds — recovered cleanly, switched to a build-free root-causing method, and captured the lesson in memory.

## Changes
- **GH #65 fix (`76a4d37`)**: `cut_validate::on_plane_eps` now returns a numerical round-off guard of `1e-3` in **source units** (capped so km-scale files stay sub-mm), reverting W3's `BASE_M / unit_scale` physical-1 mm semantics. Metre/mm/foot all resolve to `1e-3` (byte-identical to the pre-W3 clip on real units). Rewrote the two `cut_validate` eps tests to the corrected semantics; fixed a *latent test bug* — `deep_bcr_with_three_halfspaces` ran the mm fixture at `unit_scale=1.0` (masking the mm path), now `0.001`. `AGENTS.md` tolerance section corrected.
- **v0.4.36 release (`16609af`)**: version bump (Cargo.toml/pyproject/Cargo.lock) + CHANGELOG entry covering #65 + the bundled #64 W6 hardening. Pushed `main`, tagged `v0.4.36` → CI built/published wheels (run 27164879084, green, 8m42s).
- **Reroute primitives (`04f5f80`)**: `mesh/voxel.rs` `rasterize_solid_3d` (solid voxelization, 3D generalization of `qto::footprint_xy_raw`, winding-parity fill into a dense `VoxelGrid`); `mesh/axis.rs` `axis_from_extrusion` (Path-A parametric centerline + profile in world metres).
- **Reroute occupancy + router (`a3de181`)**: `occupancy/mod.rs` `build()` (obstacles + per-space keep-out prisms → go/nogo grid, plenum free by construction); `routing/mod.rs` classic A* with MEP constraints (26-conn 45° bends, angle-scaled turn cost ≤90°, per-`SystemKind` Z discipline, 3D-octile heuristic). Added `VoxelGrid::{voxel_center, world_to_voxel, in_bounds}`. Modules gated behind `mesh`, registered in `lib.rs`.

## Technical Details
- **#65 root-cause by elimination, not bisect.** Cross-tag rebuilds were OOM'ing the 16 GB box, so instead of `git bisect` I diffed `v0.4.34..main` over the default cut path and proved every function (`apply`, `partition_segments`, `is_cutter`, `derive_plane_from_slab`, `halfspace_solid`, `clip_by_plane` cap-build) byte-identical *except* the `on_plane_eps` value. That made W3 the sole suspect, confirmed by a single incremental build: the fix flipped all 6 reported walls back to ifcopenshell AND flipped `mesh_quality` `open_shell → closed` (the mechanism — suppressed cut-caps — made visible). Both the issue's diagnosis (#52 normal commit) and my own first two hypotheses (GH #60 prism fallback; W3-under-report) were wrong; the prism fallback never even fired (`volume_method="mesh"` on every wall).
- **OOM recovery.** `maturin develop --release` cold-builds the whole dep graph (manifold-csg-sys C++, arrow, parquet, parry3d, i_overlay) at `lto=thin/codegen-units=1` with 8 rustc → >15 GB peak → crash. Recovery: `CARGO_BUILD_JOBS=4` cap + prefer **incremental** rebuilds (one crate + relink, deps cached) + diff-based root-causing over rebuilds. The crash-restart also wiped untracked scratch files (recreated from context).
- **Reroute voxelization gotcha.** Even-odd Z-parity fails when a column-centre ray grazes a shared triangle edge (box diagonal `px==py`): the inclusive point-in-triangle test double-counts it and even-odd cancels it to empty. Switched to **winding-number** parity (same-facing crossings sum, don't cancel) — robust for consistently-wound IFC solids; `columns_unbalanced` flags open/inconsistent columns.
- **Router model.** 26-connectivity gives 45° bends; turn cost scales with bend *angle* (straight free, 45° ≈ half a 90°, >90° forbidden) so shallow bends are preferred. Per-system via `SystemConstraints::for_kind`: Pressure→Planar, Drain→MonotoneDown, Duct→penalised-Free, Unknown→Free+high-Z-penalty (stay level). Admissible 3D-octile heuristic keeps A* optimal under the penalties.

## Next
- **Reroute (all sub-items tracked in GH #63, not separate issues):** fall-grade (target gradient on top of MonotoneDown); C-space inflation (dilate obstacles by profile½+clearance+insulation, route centreline as a point — consumes `axis.rs`); path simplification (collapse A* voxel staircase, line-of-sight smoothing); property-based `SystemKind` inference; wire occupancy/axes through the substrate (public `reroute()` + `axes.parquet`, cache 16→17→18).
- **`feat/reroute-primitives` branch** pushed — decide on a PR vs. continue on-branch.
- **#65 follow-up (minor):** coordinate-magnitude-relative clip eps is the principled long-term form (noted in `on_plane_eps` doc); only matters for exotic-unit files, low priority.

## Notes
- Push to main + `v*` tag-push are autonomous for this repo; v0.4.36 shipped autonomously after confirming the outward-facing publish with the user. `feat/reroute-primitives` pushed at session end on user request.
- The GH #63 design doc `docs/plans/2026-06-07_reroute-design.md` referenced in memory was never committed (lost untracked file); the GH #63 design-synthesis comment is the surviving record. Offered to reconstruct it; not done yet.
- Reroute modules are leaf prototypes (behind `mesh`, no public API / substrate / cache change). 131 lib tests green, clippy clean throughout.

## Addendum — continued session (post-worklog): reroute demo + G55 real-data extraction

After the worklog the session continued on the reroute thread.

**Runnable demo (`a3ab4dd`, pushed).** `crates/core/examples/reroute_demo.rs` —
`cargo run -p ifcfast-core --no-default-features --features mesh --example reroute_demo`
prints two ASCII scenes proving the pipeline: a `Duct` routing around a wall+column
(plan view, 45°/90° bends) and a `GravityDrain` descending under a beam (side view,
`MonotoneDown` → 0 uphill steps).

**G55 obstacle extraction (exploration — not committed; scratch scripts reference
private client paths).** Goal: feed the reroute occupancy from the real G55 models.
- **G55_ARK** (156 MB, IFC2x3, mm): **0 IfcSpace** (so spaces can't drive keep-out —
  pivoted to ceilings/slabs/walls per the user). 342 `IfcCovering` all `.CEILING.`
  (suspended ceilings), 152 slabs, 4173 walls. Floor-banded **by geometry (placement
  Z), not relationships** → ~9 storeys at ~3.6 m (slab/ceiling levels 2.30 / 5.90 /
  9.45 / 13.05 / 16.68 / 20.27 / 23.84 / 27.50 m). `scripts/g55_floor_bands.py`.
- **Memory method that worked:** `ifcopenshell.open()` WITHOUT the geometry kernel +
  `util.placement` → placement Z only, peak **1.9 GB / 7 s** on the 156 MB ARK. This
  sidesteps the eager-mesher trap (see Notes); per-floor geometry would use the
  ifcopenshell `iterator` with `include=<one band's elements>` (memory-bounded).
- **RIB (beams) pulled from ACC.** Local RIB IFCs were 0-byte failed-sync stubs; the
  real ones live on ACC (Grønland 55 → `…/1_IFC/`). Downloaded the **Revit** one to
  `scratch/g55/G55_RIB.ifc` (2.8 MB, Revit 25.4, **72 beams + 54 columns + 19 slabs**);
  the 27.6 MB `G55_RIBprefab.ifc` is the **Tekla** prefab — ignored per instruction.
  Beams band onto the same ARK floor levels; columns span floors (placement Z=0, need
  geometry extent to band).

**The gate for an actual G55 route:** `occupancy`/`routing` are Rust-only — no Python
entry point — so extracted meshes can't reach them. Next concrete build is a **PyO3
binding** (meshes → `occupancy::build` → `find_path`), then a one-floor proof.

### Gotcha for the next session
`Model.meshes()` / `iter_meshes()` run the **Rust mesher EAGERLY (whole model, one
batch)** — the docstring says so explicitly ("not lower peak RAM"). Do NOT point them
at the 156 MB ARK on the 16 GB box. For per-floor work use ifcopenshell placement
(no-geom) for banding, and the ifcopenshell `iterator` with `include=` for per-band
tessellation. (A true streaming/Z-banded extractor in ifcfast core is the real fix —
candidate follow-up.)
