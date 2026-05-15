# Session: tier-1 perf (-30%) + spatial-relationship graph layer

## Summary

Two stacked deliverables on the still-uncommitted v0.1.0 branch. First,
a 22-30% speedup of the tier-1 indexer hot path, driven by replacing
the per-record if-else dispatch chain with a single `HashMap` lookup
and skipping `std::parse::<u64>()` in favour of a tight wrapping loop.
Second — a real graph layer on `Model`: three relationship DataFrames
(`m.contained_in` / `m.aggregates` / `m.storey_building`) plus seven
traversal helpers that unify spatial containment and decomposition so
a single `m.ancestors(wall_guid)` walks all the way to the project.
The relationship tables persist in the parquet cache (bumped to v2),
so hot reloads keep graph access at full speed.

## Changes

### Tier-1 perf (Rust)

- `crates/core/src/indexer.rs` — new `EntityKind` enum + lazy
  `OnceLock<HashMap<&[u8], EntityKind>>` for single-lookup dispatch.
  `extract_product` / `extract_storey` take pre-split fields. Entity-
  name canonicalisation (`IFCWALL` → `IfcWall`) is now O(1) via a
  second `OnceLock<HashMap>`. `type_counts.entry(entity.clone())`
  replaced by get-mut/insert so the String is owned by the map only
  on first occurrence. `contained_in_*` post-filter is now write-
  index-in-place — no two extra `Vec`s.
- `crates/core/src/lexer.rs` — `parse_record` switched from
  `std::str::from_utf8(...).parse::<u64>().ok()?` to a wrapping-loop
  parse over the already-validated digit slice. New
  `split_top_level_args_into(args, &mut buf)` for reusing the field
  buffer in the indexer's per-record loop.

Bench (warm cache, best of 5-6):

| file | before | after | speedup |
|---|---:|---:|---:|
| sample-A 22 MB / 8.8K products | 39 ms | 29 ms | 1.34× |
| sample-B 187 MB / 21K products | 195 ms | 152 ms | 1.28× |
| sample-D 834 MB / 87K products / 14.3M records | 1287 ms | 905 ms | 1.42× |

Parity preserved: sample-A drift counts reproduce 8042 ok / 347 warn
/ 264 error exactly; pset / classification / material counts identical.

### Spatial-relationship graph (Python)

- `python/ifcfast/model.py` — `Model` dataclass gained three new
  DataFrame fields (`_contained_in_df`, `_aggregates_df`,
  `_storey_building_df`) plus a lazy `_graph` field. Properties
  expose them as `m.contained_in` / `m.aggregates` /
  `m.storey_building`. New `_GraphIndex` helper builds six inverse
  maps (`parent_of`, `children_of`, `storey_of`, `products_in`,
  `building_of`, `storeys_in`) in a single pass and caches them.
  Seven traversal methods on `Model`: `parent`, `children`,
  `ancestors`, `descendants`, `storey_of`, `building_of`,
  `products_in`. All unify spatial containment with aggregates so
  `parent(wall)` falls back to its storey, and `ancestors(wall)`
  reaches the project (4 hops on sample-A).
- `python/ifcfast/cache.py` — bumped `CACHE_VERSION` to 2. Three new
  parquet files written in `write_index` and restored in
  `read_index`. Manifest now records relationship counts. Disk
  overhead: 231 KB total for the sample-A cache (5 parquets +
  manifest). Hot-reload speed: 332 ms cold → 19 ms hot (17.9×).
- `tests/test_graph.py` — 9 new tests covering column shape, building-
  to-storey, storey-to-products cardinality, ancestor chain reaches
  project, descendants cover spatially-contained products, storey_of
  agreement with `contained_in`, `building_of` walking via storey,
  missing-guid safety, and cache round-trip parity.
- `README.md` — new "Spatial hierarchy & relationships" section
  between Drift and Federated floor synthesis.
- `CHANGELOG.md` — Added/Changed bullets under `[Unreleased]`.
- `python/ifcfast/__init__.py` — docstring updated with the new
  surface.

## Technical Details

The perf diagnosis went one level deeper than expected. The v3
worklog's hypothesis (string alloc in `extract_product`, type_counts
clone, post-pass storey filter) was correct but only 1-2% of the
total cost. The real bottleneck appeared once I instrumented the
record counter: sample-D has **14.3 million STEP records** of which
99.4% are types we discard (IfcCartesianPoint, IfcPolyLoop,
IfcPropertySingleValue, etc.). The original dispatch chain was
running two HashSet lookups + ~8 byte-slice equality checks on every
single discarded record — that's where the time went, not in the 87K
products we actually process. Single-lookup dispatch killed it. Then
the std::parse u64 (with UTF-8 validation + per-digit checked
overflow) was running 14M times on data we knew was clean ASCII —
hand-rolled wrapping loop took another ~15% off.

The graph layer's design choice that mattered most was unifying
`IfcRelAggregates` (decomposition) and `IfcRelContainedInSpatialStructure`
(spatial containment) under a single `parent` / `children` API. In raw
IFC, walls have NO aggregate parent — only a spatial container — so a
naive aggregates-only traversal returns `[]` for `ancestors(wall)`.
The fix: `parent(g)` returns the aggregate parent if it exists, else
the storey via `storey_of`. From there `ancestors` walks the unified
chain and reaches storey → building → site → project.

Known gap surfaced during testing: `IfcOpeningElement` products (492
of them on sample-A) are NOT in either relationship — they connect
via `IfcRelVoidsElement`, which the Rust indexer doesn't yet extract.
So `m.products_in(project_guid)` returns 8381 vs total 8873.
Documented in the README's coverage paragraph; flagged as the obvious
next-tier indexer addition.

## Next

The whole session's work is uncommitted in working tree (`git
status`: 7 modified + 2 untracked). Probably wants two commits — one
for the indexer perf (cleanly separable, has its own worklog at
`2026-05-15-15-00_Tier1-indexer-dispatch-perf.md`), one for the graph
layer. Push to `EdvardGK/ifcfast` afterward.

Concrete next development moves, in priority order:

1. **`IfcRelVoidsElement` in the Rust indexer.** Cheap to add (~30
   lines following the `CONTAINED_TYPE` / `AGGREGATES_TYPE` pattern),
   would close the 492-orphan gap and let users ask "what openings
   are in this wall?" through the same graph API.
2. **`IfcRelConnectsElements` and friends.** Beam-to-column, pipe-to-
   pipe etc. Same pattern as above. Unlocks connectivity graphs for
   structural / MEP analysis.
3. **Lazy id parsing in `parse_record`.** Skip the u64 parse on
   records the dispatch rejects — saves another ~140 ms on sample-D.
   Requires reshaping `Record` (EntityTable wants id eagerly), so
   either split the API or compromise. Earlier perf worklog flags it.
4. **Boolean subtraction (net volumes).** Still the biggest user-
   facing geometry gap. The plan-doc explored options: ~300 LOC for
   the rectangular-opening custom case, larger for a CSG library.
5. **Three.js / Fragments bridge.** Per-element streaming glTF + the
   Fragments extension JSON. Larger refactor of `mesh_ifc()`.

## Notes

- The plan file used for this session lives at
  `~/.claude/plans/ok-lets-plan-next-noble-goose.md` — kept verbatim
  during execution; no scope creep.
- Re-running `maturin develop --release` is required after the Rust
  perf changes for the Python tests to pick them up. Tests pass on
  the rebuilt wheel (38/38).
- The 9 graph tests run against the bundled `tests/fixtures/minimal.ifc`
  plus the sample-A IFC under `~/workspace/inbox/ifc-workbench/data/
  samples/_big/`. The cache round-trip test creates a temp cache via
  `IFCFAST_CACHE`.
- Cache v1 → v2 forces all existing user caches to invalidate on
  first open after merge. Fine — the parse cost is bounded and the
  rewrite is incremental.
