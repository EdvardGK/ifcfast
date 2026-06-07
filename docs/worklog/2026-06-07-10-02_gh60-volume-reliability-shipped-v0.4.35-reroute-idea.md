## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `9385218` → `9774028` (2 commits: GH #60 feature, v0.4.35 release prep)
- **Session scope**: implement GH #60 (volume-reliability flag + prism fallback), cut the v0.4.35 Phase-1 release, and capture the user's MEP-rerouting passion-project idea for a future session
- **Touched paths**: crates/core/src/mesh/qto.rs, crates/core/src/bundle/record.rs, crates/core/src/bundle/parquet_sink.rs, crates/core/src/lib.rs, python/ifcfast/header.py, python/ifcfast/model.py, AGENTS.md, examples/hybrid_qto_routing.py, Cargo.toml, Cargo.lock, pyproject.toml, CHANGELOG.md
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: none (continues the prior session's `2026-06-07-08-51_*` Next list)

## Summary

Implemented GH #60 — the volume-reliability columns + prism fallback the
prior session designed — but **corrected the design under empirical
pressure**: reliability is NOT `mesh_quality == "closed"` (that
over-flags 14.5%), it's a *tight prism tripwire* that keeps the mesh
value when it's within its `footprint × height` bound and substitutes
the prism only when the mesh is provably too big. Then cut the bundled
**v0.4.35** Phase-1 release (the user chose "release now, W6 next").
Finally captured the user's constraint-aware MEP-rerouting idea as GH #63
+ memory for a future design session.

## Changes

- **`qto.rs` (`496f78f`)** — `MeshQto` gains `volume_reliable` (bool),
  `volume_method` (`"mesh"`/`"prism_fallback"`), `volume_best_m3`,
  `volume_prism_bound_m3`. New `footprint_xy_raw` (256-cell raster of the
  XY-projected triangle union) + `point_in_triangle`. Reliability logic:
  closed → trust mesh (raster-free); non-closed → compute prism, keep
  mesh if `|v| ≤ prism×1.05` else substitute prism + flag. 5 new unit
  tests.
- **`record.rs` / `parquet_sink.rs`** — 4 new substrate columns
  (`volume_mesh_m3`, `volume_prism_bound_m3`, `volume_reliable`,
  `volume_method`); `volume_m3` now carries the best estimate.
- **`lib.rs` (`mesh_qto`)** — QtoSink + dict gain the same columns +
  `mesh_quality` now exposed.
- **`header.py`** — cache schema 15 → 16 (column-shape + `volume_m3`
  semantics change).
- **`model.py` / `AGENTS.md`** — docstrings + agent-facing column docs.
- **`examples/hybrid_qto_routing.py`** — reads `volume_reliable` directly.
- **`v0.4.35` release (`9774028`)** — version bump (Cargo.toml +
  pyproject.toml + Cargo.lock), CHANGELOG entry, tag pushed → release CI.

## Technical Details

- **The design correction (the load-bearing decision).** Validated each
  candidate rule against ifcopenshell ground truth on G55_RIB's 108
  non-closed rows: substitute-prism-for-all-non-closed = 27% median error
  (regresses the 89 accurate-but-edge-pairing-flagged walls from 0.2%);
  keep-mesh-always = 96.8% *mean* error (the 18 genuinely-broken columns).
  The tight tripwire (`|v| > footprint×height×1.05` → substitute) gives
  median 0.24%, **0 regressions, 18 fixes**. Tolerance is insensitive
  (1.02–1.20 all flag the same 18 — violations are 8× over, not
  marginal). The prism is a *tighter provable upper bound* than the AABB
  (`footprint ≤ x_extent·y_extent`), so it subsumes the loose `|v|>aabb`
  tripwire and catches the columns the loose one misses.
- **FBSM slab #57**: mesh 1504 m³ garbage → prism 180.1 m³ = Solibri's
  QTO value (180.2) to within 0.1. Flagged set dropped 14.5% → 2.5%.
- **Perf**: raster runs only on non-closed rows (~14%); G55_RIB
  `mesh_qto` ~944 ms warm. `volume_prism_bound_m3` is `NaN` on closed
  rows (raster-free hot path).
- **Release**: 4-platform cross-compile (~12+ min); was still
  `in_progress` at worklog time — background watcher `boivzzh9b` monitors.

## Next

1. ~~Confirm v0.4.35 published~~ — **DONE, release CI green (12m39s,
   run 27093068432).** v0.4.35 is on PyPI.
2. **W6 — polygon-bounded halfspace correctness fix** (GH #58, last
   Phase-1 piece). Blueprint at `docs/plans/2026-06-05_*` lines 199–333:
   carry `BoundedHalfspacePayload` on `ProductMesh` (approach 2), add
   `is_tight_boundary` + 2D-reduction fast path in `cut_openings::apply`
   behind `prism-csg-fast`, 3 differential tests
   (`WALL_WITH_TIGHT_BOUNDED_HALFSPACE`). Default build byte-identical.
3. **GH #62** — prism bound: min over 3 axis projections + optional exact
   i_overlay union (deferred follow-up from #60; v1 is Z-prism only).
4. **GH #63 (future, user passion project)** — constraint-aware MEP
   rerouting; design session not yet held. See memory
   `reroute-on-clash-idea`.

## Notes

- `.venv` has the `csg`+`prism-csg-fast` wheel; `/tmp/g55_qto/` has the
  authoritative G55 IFCs + ITO xlsx for re-validation.
- CHANGELOG had a pre-existing gap (0.4.32–0.4.34 shipped without
  entries); the 0.4.35 entry notes this rather than backfilling fabricated
  history. A clean backfill from those worklogs is a low-priority chore.
- Clippy "else-if/overindented-doc" warnings in qto.rs are pre-existing
  (the AABB-update lines + the `mesh_quality` doc list); CI gates on
  `cargo test`, not clippy. New code matched the established style.
