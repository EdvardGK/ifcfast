# Changelog

All notable changes to ifcfast will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

[0.1.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.1.0
