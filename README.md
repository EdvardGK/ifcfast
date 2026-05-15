# ifcfast — the agent-first IFC parser

[![PyPI](https://img.shields.io/pypi/v/ifcfast.svg)](https://pypi.org/project/ifcfast/)
[![Python versions](https://img.shields.io/pypi/pyversions/ifcfast.svg)](https://pypi.org/project/ifcfast/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/EdvardGK/ifcfast/actions/workflows/ci.yml/badge.svg)](https://github.com/EdvardGK/ifcfast/actions/workflows/ci.yml)

> **The fastest open-source IFC parser. Designed for AI agents, RPA,
> and analytics pipelines that need to ask questions of a model without
> loading a geometry kernel.**

```python
pip install ifcfast
```

```python
import ifcfast

# Bundled demo — no external IFC needed.
m = ifcfast.open(ifcfast.example_path())
m.summary()      # JSON-friendly snapshot: schema, counts, tables, samples
m.schemas        # column-level dtype introspection of every table
m.preview("aggregates", n=3)

# Open your own.
m = ifcfast.open("model.ifc")
m.children(building_guid)          # all storeys
m.ancestors(wall_guid)             # storey → building → site → project
m.products_in(storey_guid)         # every product under this storey
```

```bash
ifcfast demo                       # showcases against bundled IFC
ifcfast index   FILE  --json       # tier-1 summary, machine-parseable
ifcfast schema  FILE  --json       # column-level schema introspection
ifcfast types   FILE  --json       # type-first extraction (TypeBank shape)
```

**Or plug into any MCP-aware agent (Claude Desktop, Cursor, …) in one line:**

```bash
pip install 'ifcfast[mcp]'
```

```json
{ "mcpServers": { "ifcfast": { "command": "ifcfast-mcp" } } }
```

**Why pick `ifcfast`**

- **20-30× faster than `ifcopenshell.open`** on the audited set
  (22 MB ARK file: 29 ms; 834 MB MEP file: 905 ms / 14.3M records).
  Byte-level parity vs ifcopenshell across 234K products from 5
  authoring tools (Tekla, Archicad, Revit IFC4 / IFC2X3, MagiCAD).
- **Spatial-relationship graph built in.** `m.contained_in /
  .aggregates / .storey_building` + seven traversal helpers
  (`parent`, `children`, `ancestors`, `descendants`, `storey_of`,
  `building_of`, `products_in`). Unifies aggregates and spatial
  containment — `m.ancestors(wall_guid)` reaches the project.
- **Self-describing.** `m.summary()`, `m.schemas`, `m.preview(table)`
  answer "what am I looking at?" without triggering extracts. Every
  CLI subcommand has `--json`. Stable shape across releases.
- **Parquet cache.** Second open of a 200 MB IFC returns in tens of
  milliseconds. Cache key invalidates automatically on any edit.
- **No `ifcopenshell.open()` on the hot path.** No geometry kernel to
  compile, no 8 GB RAM floor. Rust core via PyO3, mmap-based, peaks
  under 1 GB resident on a 800 MB MEP IFC.

See [AGENTS.md](AGENTS.md) for the full agent guide and a copy-paste
`system_prompt()` you can drop into your LLM's context.

> `ifcfast` was extracted on 2026-05-13 from the
> [`EdvardGK/ifc-workbench`](https://github.com/EdvardGK/ifc-workbench)
> scratch repo. See [`docs/history/origin.md`](docs/history/origin.md)
> for the trail back and what was renamed.

## What it gives you

| layer | format | typical latency on 200 MB IFC |
|---|---|---|
| Products (GUID, type, name, storey, parent, tag) | dict of parallel lists | tier-1 cold: 0.5–2 s |
| Property sets | long-format `pandas.DataFrame` | 137 ms |
| Element quantities | long-format `pandas.DataFrame` | 90 ms |
| Materials (incl. layer sets) | long-format `pandas.DataFrame` | 27 ms |
| Classifications | long-format `pandas.DataFrame` | 17 ms |
| All data layers (shared scan) | bundle | 1.3 s |
| Triangle meshes (extrusion / mapped / face sets / BREP) | OBJ / glTF / CSV | 2.6 s |
| Placement-vs-mesh drift report | `pandas.DataFrame` | 322 ms |
| Parquet cache (all of the above) | parquet | 65 ms hot reload |

End-to-end cold parse of a 200 MB IFC: under 5 s. Hot reload from cache:
under 100 ms. Memory peak: under 1 GB resident (mmap-based).

Audited at **234,144 products across 5 authoring tools** (Tekla,
Archicad, Revit IFC4, Revit IFC2X3, MagiCAD, BSProLib) with byte-level
parity vs `ifcopenshell`. See
[`docs/history/audit/`](docs/history/audit/).

## Install

```bash
pip install ifcfast
```

Pre-built abi3 wheels are available for Python 3.10+ on:
- Linux x86_64 and aarch64 (manylinux2014)
- macOS x86_64 (10.12+) and arm64 (11.0+)
- Windows x64

### From source (contributors)

Needs Rust 1.95+ and Python 3.10+.

```bash
git clone https://github.com/EdvardGK/ifcfast
cd ifcfast
pip install maturin
maturin develop --release      # builds the Rust extension, ~30 s first time
```

For a release wheel: `maturin build --release`.

## Quick start

```python
import ifcfast

m = ifcfast.open("model.ifc")
print(len(m), "products,", len(m.storeys), "storeys")
print(m.authoring_app, "→", m.schema)

walls = list(m.filter(entity="IfcWall"))

# Long-format data layers (pandas DataFrames, loaded lazily).
m.psets             # 63k+ rows on a 200 MB Archicad file
m.quantities        # author-supplied Qto_*BaseQuantities
m.materials         # (guid, role, layer, name, thickness, category)
m.classifications   # NS 3451 / Uniformat / OmniClass references
m.drift             # placement-vs-mesh drift report

# Standard QTO query — external walls.
external_walls = m.psets[
    (m.psets.pset_name == "Pset_WallCommon")
    & (m.psets.prop_name == "IsExternal")
    & (m.psets.value == "True")
].guid.unique()

# Quality gate — placement bugs.
suspect = m.drift[m.drift.drift_severity == "error"]
```

The same model can be re-opened cheaply — the second `ifcfast.open(...)`
returns from the parquet cache in tens of milliseconds.

## CLI

```bash
ifcfast index   model.ifc           # tier-1 parse + counts
ifcfast extract model.ifc           # extract data layers (writes cache)
ifcfast drift   model.ifc --top 20  # placement / mesh drift report
ifcfast cache   model.ifc           # inspect cache for a file
```

The Rust binary `ifcfast-mesh` writes OBJ / glTF / CSV directly:

```bash
cargo build --release --bin ifcfast-mesh --no-default-features --features mesh
./target/release/ifcfast-mesh model.ifc model.glb
```

## Cache

Parquet files live under `~/.cache/ifcfast/<cache_key>/`, where
`cache_key` is `sha256(file_size + first 4 MB + last 4 MB)` truncated.
Any edit to the IFC invalidates the entry automatically.

Override with the `IFCFAST_CACHE` environment variable, e.g.
`IFCFAST_CACHE=/srv/cache ifcfast extract model.ifc`.

Disk footprint on a 200 MB Archicad IFC: **2.4 MB total** zstd-compressed.

## Data schemas

All extractors return long-format (one row per fact, no nested fields).
Easy to join, easy to filter, easy to flatten to Excel.

**Missing values:** string columns use pandas `StringDtype` with `nan`
as the NULL sentinel (chosen for memory and pyarrow round-trip).
Cells corresponding to a STEP `$` field hold `float('nan')`, **not**
Python `None`. Use `.isna()` to test, not `== None` or `is None`:

```python
m.classifications[m.classifications.identification.isna()]   # correct
m.classifications[m.classifications.identification == None]  # always False
[r for r in m.classifications.itertuples() if r.identification is None]  # always False
```

If you're cross-checking against `ifcopenshell` (which returns `None`),
normalise NaN→None on the comparison side.


### psets

| column | type | description |
|---|---|---|
| `guid` | str | `IfcProduct.GlobalId` |
| `pset_name` | str | e.g. `Pset_WallCommon` |
| `prop_name` | str | e.g. `IsExternal` |
| `value` | str \| None | booleans normalised to `True` / `False` / `UNKNOWN` |
| `value_type` | str \| None | `IfcBoolean`, `IfcText`, `IfcReal`, … |

### quantities

| column | type | description |
|---|---|---|
| `guid` | str | `IfcProduct.GlobalId` |
| `qto_name` | str | e.g. `Qto_WallBaseQuantities` |
| `quantity_name` | str | e.g. `NetVolume`, `GrossArea`, `Length` |
| `value` | str \| None | numeric value as string |
| `quantity_type` | str | `Area` / `Length` / `Volume` / `Count` / `Weight` / `Time` |
| `unit_step_id` | int \| None | usually None (project default applies) |

### materials

| column | type | description |
|---|---|---|
| `guid` | str | `IfcProduct.GlobalId` |
| `role` | str | `direct` / `list` / `layer` / `unknown` |
| `layer_index` | int | 0-based for layered materials, `-1` otherwise |
| `material_name` | str \| None | material label |
| `layer_thickness_mm` | float \| None | only set for `role="layer"` |
| `category` | str \| None | IFC4 only |

### classifications

| column | type | description |
|---|---|---|
| `guid` | str | `IfcProduct.GlobalId` |
| `system_name` | str \| None | `NS 3451`, `Uniformat II`, `OmniClass` |
| `edition` | str \| None | e.g. `2022` |
| `identification` | str \| None | the actual code |
| `name` | str \| None | human label |
| `location` | str \| None | URI to spec (rarely populated) |
| `source` | str \| None | publisher |

### drift

| column | type | description |
|---|---|---|
| `guid`, `entity`, `source` | str | identification |
| `triangle_count`, `surface_area`, `volume_abs` | int / float | geometric stats |
| `placement_x/y/z` | float | what `IfcLocalPlacement` says |
| `centroid_x/y/z` | float | where the mesh AABB centre actually is |
| `drift_distance` | float | Euclidean distance, mm |
| `max_extent` | float | largest AABB dimension |
| `drift_ratio` | float | `drift_distance / max_extent` |
| `drift_severity` | str | `ok` / `warn` / `error` |

Severity rule: `ok` when `drift_ratio ≤ 2.0` or `drift_distance < 10
mm`; `error` when `drift_ratio > 10.0` and `drift_distance > 10 mm`.

A 100 m wall placed at one end has ratio 0.5 (legitimate). A 50 mm sensor
100 m from its placement has ratio 2000 (clear authoring bug).

## Spatial hierarchy & relationships

The tier-1 index exposes three long-format relationship tables and a
small set of traversal helpers. No graph library required — the tables
are plain `pandas.DataFrame`s with string-guid columns and feed
directly into NetworkX, PyArrow or a custom three.js scene if you want.

```python
m = ifcfast.open("model.ifc")

m.contained_in     # IfcRelContainedInSpatialStructure (product → storey)
m.aggregates       # IfcRelAggregates (child → parent, with parent_kind)
m.storey_building  # storey → building (subset of aggregates)

# Traversal helpers — none of these raise on unknown guids.
m.parent(guid)            # unified parent (aggregate, else spatial storey)
m.children(guid)          # direct children: products + sub-decomposition
m.ancestors(guid)         # chain to root (storey → building → site → project)
m.descendants(guid)       # BFS over the unified-children tree
m.storey_of(guid)         # spatial container, or None
m.building_of(guid)       # building that hosts the storey, or None
m.products_in(parent)     # all products under parent (BFS, filtered)
```

`parent_kind` on `m.aggregates` is one of `product` / `storey` /
`building` / `site` / `project` / `space`. The tables are persisted in
the parquet cache, so hot reloads keep graph access at full speed.

Coverage today: `IfcRelAggregates` (decomposition) and
`IfcRelContainedInSpatialStructure` (spatial). `IfcRelVoidsElement`
(wall ↔ opening), `IfcRelConnectsElements`, and other relationship
types are not yet emitted — that means `IfcOpeningElement` products
appear in `m.products` but not in the graph traversal yet.

## Federated floor synthesis

Multi-discipline projects have the same physical floor named differently
by ARK / RIB / RIV / RIE authors. `ifcfast.federated_floors` clusters by
elevation across discipline models and applies a project-supplied YAML
rule.

```yaml
# examples/projects/lbk-building-c.yaml
prefix: "C - "
overrides:
  Plan U1: Hav
  C - U1:  Hav
idempotent_labels: [Hav]
apply_drop_leading_zero: true
```

The module is project-agnostic — project tables live in user config.

## Architecture in two paragraphs

The Rust core (`crates/core`) does one byte-level pass over the IFC's
DATA section using a string-aware STEP tokenizer (memchr-accelerated).
That pass builds an `EntityTable` — a `step_id → byte_range` map of
every entity in the file. Each PyO3 entry point (`index_ifc`,
`extract_psets`, etc.) walks the table once, dispatching on entity type
and extracting only the fields that layer needs.

The Python cache (`ifcfast.cache`) writes each extractor's output as
zstd-compressed parquet, keyed by `sha256(size + 4 MB head + 4 MB tail)`
so any IFC edit invalidates automatically. Hot reads are pure
pandas / pyarrow — no Rust call needed. There is no `ifcopenshell.open()`
anywhere in the data path; `ifcopenshell` is an *optional* dev dep used
only for cross-checking parity in tests.

## What it doesn't do

- Write or modify IFCs. Read-only by construction.
- Schema validation. Trusts the file's syntax. Use
  [bsi-validator](https://github.com/buildingSMART/IFC) for conformance.
- Tessellate `IfcBooleanClippingResult` (walls with openings render
  without cutouts — gross volume correct, net volume not).
- NURBS / advanced BREP geometry. ~0.5% of elements in typical exports.
- Property variants beyond `IfcPropertySingleValue` —
  `IfcPropertyEnumeratedValue`, `IfcPropertyListValue`,
  `IfcPropertyBoundedValue`, `IfcComplexProperty` are skipped. Covers
  ~90% of psets seen on Revit / Archicad / Tekla / MagiCAD exports.

## Layout

```
crates/core/         Rust extension (PyO3) — tokenizer, indexer, extractors, mesh
  src/
    lib.rs           PyO3 entry points
    lexer.rs         STEP tokenizer
    indexer.rs       tier-1 product / storey index
    entity_table.rs  step_id → byte range lookup
    extractors/      psets, quantities, materials, classifications
    mesh/            extrusion, mapped, face sets, BREP, glTF writer
    bin/             ifcfast-bench, ifcfast-mesh CLIs
python/ifcfast/      Public Python API
  __init__.py        ifcfast.open(), Model, header, classify
  header.py          STEP header reader (tier-0)
  model.py           Model class + native tier-1 driver
  cache.py           parquet cache for index + data layers
  classify.py        element-mode policy (count / measure / linear / skip)
  federated_floors.py multi-discipline floor synthesiser
  cli.py             ifcfast CLI
docs/history/        origin doc + audit issues from ifc-workbench
examples/projects/   project YAMLs for federated_floors
tests/               pytest suite
```

## License

MIT — see [`LICENSE`](LICENSE).
