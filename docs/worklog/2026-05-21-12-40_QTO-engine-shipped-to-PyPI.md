# Session: Mesh QTO engine — shipped to PyPI as a standalone product

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `3109604` → `fb45069` (4 release tags pushed: v0.2.0, v0.3.0, v0.4.0; v0.2.0 carried the prior session's Arc<str> work that hadn't been released)
- **Session scope**: Cut v0.2.0 to ship the prior session's bundle interning; built a per-product mesh QTO engine (volume + area + orientation buckets + per-surface UNNEST) into the bundle (v0.3.0); then made it standalone-reachable from the PyPI wheel via `Model.mesh_qto()` (v0.4.0). Side branch: merged the site's `Types`/`Untyped` tabs into a single Types view per the user's classification-status framing.
- **Touched paths**:
  - ifcfast: `Cargo.toml`, `crates/core/Cargo.toml`, `pyproject.toml`, `python/ifcfast/__init__.py`, `python/ifcfast/model.py`, `crates/core/src/mesh/mod.rs`, `crates/core/src/mesh/qto.rs` (new), `crates/core/src/bundle/record.rs`, `crates/core/src/bundle/parquet_sink.rs`, `crates/core/src/lib.rs`, `CHANGELOG.md`
  - ifcfast-site: `components/qto-panel.tsx`, `components/selection-context.tsx`, `components/vector-graph.tsx`, `components/viewer.tsx` (separate repo, also created `EdvardGK/ifcfast-site` on GitHub + production deploy on vercel)
- **Parallel sessions observed**: none on origin/main during this window. Another agent flagged a stale Windows clone (`C:/workspace/toolkit/ifcfast` on `examples/graph-viewer` @ v0.1.0-era) but didn't push anything.
- **Supersedes / superseded by**: none

## Summary

Three big things in sequence:

1. **Released v0.2.0.** The prior session shipped Arc<str> interning + zero-clone regrouping in `Bundle::build` but never tagged. Bumped pyproject + workspace + crate + `__init__.py` from 0.1.0 → 0.2.0, wrote CHANGELOG entries, tagged. Release workflow built 6 wheels + sdist + Trusted-Publisher to PyPI in one shot.

2. **Built the geometric QTO engine** (`crates/core/src/mesh/qto.rs`, new) — the most-important-feature-of-ifcfast that I had somehow missed during the prior session despite `mesh/stats.rs` having had a partial version for ages. The user reframed twice:
   - "m3, m2, break it down into surfaces, angles etc"
   - "I want to be able to ask 'whats the biggest and smallest surface on type X' and get 'instant' answers"
   - "any mesh needs to be calculated FAST, hence the ifcfast name"

   What I built does all of this in one O(triangles) inline sweep over the world-coord mesh inside `pair_split`:
   - `volume_m3` — signed-tetrahedra divergence (stored as |volume|)
   - `aabb_volume_m3` — bbox volume (compactness proxy)
   - `surface_area_m2` — total
   - `area_top_m2` / `area_bottom_m2` — triangles within 20° of ±Z (walkable surfaces / soffits)
   - `area_side_m2` — within 20° of horizontal plane (wall sides, façades, doors)
   - `area_inclined_m2` — everything else (ramps, sloped roofs, chamfers)
   - `largest_surface_m2`, `smallest_surface_m2`, `surface_count` — scalar shortcuts
   - `surfaces: List<Struct<area_m2, nx, ny, nz>>` — every distinct planar surface, normal-bucket aggregated at ~5.7° granularity (quantized to 0.1 per normal component), sorted by area descending

   The bucket key is a `(i32, i32, i32)` triple in a small linear-probe `Vec` (fits the typical 6-20 surfaces per product without paying HashMap overhead); falls back to a HashMap past 64 buckets for curved geometry. All values scale via the IFC's `unit_scale` so output is in m² / m³ regardless of source unit.

   Released as v0.3.0 with 11 new non-nullable columns on `instances.parquet` and a new `surfaces` List column.

3. **Standalone Python surface (v0.4.0).** The first thing the user pointed out after v0.3.0: "it needs to be a standalone product. Cant require cloning the repo." Correct — the QTO engine was compiled into the wheel but only reachable via the `ifcfast-bundle` binary (feature-gated, not built into the Python wheel). Added `_core.mesh_qto(path)` PyO3 binding + `Model.mesh_qto()` Python wrapper that returns `(products_df, surfaces_df)` pandas tuple. Now `pip install ifcfast` is enough.

## Validation

**Unit tests (`qto.rs`)** — 5 tests on a unit cube: total V=1 m³, A=6 m², orientation buckets sum correctly (top 1 + bottom 1 + side 4 + inclined 0), six distinct planar surfaces each 1 m², mm→m unit_scale path, degenerate inputs return zero.

**Duplex (2.4 MB, 286 instances)**:
- Big wall: V=0.707 m³, A=12.60 m² with top=0.24 + bottom=0.24 + side=12.12, 6 distinct surfaces (two 5.70 m² main faces, two 0.36 m² end caps, two 0.24 m² top/bottom). Exact QTO decomposition of an interior wall.
- 2817 distinct planar surfaces across the file.

**ST28_RIV (834 MB IFC, 27M triangles, 85976 instances)**:
- Pre-QTO baseline (post-Arc<str>): 30.28 s, 2627 MB peak RSS, 69.3 MB output.
- Post-QTO: **33.98 s** (+12%), 2628 MB peak RSS (flat), 99.2 MB output (+30 MB for the `surfaces` list).
- Per-triangle cost: ~140 ns including normal bucket probe. Streaming-pass overhead from the QTO sweep is the +3.7 s.

**Query latency** (on the materialized `instances.parquet`):
- biggest surface anywhere across 86K rows: **3 ms**
- group-by-entity (`max(largest_surface_m2)` + `sum(volume_m3)`): **10 ms**
- Loading the full 9.8 MB parquet into pyarrow: 534 ms.

That's the "instant" bar.

**Python wheel verification** (after v0.4.0 publish):
```python
import ifcfast
m = ifcfast.open(".local-samples/Duplex_A_20110907.ifc")
products_df, surfaces_df = m.mesh_qto()
# shapes: (286, 12) and (2817, 6)
# walls: top wall comes back with V=122.87 m³ A=176.95 m², biggest face 125.74 m² with normal (1, 0, 0)
```

## Side branch — ifcfast-site Types/Untyped merge

User asked to merge the QTO panel's `Types` and `Untyped` tabs into one. Implemented per their classification-status framing: untyped products become a single `Untyped` pseudo-row per entity inside the merged Types view, italic-muted to distinguish from a real IfcTypeObject name. `selection-context.tsx`'s `untyped` selection kind gained an optional `entity` scope so clicking that row narrows to "untyped products of THIS entity". Wired through `vector-graph.tsx` and `viewer.tsx`. Deployed to a Vercel preview (the user hasn't promoted to production yet). Also created `EdvardGK/ifcfast-site` on GitHub (was local-only) and pushed master.

## Honest scope gaps

- **The PyPI wheel still doesn't ship `ifcfast-bundle`** — that binary needs `--features bundle` which pulls heavy arrow + parquet deps. Users who want the DuckDB-queryable substrate still need a cargo build. The Python `m.mesh_qto()` path covers the "I just want the numbers in pandas" use case without that.
- **Normal-bucket aggregation under-merges curved surfaces.** A 12-segment cylinder reads as 12 distinct wedge surfaces, each ~1/12 of the side area. Correct for QTO ("largest planar approximation") but not what someone asking "biggest surface on this pipe" probably expects. A flood-fill connected-coplanar grouping with smooth-curvature merging is the v2.
- **No `surface_count` cap.** A high-poly free-form mesh could blow up the per-surface list. Today the cap is implicit (linear-probe Vec grows past 64 → HashMap; HashMap unbounded). Add an explicit max-K-by-area truncation if a real file exposes a problem.
- **`mesh_qto` walks the file from scratch each call** — no caching layer like the tier-1 extracts have. Cheap on Duplex; on ST28_RIV-class files it's ~30 s end-to-end. A `Model.mesh_qto(cache=True)` that writes a sidecar would help, but defer until a real consumer hits the cost.

## Next

1. **Wire `m.mesh_qto()` into the site's QTO panel** so the panel's empty m³/m² columns get filled from the geometric QTO when authored `Qto_*` is missing. That's the original "G55 ARK has no Qto_*" gap the prior session flagged.
2. **Promote the site preview to production** once the user's Turbopack PostCSS worker crash is resolved on their side.
3. **Cache layer for `mesh_qto`** — sidecar parquet next to the tier-1 cache so re-running on the same file is near-instant.
4. **Curved-surface connected-coplanar grouping** — flood-fill over triangle adjacency with smooth-curvature merging, for "biggest surface on a cylinder = the whole cylinder side, not 1/12 of it".
5. **Bundle in wheel?** Decide whether the wheel should also ship `ifcfast-bundle` (would inflate by ~5-10 MB for arrow + parquet) or stay opt-in via `cargo install --features bundle`. The `m.mesh_qto()` Python path means most users don't need the binary; keep it opt-in unless someone asks.
6. **`.ifczip` silent-drop (#19)** — still queued, separate session.
7. **`shape_cache` eviction / disk-backing** — the unbounded mesh cache in `mesh_ifc_streaming` is the actual peak-RSS killer on big files. Per-rep-id ref count from the indexer → evict on last use, or move to mmap-backed temp file.

## Numbers reference

| version | what landed | wheel impact |
|---|---|---|
| **0.2.0** | bundle Arc<str> interning + zero-clone regrouping (prior session) | bundle-only |
| **0.3.0** | per-product QTO engine in `pair_split` → instances.parquet | bundle-only |
| **0.4.0** | `Model.mesh_qto()` PyO3 binding + Python wrapper | **wheel-reachable** |

| file | products | surfaces | wall-clock | peak RSS |
|---|---:|---:|---:|---:|
| Duplex (2.4 MB) | 286 | 2817 | 0.05 s | 19 MB |
| ST28_RIV (834 MB) | 85976 | ~700K | 33.98 s | 2.6 GB |
