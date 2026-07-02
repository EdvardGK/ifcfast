## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `4922bce` → `df678fa` (3 commits this session)
- **Session scope**: close subset completeness gap (#126), then ship the #124 north-star write payload — one-element mesh hotswap
- **Touched paths**: crates/core/src/doc/{rel_rules.rs,hotswap.rs,mod.rs}, crates/core/src/lib.rs, crates/core/tests/{doc_rel_rules.rs,doc_hotswap.rs}, tests/fixtures/{rel_field_pinning.ifc,hotswap_body.ifc,hotswap_body_2x3.ifc}, tests/{test_subset.py,test_hotswap.py}, python/ifcfast/model.py, AGENTS.md
- **Parallel sessions observed**: none (all 3 commits authored by this tree)
- **Supersedes / superseded by**: none

# Session: GH #126 subset rels + GH #124 Phase 3 mesh hotswap

## Summary
Two write-axis increments, both shipped to `main` and corpus-gated. First
closed the subset completeness gap (#126) by teaching the rel pass about
coverings and system-service relationships. Then built the #124 north-star
payload: `m.hotswap(guid, verts, tris)` — replace one element's Body
geometry with a new triangle mesh, with refcount-based orphan GC and
schema-aware output. The load-bearing discovery: **3 of 4 G55 disciplines
are IFC2x3**, where `IfcTriangulatedFaceSet` doesn't exist — caught by the
corpus gate after an IFC4-only first cut, forcing a dual-dialect emitter.

## Changes
- **#126 (`aa023fb`)** — `doc/rel_rules.rs`: added `IfcRelCoversBldgElements`
  (anchor=host@4 single, pull=coverings@5 **SET** — void-parallel, kept wall
  drags its finishes) + `IfcRelServicesBuildings` (anchor=system@4, pull=
  buildings@5 SET — activates only on a seeded system, no ballooning). First
  **SET pulls**; relaxed the single-pull invariant doc note (machinery
  already iterates `pull` as a Vec, so it's safe — pull refs only ever
  added to keep, never rewritten). `IfcRelConnectsPathElements` deliberately
  left dropped. 13 rules now. +2 pinning fixtures, + Python corpus gate
  `test_subset_pulls_coverings_when_host_seeded`.
- **#124 Phase 3 (`4944a19`)** — new `doc/hotswap.rs`: traversal
  `Representation@6 → IfcProductDefinitionShape.Reps@2 → 'Body' shaperep`,
  two-field byte-splice (Items→new root, RepType→dialect tag), refcount
  orphan GC, schema-aware geometry builder. PyO3 `hotswap_ifc` +
  `Model.hotswap` (NumPy-friendly via `_as_rows`). 6 Rust unit tests + 2
  fixtures + Python local & corpus gates. AGENTS.md: decision-table row,
  `m.hotswap` section, "does NOT do" edit.
- **Doc warning (`df678fa`)** — AGENTS.md ⚠️ on `m.hotswap`: `m.meshes()` is
  world-frame, hotswap wants local-frame → naive round-trip double-applies
  placement (GH #127 filed).

## Technical Details
- **Orphan GC is the crux.** After repointing the Body rep, compute the old
  items' forward closure, then a refcount fixpoint over the *post-swap*
  graph (the body rep contributes its new refs, not old items). Peel any
  closure member with zero inbound refs, cascade. A shared
  `IfcRepresentationMap` referenced by other instances keeps a positive
  count → survives automatically; only per-instance items (mapped items,
  swept solids) are reclaimed. Pinned with a two-wall shared-map fixture.
- **Schema dialect.** IFC4/IFC4x3 → `IfcTriangulatedFaceSet` +
  `IfcCartesianPointList3D` (compact, 2 records). IFC2x3 →
  `IfcShellBasedSurfaceModel` over an `IfcOpenShell` of
  `IfcFace`/`IfcFaceOuterBound`/`IfcPolyLoop` (N + 3M + 2 records; open-shell
  tolerates non-closed meshes). Detected by scanning `FILE_SCHEMA` in the
  header prefix. RepresentationType `'Tessellation'` vs `'SurfaceModel'`.
- **Gotcha — ifcopenshell dangling check is weak.** `inst.get_info(
  recursive=False)` does NOT follow refs, so an unresolvable `Items` ref
  reads as empty (0 dangling) rather than an error. The real gate is
  `body.Items[0].is_a(...)`, which errors on an emptied Items list — that's
  what surfaced the IFC2x3 miss.
- **PyO3 stats-field renames must sync `lib.rs`.** Renamed HotswapStats
  fields (`new_faceset`/`new_point_list` → `new_geometry`/`new_records`)
  compiled under `--no-default-features` but broke the `python` feature
  build; caught by a targeted `cargo build --features python,mesh,csg`.

## Next
- **GH #127** — local-frame mesh bridge so extract→decimate→swap round-trips
  work (lean: `m.meshes(frame="local")`). This is what makes the headline
  decimation use-case usable.
- General attribute mutation (properties/placement) — the remaining write axis.
- QTO reliability cluster still open: #123, #62, #119/#120.

## Notes
- All 3 commits pushed to `main` (push-to-main autonomous per project
  convention). #126 closed via "Closes #126"; #124 epic updated with a
  progress comment; #127 filed.
- G55 corpus lives in `scratch/g55/` (gitignored). Corpus gates:
  `IFCFAST_SUBSET_CORPUS="a.ifc:b.ifc" pytest tests/test_{subset,hotswap}.py
  -k real_corpus -s`. Build the Python surface with `unset CONDA_PREFIX` +
  `maturin develop` (debug — never `--release` on 16GB).
- No cache-schema bump: hotswap is a writer, adds no extractor columns.
