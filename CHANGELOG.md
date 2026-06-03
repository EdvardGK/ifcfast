# Changelog

All notable changes to ifcfast will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.31] - 2026-06-03

### Fixed — `__version__` single source of truth (GH #46)

- **`ifcfast.__version__` now reads from the installed package
  metadata** via `importlib.metadata`, not a hand-bumped string in
  `__init__.py`. The version string lived in four files
  (`pyproject.toml`, `Cargo.toml`, `crates/core/Cargo.toml`,
  `python/ifcfast/__init__.py`) and silently drifted out of sync
  across releases — every release required four manual edits, one
  of which got forgotten or stale. Two of those four now collapse:
  `crates/core/Cargo.toml` inherits `version` / `edition` /
  `license` / `repository` from `[workspace.package]`, and
  `__init__.py` reads from `importlib.metadata`. A release now
  needs two coordinated bumps: `pyproject.toml` (wheel side) and
  `[workspace.package].version` (Rust side). Pinned by
  `tests/test_smoke.py::test_version_matches_installed_metadata`.

### Fixed — `world_coordinate_baked` detector rewrite (GH #33)

- **`m.world_coordinate_baked` is now symptom-based, not
  cause-based.** The v0.4.27 detector required ≥80% of meshed
  products to have placement within 1 mm of world origin — a
  Tekla-specific guess at the underlying authoring style. It
  missed every other baked-coords variant: building-origin-anchored
  placements with geometry authored further out, prefab-heavy
  structural files, and mixed-baked exports like G55_RIB
  (Ed's tester-in-chief re-verify on 2026-06-02 showed 382/896
  `error` rows still flagged after v0.4.27 shipped). The new
  detector trips when ≥25% of meshed products would carry
  `drift_severity == "error"` under the per-row rule (file must
  have ≥20 meshed products to qualify). When tripped, every
  `error` and `warn` row demotes to `info`. Raw `drift_distance_m`
  and `drift_ratio` columns are unchanged — the demotion is
  cosmetic on the severity column, the underlying signal is
  intact for analysts who want it.
- **`ifcfast drift` banner rewritten to match the new semantic.**
  No longer claims "origin-placed products demoted"; explains the
  model-level pattern, names the common authoring styles that
  produce it, and points at the raw drift columns as the un-
  demoted signal.
- **`ifcfast drift --top` widens past `error`.** In a baked-
  pattern model all interesting rows are `info`; the top-N now
  ranks any non-`ok` row by `drift_distance_m` so the worst
  cases stay visible.

### Schema

- `_CACHE_SCHEMA_VERSION` 11 → 12. Old caches re-extract on next
  open; severity counts shift on baked-pattern models, raw drift
  columns are byte-identical.

## [0.4.30] - 2026-06-02

### Fixed — IfcArcIndex tessellation in IfcIndexedPolyCurve (GH #48)

- **`IfcArcIndex` segments inside `IfcIndexedPolyCurve` are now
  curve-sampled, not chorded.** Old behavior treated every
  3-index arc tuple as a straight chord between the first and
  third indexed points, collapsing Revit MEP pipes / ducts
  authored via `IfcArbitraryProfileDef(WithVoids)` to square
  prisms. Cross-validation on G55_RIV vs ifcopenshell:
  `IfcPipeSegment` volume-ratio 0.21 → 1.003 (-79% error → <1%),
  `IfcDuctSegment` 0.997. New shared module
  `mesh::indexed_curve` exposes 2D and 3D arc evaluators
  (32-segment-per-full-circle budget, matching
  `IfcCircleProfileDef`), wired into `profile.rs`,
  `curveset.rs`, and `boolean.rs`. CCW orientation forced on
  output — Revit MEP authors CW and the earcut + extrusion
  pipeline silently inverts cap triangulation otherwise (volume
  drops to 1/3 with correct arc geometry).

### Schema

- `_CACHE_SCHEMA_VERSION` 10 → 11. Old caches re-extract on next
  open on any RIV / HVAC / MEP model.

## [0.4.29] - 2026-06-02

### Fixed — pset / quantity extractor batch (GH #36, #38, #43, #45)

- **`m.psets` now inherits type-level properties (GH #36).** Properties
  carried on `IfcTypeObject.HasPropertySets` and bound via
  `IfcRelDefinesByType` surface on every related instance, tagged
  `source = "type"`. Instance-declared properties carry
  `source = "instance"` and shadow same-named type properties on
  collision (matches `ifcopenshell.util.element.get_psets(..., should_inherit=True)`).
  Real-world payoff verified on G55_RIB.ifc: 17 `IfcBuildingElementProxy`
  instances now show `Pset_ManufacturerTypeInformation.Manufacturer = 'Wurth'`
  that were entirely absent pre-fix. Same GUIDs and manufacturer
  values ifcopenshell returns. `m.psets` gains the `source` column.
- **`m.quantities.unit_step_id` falls back to the project's
  `IfcUnitAssignment` (GH #43).** When `IfcQuantity*.Unit` is null —
  the common Revit / ArchiCAD authoring pattern — the column resolves
  to the project's `IfcSIUnit` for the quantity's kind
  (`Length`→`LENGTHUNIT`, `Area`→`AREAUNIT`, `Volume`→`VOLUMEUNIT`,
  `Weight`→`MASSUNIT`, `Time`→`TIMEUNIT`; `Count` stays null —
  dimensionless). Explicit per-quantity `Unit` refs still win.
  `IfcConversionBasedUnit` and `IfcDerivedUnit` resolution stay out
  of scope (separate feature). Verified on A4_RIB_B.ifc (16,742 qty
  rows): pre-fix every `unit_step_id` was null; post-fix all four
  resolved kinds match the IfcSIUnit step_ids ifcopenshell returns.
- **`IfcPropertyTableValue` now surfaces as a row and unhandled
  property classes emit a marker (GH #38).** `IfcPropertyTableValue`
  parses to a single row with `value = "d1=>v1, d2=>v2, ..."`
  pairing DefiningValues + DefinedValues; `value_type` takes the
  DefinedValues axis type. Any `IfcSimpleProperty` subclass without
  a per-class parser (`IfcPropertyReferenceValue`, future `*Value`
  classes, authoring-tool customs) emits a marker row with
  `value = None` and `value_type = "unhandled:IFCXXX"`. Enumerate
  blind spots:
  ```python
  m.psets[m.psets.value_type.fillna("").str.startswith("unhandled:")]
  ```
- **`m.quantities` inherits type-attached `IfcElementQuantity`
  (GH #45).** Mirrors the GH #36 pset path: types can carry
  quantities the same way they carry psets because
  `IfcTypeObject.HasPropertySets` accepts any
  `IfcPropertySetDefinition` and `IfcElementQuantity` IS-A
  `IfcPropertySetDefinition`. Inherited rows carry `source = "type"`,
  deduped against instance ones on `(qto_name, quantity_name)`.
  GH #43 unit fallback composes through — inherited rows still get
  project-default `unit_step_id` resolution. Type-attached quantities
  are rare in practice (0 hits across 81 real files in the sweep)
  but the fix is principled and free at that cardinality.

### Schema

- `_CACHE_SCHEMA_VERSION` 6 → 10. Four bumps stacked across this
  release (one per behavior change). Old caches re-extract on next
  open.
- Substrate `instances.parquet` nested `psets` struct gains
  `source : string (not null)`.
- Substrate `instances.parquet` nested `quantities` struct gains
  `source : string (not null)`.
- `m.psets` / `m.quantities` DataFrames gain a `source` column
  with the same two values.

## [_pre-0.4.29 unreleased]

### Fixed — tester-in-chief cross-validation batch (GH #29–#35)

- **`m.products` is no longer silently empty on cache hits (GH #29).**
  The attribute used to be a list backing the cold-parse path and was
  left empty when the model came back from a parquet cache, so the
  README's `for p in m.products:` quickstart yielded nothing. Now a
  property: returns the materialised list either way, building it
  lazily from `m.products_df` on cache-hit Models. `len(m)` and
  `len(m.products)` agree. `iter(m)` also added — streams
  `ProductRow` without materialising the list.
- **CLI `ifcfast demo` no longer crashes on Windows cp1252 consoles
  (GH #30).** `_force_utf8_stdio()` at the CLI entry reconfigures
  stdout/stderr to UTF-8 so the em-dash and `→` glyphs in the pretty
  output don't trip `UnicodeEncodeError`. `--json` paths were
  already ASCII-safe; this only mattered for pretty mode and the
  argparse `--help` banner.
- **`m.drift` columns are SI-suffixed and the values match
  `m.mesh_qto()` (GH #31).** The Rust drift extract now applies the
  file's `unit_scale` before emitting, so columns are
  `drift_distance_m`, `max_extent_m`, `surface_area_m2`,
  `volume_abs_m3`, `aabb_volume_m3`, `placement_{x,y,z}_m`,
  `centroid_{x,y,z}_m`. Joining `m.drift` against `m.mesh_qto()` no
  longer needs an out-of-band rescale (used to be off by 10⁶–10⁹
  on mm-unit files with no signal in the column names). Cache schema
  v4 — old drift caches are rebuilt on next access.
- **`m.contained_in` captures every spatial-container kind, not
  just storey (GH #32).** The indexer no longer filters
  `IfcRelContainedInSpatialStructure` to storey edges. The
  DataFrame schema is now `(product_guid, container_guid,
  container_kind)` with `container_kind ∈ {site, building, storey,
  space}`. `m.parent(guid)` falls back to whichever spatial
  container the element sits in; `m.storey_of(guid)` walks through
  non-storey containers (e.g. an element contained in an
  `IfcSpace` resolves to the space's storey); `m.building_of(guid)`
  honours direct building containment in addition to
  storey-then-building. Cache schema v5. Adds
  `tests/fixtures/site_annotation.ifc` covering site- and
  building-level containment.
- **`drift_severity` no longer carpet-bombs world-coordinate-baked
  models (GH #33).** Per-row severity recomputed against SI values
  with a unit-independent 10 mm absolute threshold (the old `drift
  < 10.0` test was 10 mm on mm-files but 10 m on metre-files —
  over-strict in one direction, over-lenient in the other). New
  file-level detector: when ≥ 80 % of meshed products are placed
  at the origin within 1 mm, the file is flagged
  `m.world_coordinate_baked == True` and the per-row severity of
  origin-placed products is demoted to `info` so the file-level
  fact is the actionable signal instead of N "errors". Adds the
  `info` severity bucket; CLI `ifcfast drift` surfaces the flag.
- **README/AGENTS/system_prompt mismatch fixes (GH #34).** Softened
  the "NaN-not-None" invariant claim (materials/classifications
  carry Python `None` in object-dtype columns; `pd.isna()` catches
  both); documented `mesh_qto()` returning a `(products_df,
  surfaces_df)` tuple in both `system_prompt()` and the README;
  added an `iter(m)` row to the AGENTS.md decision tree; fleshed
  out the `cache.py` module docstring (missing `segments.parquet`,
  `voids.parquet`, `spaces.parquet`, `type_objects.parquet`).
  `Model.__iter__` ships as part of the GH #29 fix.
- **Quantity extractor now covered end-to-end against
  `ifcopenshell` ground truth (GH #35).** Real-world test set the
  tester used happened to lack `IfcElementQuantity` across all six
  files. Adds `tests/fixtures/quantities.ifc` exercising every
  `IfcQuantity*` subtype (Length, Area, Volume, Count, Weight,
  Time) via `IfcRelDefinesByProperties` and a cross-check that
  diffs `m.quantities` against `ifcopenshell.util.element.get_psets(
  el, qtos_only=True)`.

### Added

- **`m.meshes(cut_openings=True)` — net booleans on demand.** Opt-in
  CSG path that folds every `boolean_second_operand|...` mesh
  segment (the door / window void emitted alongside the host wall
  by the reveal-all pipeline) into the host via `manifold-csg`.
  Doors and windows render as actual holes instead of solid
  volumes-on-volumes. Default `cut_openings=False` preserves the
  reveal-all stance (both operands visible). The substrate stays
  reveal-all unconditionally — the flag only affects `m.meshes()` /
  `m.iter_meshes()` callers. Closes the viewer-integrator's P0 #1
  ask (GH #20). Requires a wheel built with the new `csg` Cargo
  feature; raises `RuntimeError` if the underlying wheel was
  compiled without it. Cross-product `IfcRelVoidsElement` openings
  (host wall + separately-modelled `IfcOpeningElement`, no boolean
  in the wall's own representation) are NOT cut by this path yet —
  a follow-on.
- New `csg` Cargo feature pulling in `manifold-csg = "0.2"` (Apache-2
  / MIT, f64 precision, Send-safe, cmake-built C++ core). Off in the
  default Python wheel until cross-platform wheel-build smoke
  testing lands. Build locally with
  `maturin develop --features csg`.

- **`ifcfast.clash()` — substrate-aware narrow-phase clash engine.**
  Reads `instances.parquet` + `representations.parquet` from a
  bundle directory, runs broad-phase AABB overlap (via the
  `geom::pairs_overlapping` kernel landed earlier in v0.4.19), then
  narrow-phases each candidate pair as a true mesh-mesh
  intersection / distance query against `parry3d` BVH-built
  TriMeshes. Writes `clashes.parquet` next to the inputs.
  ```python
  import ifcfast
  df = ifcfast.clash("model.bundle/", tolerance_m=0.05)
  ```
  Output columns: `ifc_id_a/b`, `guid_a/b`, `class_a/b`, `kind`
  (`"hard"` or `"clearance"`), `min_distance_m`. `tolerance_m` and
  `min_distance_m` are always in metres regardless of the source
  IFC's linear unit. The bundle now records the project's unit
  scale as parquet schema metadata (`ifcfast.unit_scale`) and the
  clash engine converts at load time. The engine is the *fact*
  layer ("do they touch, by how much, how far apart"); policy
  (connectivity dismissal, BCF emit, discipline routing) lives in
  the layer above and queries `clashes.parquet` joined to
  `instances.parquet`. See the "Narrow-phase clash" section in
  [`AGENTS.md`](AGENTS.md) for the worked DuckDB example.
- New `ifcfast-clash` binary mirroring the Python entry:
  `ifcfast-clash BUNDLE_DIR [--tolerance N] [--out file.parquet]`.
- `_core.clash(bundle_dir, tolerance_m, write_parquet, ...)` PyO3
  binding returning the same column-dict shape as the other
  extractors.
- `clash` Cargo feature stacking `bundle` + `geom`. The default
  Python wheel build now ships `bundle`, `geom`, and `clash` —
  previously it shipped only `python`, so the substrate writer
  and geom kernel only worked from a `cargo build --features
  bundle,geom` build. Agents shouldn't need to know about extras
  for first-class promises.
- **Geometric fingerprint columns on `instances.parquet`** — phase 1a
  of the federated-model clash-control / cross-discipline duplicate
  detection feature. Three new non-nullable columns on every instance
  row:
    * `centroid_xyz` — `FixedSizeList[Float32, 3]`, world-AABB
      midpoint when the product has mesh geometry, falling back to
      `placement_xyz` for geometryless products so location queries
      don't collapse no-body elements onto the world origin.
    * `vertex_count` — `UInt32`, world-baked mesh vertex count
      (zero when geometryless).
    * `triangle_count` — `UInt32`, world-baked mesh triangle count
      (zero when geometryless).
  Lets agents compose cross-model duplicate detection, version-diff,
  and broad-phase clash candidate filtering as pure DuckDB queries
  against the substrate (centroid distance + bbox overlap +
  complexity match), without re-running the parser or recomputing
  midpoints on every join. See the "Substrate output" section in
  [`AGENTS.md`](AGENTS.md) for the worked example.

### Changed

- `instances.parquet` and `representations.parquet` now carry
  `ifcfast.unit_scale` and `ifcfast.version` as parquet schema
  metadata. Backwards-compatible — readers that ignore the metadata
  see no change. The clash engine uses `unit_scale` to convert
  source-unit vertex / bbox data to metres at load time.
- `_CACHE_SCHEMA_VERSION` bumped 4 → 5. Existing caches become
  orphaned automatically — re-extract on next open is automatic.

## [0.4.0] - 2026-05-21

### Added

- **`Model.mesh_qto()`** — the geometric QTO engine is now reachable
  from the PyPI wheel, no cargo build required. Returns a tuple
  `(products_df, surfaces_df)`:
    * **products_df** — one row per meshed product. Columns:
      `guid, entity, volume_m3, aabb_volume_m3, surface_area_m2,
      area_top_m2, area_bottom_m2, area_side_m2, area_inclined_m2,
      largest_surface_m2, smallest_surface_m2, surface_count`.
    * **surfaces_df** — long-format, one row per distinct planar
      surface per product (sorted by area within a product).
      Columns: `guid, surface_index, area_m2, nx, ny, nz`. Normal-
      bucket aggregation at ~5.7° granularity collapses coplanar
      triangles into one surface; curved geometry resolves to one
      tessellation-wedge per row.
  All values are in m² / m³ regardless of source unit. The
  computation runs `mesh_ifc_streaming` once and emits both
  DataFrames in a single pass — no second walk, no intermediate
  Parquet round-trip. Authored `IfcElementQuantity` values stay in
  `m.quantities` and remain the gold-standard override when present.
- PyO3 binding `_core.mesh_qto(path)` returns the raw column dict
  for callers who want to skip the pandas wrapper.

### Changed

- The PyPI wheel now exposes the geometric QTO engine alongside the
  existing `analyse_drift` mesh path. This closes the gap from 0.3.0
  where the engine code shipped in the wheel but wasn't reachable
  from Python (only via the opt-in `ifcfast-bundle` binary).

## [0.3.0] - 2026-05-21

### Added

- **Per-product geometric QTO engine** in the bundle. One
  O(triangles) pass over the world-coord mesh during the streaming
  pass emits volume + surface area decomposed by face orientation +
  the full list of distinct planar surfaces per product. New columns
  on `instances.parquet`:
    * `volume_m3`, `aabb_volume_m3`
    * `surface_area_m2` total
    * `area_top_m2` / `area_bottom_m2` (triangles within 20° of ±Z)
    * `area_side_m2` (within 20° of horizontal plane)
    * `area_inclined_m2` (everything else)
    * `largest_surface_m2`, `smallest_surface_m2`, `surface_count`
    * `surfaces`: `List<Struct<area_m2, nx, ny, nz>>` — every
      distinct planar surface, normal-bucket aggregated at ~5.7°
      granularity, sorted by area descending. DuckDB UNNEST gives
      one row per face for "every surface on type X" queries.
  All values in m² / m³ regardless of source unit (mm → m via
  `unit_scale`). Authored `IfcElementQuantity` values stay in
  `quantities` as the gold-standard override; these geometric values
  are the truth that survives when authors omit `Qto_*` sets.
- Bundle output grew ~30 MB on the 27M-triangle ST28_RIV (834 MB
  IFC, 85,976 instances) for the per-surface list; compute pass +12%
  over the prior bundle pass; query latency against the materialized
  parquet is sub-15 ms for typical group-by-entity QTO queries on
  86K-row substrates.

### Changed

- Bundle `instances.parquet` schema gains 11 non-nullable columns
  (the QTO columns above). Strict-schema consumers expecting the
  v0.2.0 shape will need to update; permissive readers (DuckDB,
  pyarrow with column-projection) are unaffected.

## [0.2.0] - 2026-05-21

### Added

- **Streaming GeoParquet substrate writer (`ifcfast-bundle`).** New
  cargo feature `bundle` + binary `ifcfast-bundle <file.ifc> [out_dir]`
  emits a two-table substrate (`representations.parquet` +
  `instances.parquet`) in one streaming pass. Pairs geometry with full
  IFC semantics (psets, materials, quantities, classifications,
  storey, type) so the downstream analyser can join geometry to
  metadata without re-parsing the IFC. Working-set RAM bounded by the
  Parquet row-group buffer; the old `Vec<ProductMesh>` accumulator's
  OOM class is gone. DuckDB queries via the emitted `view.sql` join.
  Cargo feature is opt-in (default off); the Python wheel does not
  bundle the heavy arrow + parquet crates.
- **Hierarchical / instanced substrate layout.** The substrate now
  splits into `representations.parquet` (one row per unique mesh
  shape, keyed by `rep_id`) and `instances.parquet` (one row per
  `IfcProduct`, geometry-free except for a `rep_id` foreign key and a
  4×4 world transform). Cross-product dedup on `IfcMappedItem` /
  `IfcRepresentationMap` collapses N instances of the same window-
  facade family to ONE rep row — ST28_RIV (834 MB, 87K products)
  output dropped from 180 MB single-file to 68.6 MB across the two
  files (−62%).
- **Bundle pre-pass: `Arc<str>` interning + zero-clone regrouping.**
  Pset / material / quantity / classification regrouping now interns
  repeated semantic strings (set_name, prop_name, source_class,
  storey_name, type_name, …) and consumes the extractor's
  `Vec<String>` columns by-move rather than by-clone. On ST28_RIV
  (2.57M pset rows): peak RSS 2709 → 2627 MB (−3.0%), wall 33.06 →
  30.28 s (−8.4%), output bit-identical.
- **MCP server (`ifcfast-mcp`).** Standalone Model Context Protocol
  server exposing 18 tools (open_ifc / summary / schemas / preview /
  types / by_type / parent / children / ancestors / descendants /
  storey_of / building_of / products_in / diff / list_open / close /
  system_prompt / example_path) plus an `ifcfast://agents-guide`
  resource. Plug into Claude Desktop, Cursor, or any MCP-aware
  client by adding `{"command": "ifcfast-mcp"}` to the client's
  MCP server config. Install with `pip install 'ifcfast[mcp]'`.
- **`Model.diff(other)`** — first-class model-version comparison.
  Returns JSON-friendly dict with products added/removed/changed
  (and exact counts), type cardinality deltas, and storey changes.
  Makes "what changed since v3?" a one-liner.
- **`Model.type_summary()` and `Model.type_bank()`** — type-first
  extraction shaped for TypeBank-style workflows. Cheap (no extracts
  for `type_summary`; lazily pulls materials + classifications for
  `type_bank`).
- **`Model.by_type(entity)`** — ifcopenshell-compat shortcut. Same
  shape as `ifcopenshell.file.by_type(entity)`.
- **`ifcfast types FILE`** CLI subcommand — JSON-friendly type
  extraction with optional `--with-data` for the full TypeBank shape.
- **Agent-first surface.** New top-level helpers
  `ifcfast.example_path()` (path to a bundled 2 KB IFC4 fixture) and
  `ifcfast.system_prompt()` (paste-into-LLM description of the API).
  `Model.summary()` returns a JSON-friendly snapshot — schema, counts,
  every available table with shape + loaded-state. `Model.schemas`
  exposes column-level dtypes. `Model.preview(table, n=5)` returns
  sample rows as plain list-of-dicts.
- CLI: every subcommand now takes `--json` and emits a stable
  JSON shape. New subcommands: `ifcfast demo` (runs the showcase
  against the bundled fixture) and `ifcfast schema FILE` (full
  schema introspection without paying any extract cost).
- `py.typed` marker — type checkers (pyright, mypy, IDE LSPs) now
  pick up annotations from the package.
- `AGENTS.md` at the repo root: agent-onboarding guide, decision
  tree, performance budget table, and the conventions an agent can
  rely on.
- Spatial hierarchy & relationship graph on the `Model`. Three new
  long-format DataFrame properties — `m.contained_in`, `m.aggregates`,
  `m.storey_building` — plus seven traversal helpers (`parent`,
  `children`, `ancestors`, `descendants`, `storey_of`, `building_of`,
  `products_in`). The helpers walk the unified aggregates +
  spatial-containment graph so a single `ancestors(wall_guid)` reaches
  the project, and `products_in(building_guid)` returns every product
  in every storey of that building.
- Tier-1 cache bumped to v2: relationship tables persist as
  `contained_in.parquet`, `aggregates.parquet`, and
  `storey_building.parquet` alongside the existing index parquets. Old
  v1 caches re-parse on first open. Disk overhead: <500 KB on a 200 MB
  IFC.

### Changed

- Tier-1 indexer is 22-30% faster end-to-end. Hot-path dispatch now
  uses a single HashMap lookup keyed by STEP type name (was a chain of
  two HashSet lookups + ~8 byte-slice equality checks per record).
  Step-id parsing skips std's UTF-8 + checked-overflow path in favour
  of a tight wrapping loop. The argument splitter reuses a buffer
  across records instead of allocating one `Vec` per STEP entity.
- Entity name canonicalisation (`IFCWALL` → `IfcWall`) is now O(1)
  via a lazy `OnceLock<HashMap>` instead of a 130-entry linear scan.
- `IfcRelContainedInSpatialStructure` post-pass filter is now
  in-place; previously allocated two fresh `Vec`s sized to the
  unfiltered input.

Measured against the published audit set (results, throughput on a
warm cache):

| file shape | before | after | speedup |
|---|---:|---:|---:|
| Small ARK (22 MB, 8.8K products) | 39 ms | 29 ms | 1.34× |
| Federated mid-size (187 MB, 21K products) | 195 ms | 152 ms | 1.28× |
| Large MEP (834 MB, 87K products, 14.3M records) | 1287 ms | 905 ms | 1.42× |

Byte-level parity vs `ifcopenshell` preserved across the audit set
(drift severity histograms reproduce exactly on every file).

## [0.1.0] - 2026-05-14

Initial PyPI release. Library was extracted on 2026-05-13 from the
`EdvardGK/ifc-workbench` scratch repo; see [`docs/history/origin.md`](docs/history/origin.md)
for the trail and rename table.

### Added

- `ifcfast.open(path)` — tier-1 parse with lazy data layers (`psets`,
  `quantities`, `materials`, `classifications`, `drift`).
- `ifcfast.header(path)` — tier-0 STEP header read in 30-80 ms regardless
  of file size.
- Parquet cache (`~/.cache/ifcfast/<cache_key>/`) — second open returns
  in tens of milliseconds. Override via `IFCFAST_CACHE` env var.
- `ifcfast.classify` — element-mode policy (count / measure / linear / skip).
- `ifcfast.federated_floors` — multi-discipline floor synthesis with
  project-supplied YAML rules.
- CLI: `ifcfast {index,extract,drift,cache} FILE`.
- Rust binary `ifcfast-mesh` — writes OBJ / glTF / CSV from extrusion,
  mapped, face-set, and BREP representations.
- Pre-built abi3 wheels for Linux (x86_64/aarch64), macOS (x86_64/arm64),
  and Windows (x64) on Python 3.10+.

### Validated

- Byte-level parity vs `ifcopenshell` across 234,144 products from 5
  authoring tools (Tekla, Archicad, Revit IFC4/IFC2X3, MagiCAD, BSProLib).
  See [`docs/history/audit/`](docs/history/audit/).
- 100% parity confirmed on the standalone repo against 4 production
  IFCs from Skiplum projects (issue #1).
- Warm-cache speedup vs `ifcopenshell.open()`: 59-678× on production files.

[0.4.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.4.0
[0.3.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.3.0
[0.2.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.2.0
[0.1.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.1.0
