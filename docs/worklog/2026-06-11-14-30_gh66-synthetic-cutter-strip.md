# GH #66 — strip synthetic half-space cutter slabs from every no-cut surface

**Agent:** Claude (edkjo, the-super-tester/sidehustles) — tester-in-chief, per Ed's "dig into the
mesh calculations, solve broken meshes clearly or add acceptable flagged shortcuts".
**Branch:** `fix-66-synthetic-cutter-strip`. No parallel sessions on this repo.

## Root cause (empirical + code)

#66's "54×54 m degenerate slab plane" decomposes into **3 connected components**: the real slab
(5.6×7.8×0.26 m) + two `±HALFSPACE_PLANE_EXTENT` (=±20 000 model units → 40 m square, rotated →
54 m AABB) visualisation stand-ins for the slab's two infinite `IfcHalfSpaceSolid` cutters
(`Body=Clipping`, BCR DIFFERENCE chain). `mesh_qto` was right because the Python wrapper forces
`cut_openings=True` → `cut_openings::apply` consumes the cutters; every no-cut surface shipped
them as element geometry. Blast radius measured before the fix: **26 contaminated representations
in a real architectural bundle** (clash false-positive feed), point-cloud sampling budget soaked
by 40 m slabs, drift/instances AABBs foreign-extent (part of the #94 aabb>100× population).

## Change

- `mesh::strip_synthetic_cutters(&mut ProductMesh) -> u32` + `is_synthetic_cutter_tag` —
  removes fragments whose chain has `boolean_second_operand` + `halfspace_plane*`/`halfspace_bounded*`;
  rewrites indices/segments/parts in lockstep and **compacts vertices** (stats.rs and
  bundle::record AABB over the raw buffer). 4 unit tests incl. interleaved-segment ordering.
- Wired as the exclusive alternative to `apply` (never before it — apply consumes the cutters as
  payloads): MeshSink (behind new `keep_cutters` opt-out), QtoSink no-cut path, both cloud sinks,
  GltfSink, bundle ParquetSink (unconditional), `analyse_drift` (drift + segments tables).
- Python: `meshes()`/`iter_meshes(keep_cutters=False)`; `extract_meshes` reports
  `cutters_stripped` + echoes `keep_cutters`. Authored solid subtractors / union operands untouched.
- `_CACHE_SCHEMA_VERSION` 16 → **17** (drift/segments value change). AGENTS.md reveal-all section
  updated with the exception; CHANGELOG.

## Verification (real models, patched wheel vs published 0.4.37)

- #66 GUIDs: 1 component each, true extents (5.6×7.8 / 8.1×14.0 m); `keep_cutters=True` restores
  3 components; 216 cutters stripped across G55_ARK.
- **Bundle decontamination: 26 → 0** halfspace-tagged reps on the ARK_E substrate.
- **Cut-path invariance: max |Δvolume| = 0.000000000 across 12 309 products** (default-features
  build; an earlier 62-row delta was the `prism-csg-fast` feature flag in my build config, not
  the change — characterized: all 62 were `halfspace_bounded` W6 rows).
- Suites: cargo 117+15 (mesh) and 151 (mesh+prism-csg-fast) green; pytest 95 passed/15 skipped
  (3 extra skips = no `mcp` in the test venv).
- drift for the #66 slabs: aabb sane (0 rows >100 m³).

## Build gotchas (recorded for next time)

Deep repo path breaks `manifold-csg-sys`'s TBB FetchContent (MAX_PATH) → `CARGO_TARGET_DIR` to a
short path + `git config --global core.longpaths true`. And `cmd //c relative\path.bat` from Git
Bash silently no-ops with exit 0 when piped — check the log for "Compiling", never the exit code.
