## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `feat/reroute-primitives` @ `dd09fc3` → `47d5a44` (1 commit: `47d5a44`; branch now 7 ahead of `main`)
- **Session scope**: Bridge the Rust-only reroute prototypes to Python (PyO3 `reroute` binding), add C-space clearance (object-diameter) awareness, and demo the pipeline on real G55 Plan 06 geometry.
- **Touched paths**: `crates/core/src/lib.rs`, `crates/core/src/occupancy/mod.rs`, `crates/core/src/routing/mod.rs`, `crates/core/examples/reroute_demo.rs`, `python/ifcfast/reroute.py` (new), `python/ifcfast/__init__.py`, `pyproject.toml`, `AGENTS.md`; scratch (uncommitted): `scratch/g55/extract_floor6.py`, `scratch/g55/reroute_floor6_demo.py`, `scratch/g55/floor6_obstacles.npz`, `scratch/g55/reroute_floor6.png`
- **Parallel sessions observed**: none (no commits on origin/main during the window)
- **Supersedes / superseded by**: none. Follows `docs/worklog/2026-06-08-22-01_gh65-halfspace-regression-fix-v0.4.36-release-reroute-mvp.md`.

# Session: reroute PyO3 binding + C-space clearance field + G55 floor-6 demo

## Summary
Closed the gate the prior handoff named: the `occupancy`/`routing` reroute
prototypes were Rust-only with no Python entry point, so no real model could
reach them. This session built the **PyO3 `reroute` binding** (+ top-level
`ifcfast.reroute()` wrapper) and — prompted by the user mid-session — added
**C-space inflation** so a route is feasible for the object's real
cross-section, not an infinitely-thin centreline. Then demoed the whole
pipeline on **real G55 Plan 06** geometry, extracted memory-safely from the
164 MB ARK. The user's verdict on the demo: **"not impressed, but we can work
on this"** — the mechanism is proven end-to-end; the *demonstration* needs to
be more convincing (see Next).

## Changes
- **`reroute` binding (`47d5a44`)** — `_core.reroute(obstacles, requests, …)`
  (mesh feature) + `python/ifcfast/reroute.py` returning a per-request
  DataFrame. **Engine, not policy** (same split as `clash()`): takes
  caller-supplied **world-metre** obstacle meshes + start→goal requests,
  builds the go/nogo field once, routes each segment. Caller-supplied
  geometry is deliberate — lets a memory-bounded per-storey extractor feed it
  without the eager-mesher OOM (GH #67).
- **C-space clearance field** — `occupancy::build` now computes an exact
  squared-EDT (Felzenszwalb–Huttenlocher, separable 3-axis) stored as
  `Occupancy::clearance2_vox`; `Occupancy::is_clear(v, clearance_m)`
  thresholds it per object radius. `find_path` gained a `clearance_m` arg
  (start/goal snap respects it too). **One build serves every diameter** —
  thin pipe and fat duct route through the same occupancy at their own sizes,
  no obstacle dilation/rebuild. Binding takes per-request `clearances`
  (scalar-broadcast or one-per); wrapper exposes `clearance_m`.
- New Rust test `clearance_gates_a_narrow_gap` (132 lib tests green, clippy
  clean on touched files). `AGENTS.md` documents `ifcfast.reroute()` +
  decision-tree rows. `numpy` declared a direct dep (reroute.py imports it).
- **G55 floor-6 demo (scratch, uncommitted)** — `extract_floor6.py` pulls
  Plan 06 walls/columns/ceilings/slabs from the 164 MB ARK + beams from the
  RIB, **peak 1.88 GB / 10 s**; `reroute_floor6_demo.py` builds a 1.18 M-voxel
  occupancy (**261 ms**), routes a duct corner-to-corner at 4 diameters, and
  renders a matplotlib plan view (`reroute_floor6.png`).

## Technical Details
- **Why the binding takes meshes, not an IFC path.** The whole point of GH #67
  is that `Model.meshes()` meshes eagerly and OOMs on the 156–164 MB ARK. So
  the binding consumes already-extracted world-metre triangle soup; Python
  does the memory-bounded per-storey extraction via ifcopenshell
  `iterator(include=…)`. Confirmed safe: ARK opens no-geom at 1.88 GB, the
  included floor-6 subset tessellates within that envelope.
- **The user's "voxelise by diameter" → C-space inflation.** User pitched
  accounting for the object's size, worried a fixed inflation "introduces more
  space than needed," and asked for best-practice ("channels of free space").
  Reframed and built the principled form: the EDT distance field *is* the
  channel-width map; threshold it per-object-radius so fine cells stay fine
  and object size enters only as a threshold (build-once / route-many-radii).
  Medial-axis / GVD roadmaps are the "channels" formalisation but give
  *maximum*-clearance routes; MEP wants *compact*, so feasibility = clearance
  threshold, optimality = A* (clearance-bias is a documented future knob).
- **ifcopenshell gotcha (cost me one extract cycle).** The geometry kernel
  returns vertices already in **SI metres** (it applies the model's mm→m
  length-unit scale). My initial `*0.001` shrank everything 1000× (bbox
  collapsed to a point at ~0.1 m). Fix: no rescale. Verified by raw vert
  z≈20.3 == floor-6 level.
- **Build discipline held.** All builds were debug `maturin develop` with
  `CARGO_BUILD_JOBS=4` — no `--release` LTO (the v0.4.36-session OOM source).
  Each rebuild ~10 s (deps cached). No OOM this session.
- **Plan 06 anatomy** (via ifcfast tier-1 index, no geom): storey "Plan 06" at
  z=20.30 m, next "Plan 07" at 23.90 m. Columns/walls span the full height;
  suspended ceiling at z≈22.75–23.60; slab-above at 23.8–23.9 → the *true*
  plenum is only ~0.2 m (too thin for a clean plenum-route demo; I routed a
  quasi-planar run at z=21.6 m through walls/columns/beams instead).

## Next
- **The demo needs to land better** (user "not impressed"). Concrete levers:
  (1) a genuinely pinched passage on Plan 06 where a fat duct gets a hard
  "no route" — show the failure, not just longer detours; (2) a *real plenum*
  route — clip the grid Z to the ceiling→slab void so it routes the way MEP
  actually runs; (3) reroute-on-clash framing — run `ifcfast.clash()`, take a
  real MEP-vs-structure hit, move the offending element; (4) 3D view, not just
  plan; (5) speed (~9 s per 57 m run at 0.15 m over a whole floor — sparse /
  plenum-band grid or roadmap reduction). **Ask the user which story they want
  before building** — the mechanism is proven, the framing is the open
  question.
- **GH #63 sub-items** (unblocked now that Python can reach the router):
  property-based `SystemKind` inference, fall-grade on drainage, path
  simplification (collapse the A* voxel staircase), substrate wiring
  (`reroute()` public + `axes.parquet`).
- **Branch decision still open:** `feat/reroute-primitives` is 7 ahead of
  `main`, unpushed. Push / PR is gated — ask per case.

## Notes
- Routing perf: A* over 1.18 M voxels is ~9 s per cross-floor run. Fine for a
  demo, a real bottleneck for interactive use. The dense whole-floor grid is
  the cost; a plenum-band or sparse grid is the obvious fix.
- Demo scripts stay in `scratch/g55/` uncommitted — they hard-code the
  client's private G55 ACC paths. The `.npz`/`.png` are byproducts.
- "Collision-free" in the demo is at 0.15 m voxel resolution. Walls ~0.2 m
  thick rasterise to 1–2 voxels.
- The C-space clearance field adds an EDT pass to every `occupancy::build`
  (~unmeasured but ≤ build time on the floor-6 grid: 261 ms total). Eager, not
  lazy — build doesn't see the request radii. Fine for per-floor grids.
