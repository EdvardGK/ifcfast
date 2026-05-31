## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `87b1c29` → `046e196` (one commit this session)
- **Session scope**: After the v0.4.19 release tag landed and CI published the wheel, wire the geom::csg kernel (committed in 2420423 but unused) into `m.meshes()` as an opt-in `cut_openings=True` so doors and windows render as actual holes — viewer-integrator's P0 #1 from GH #20.
- **Touched paths**: `crates/core/src/mesh/cut_openings.rs` (new), `crates/core/src/mesh/mod.rs` (module decl), `crates/core/src/lib.rs` (extract_meshes PyO3), `crates/core/tests/cut_openings_integration.rs` (new), `python/ifcfast/model.py` (meshes / iter_meshes signature), `AGENTS.md`, `CHANGELOG.md`
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: continues `2026-05-30-23-30_clash-and-csg-kernel.md`

## Summary

v0.4.19 published cleanly on PyPI after the previous session's tag
push — 5 platforms × all green, Publish to PyPI step uploaded all
wheel artifacts. With the release out and the CSG kernel already
sitting in the source tree (commit 2420423, off-by-default `csg`
feature), this session built the natural consumer: an opt-in
`cut_openings=True` flag on `m.meshes()` that subtracts the
`boolean_second_operand|...` segments from their host via
`manifold-csg`, producing a single net-solid mesh per product.

The key implementation insight came from the exploration in the
prior session: ifcfast's mesh extractor already tags every
triangle's provenance in `ProductMesh.segments` — the host gets
`"boolean_first_operand|extrusion"` and the void gets
`"boolean_second_operand|extrusion"`. So this session's work is
purely consumer-side: partition the existing segments, assemble
host + cutter sub-meshes via vertex remap, hand to
`geom::csg::subtract_many`, rewrite the mesh. No changes to the
mesh extractor itself — reveal-all stance preserved end-to-end at
the substrate boundary.

## Changes

**Commit `046e196`** (the only commit this session, 7 files, +729):

- `crates/core/src/mesh/cut_openings.rs` (new) — `apply(&mut ProductMesh)
  -> Outcome` with three outcomes (`Cut`, `Passthrough`, `Fallback`)
  accumulated into a `CutOpeningsStats` counter. Source-tag
  partition uses `split('|').any(== "boolean_second_operand")` to
  handle nested booleans whose chain tag carries multiple links.
  `assemble_submesh` does the compact vertex remap (each segment's
  triangles + only their referenced vertices, remapped to 0..N) so
  the sub-meshes handed to manifold are minimal and don't drag
  unreferenced verts. On `Cut`, `mesh.vertices` /
  `mesh.indices` / `mesh.segments` are replaced with the net solid
  (single segment tagged `"cut_openings"`) and `mesh.parts` is
  cleared because the per-fragment instance-dedup payload is
  invalidated by the CSG.

- `crates/core/src/lib.rs` — `extract_meshes` PyO3 binding gains
  `cut_openings: bool = False` via pyo3 signature. Without the
  `csg` Cargo feature compiled in, calling with `cut_openings=True`
  raises `PyRuntimeError` with a clear "rebuild with --features
  csg" message rather than silently no-op. Return dict gains
  `cut_openings_cut` / `cut_openings_passthrough` /
  `cut_openings_fallback` counters so callers can audit how the
  pass actually went on a real model.

- `python/ifcfast/model.py` — `Model.meshes()` and
  `Model.iter_meshes()` gain matching `cut_openings: bool = False`
  params with the full contract documented inline (in-representation
  booleans only; cross-product `IfcRelVoidsElement` openings flagged
  as a follow-on).

- `crates/core/tests/cut_openings_integration.rs` (new) — three
  integration tests against synthetic IFC4 fixtures:
    * `reveal_all_emits_both_operand_segments` — confirms the
      extractor still emits both operands as separate segments by
      default.
    * `cut_openings_produces_single_segment_and_reduced_volume` —
      cut wall has one `"cut_openings"` segment and volume drops
      from 0.6 m³ (host) to ≈ 0.4 m³ (host − opening's 0.2 m³
      overlap).
    * `cut_openings_is_a_no_op_on_solid_wall_without_boolean` — a
      plain `IfcExtrudedAreaSolid` wall passes through untouched.

- `AGENTS.md` — "Reveal-all geometry stance" section now documents
  the opt-in cut path; decision-tree row updated.

- `CHANGELOG.md` — `[Unreleased]` records the cut_openings flag,
  the new `csg` Cargo feature, the cross-product opening follow-on
  scope, and the wheel-default deferral.

## Technical Details

**Source-tag partition rule.** The extractor's compound source tag
threads structural roles through nested booleans — e.g. a wall
nested inside another boolean produces tags like
`"boolean_first_operand|boolean_second_operand|extrusion"`. The
partition's correctness depends on classifying based on chain
membership rather than the leaf alone: a triangle is a cutter if
ANY link in its source chain is `"boolean_second_operand"`,
because that means it was structurally on the subtract side of
some boolean node somewhere up the tree. Implemented as
`source.split('|').any(|link| link == "boolean_second_operand")`.

**Compact vertex remap for sub-meshes.** `mesh.vertices` is shared
across all segments; segments only slice the index buffer. Naive
assembly would hand manifold a 1000-vertex buffer where only 8
vertices are actually referenced by the second-operand triangles —
wastes memory and risks BVH-unfriendly topology. The remap
collects referenced vertices into a fresh buffer and rewrites
indices to 0..N. Per-segment HashMap lookup means each unique
vertex is emitted once.

**Outcome::Fallback is the surfaced failure mode.** When manifold
rejects the cut (typically because the host mesh isn't manifold —
open shells, self-intersections, inconsistent winding from
IfcFacetedBrep edge cases), the original mesh stays in place.
This matters because v1 of cut_openings runs against real model
files; some will have authoring issues. Reveal-all is the safety
net, not a silent error. The `cut_openings_fallback` counter in
the return dict is the agent's signal that the file needs manual
attention on those products.

**Why parts is cleared post-cut.** `ProductMesh.parts` carries
per-fragment InstancePart entries used by the substrate writer for
representation deduplication (5000-window facade → one rep + 5000
instances). After a CSG cut, the resulting mesh is unique to that
specific host-cutters combination — not shareable with other
instances of the same shared rep. The parts payload would be lies.
Clearing it correctly signals "no dedup possible" to any downstream
consumer that walks parts. In practice the substrate writer never
invokes cut_openings (it's an output-side flag, not a
substrate-write flag) so this is defense-in-depth rather than a
load-bearing invariant today.

**Test inventory:** 97 lib (incl. 5 new `mesh::cut_openings`) + 4
bundle + 4 clash + 3 cut_openings integration + 13 mesh_reveal =
**121 tests green**.

## Next

Direct follow-ons:

1. **Cross-product `IfcRelVoidsElement` cut path** — handle the
   older authoring pattern where a solid IfcWall has a
   separately-modelled IfcOpeningElement attached via
   IfcRelVoidsElement (no boolean in the wall's own
   representation). The mesh extractor needs to thread the
   indexer's `voids_opening` / `voids_host` arrays. Algorithm
   reuses `geom::csg::subtract_many` — only the host→cutters lookup
   path is new. Buffered / two-pass approach needed because
   streaming order is entity-table-order and the opening can be
   emitted before the host.

2. **Wheel default for `csg`.** Wait for `release.yml` to grow a
   cross-platform smoke-test job that builds the wheel with `csg`
   enabled — the cmake-built Manifold C++ core needs to verify on
   linux x86_64 + aarch64, windows x64, macos x86_64 + aarch64
   before flipping the default. Once green, `default = ["python",
   "bundle", "geom", "clash", "csg"]` and `m.meshes(cut_openings=
   True)` works on `pip install ifcfast` out of the box.

3. **`m.to_gltf(path, *, cut_openings=True, instancing=True)`** —
   the viewer integrator's one-call viewer export. Wraps the
   existing gltf writer + this session's cut path + the still-to-do
   `EXT_mesh_gpu_instancing` (uses bundle's rep/instance dedup).

Parallel / unrelated:

- **Rayon parallelization** of `mesh_ifc_streaming` (GH #20 P0 #2,
  unchanged from prior session's next-steps).
- **OOBB + principal-axis fingerprint columns** (cache schema v5 →
  v6) — pre-req for the geometry hot-swap roadmap arc
  ([[roadmap-geometry-hotswap]]).

## Notes

- v0.4.19 wheel on PyPI does NOT include cut_openings — the `csg`
  feature is off by default. Users who want to try this session's
  work need a source build (`pip install --no-binary :all: ifcfast`
  with `MATURIN_PEP517_ARGS="--features csg"` env, or
  `git clone` + `maturin develop --features csg`).
- 1 commit unpushed locally: `046e196`. Push when ready; no tag
  needed yet — the cut_openings work doesn't ship to wheel users
  until the `csg`-in-default decision lands.
- `.claude/` directory holds session-local state
  (`scheduled_tasks.lock`, `settings.local.json`) — intentionally
  not staged. Worth adding to `.gitignore` as a hygiene follow-on.
