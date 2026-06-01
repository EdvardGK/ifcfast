# ifcfast — the agent-first IFC parser

[![PyPI](https://img.shields.io/pypi/v/ifcfast.svg)](https://pypi.org/project/ifcfast/)
[![Python versions](https://img.shields.io/pypi/pyversions/ifcfast.svg)](https://pypi.org/project/ifcfast/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/EdvardGK/ifcfast/actions/workflows/ci.yml/badge.svg)](https://github.com/EdvardGK/ifcfast/actions/workflows/ci.yml)

> **An agent-first IFC parser. Built for AI agents, RPA, and analytics
> pipelines that need to ask questions of a model without loading a
> geometry kernel. Complements `ifcopenshell` — different tradeoffs,
> different jobs.**

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

> **Early & unverified.** `ifcfast` is under active development and has
> not been validated against established tools. Treat its output as
> provisional — cross-check against `ifcopenshell` or your existing
> toolchain before relying on it, and
> [open an issue](https://github.com/EdvardGK/ifcfast/issues) when
> something looks wrong. It complements `ifcopenshell` (which owns
> geometry kernels, authoring, and schema work) rather than replacing it.

**What `ifcfast` is**

- **A native, kernel-free parser.** A Rust core (via PyO3, mmap-based)
  reads the IFC STEP data section directly. No geometry kernel on the
  hot path — a deliberate scope cut.
- **Data layers as pandas.** Property sets, quantities, materials, and
  classifications come back as long-format DataFrames. Filter, join,
  pivot, export.
- **Geometry without a CAD kernel.** Per-product triangle meshes
  (`m.meshes()`), area-weighted point-cloud sampling with normals
  (`m.point_cloud()`), and geometric quantities (`m.mesh_qto()`) —
  handed back as numpy / pandas, ready for trimesh / Open3D.
- **Spatial-relationship graph built in.** `m.contained_in /
  .aggregates / .storey_building` + seven traversal helpers
  (`parent`, `children`, `ancestors`, `descendants`, `storey_of`,
  `building_of`, `products_in`). `m.ancestors(wall_guid)` reaches the
  project.
- **Self-describing.** `m.summary()`, `m.schemas`, `m.preview(table)`
  answer "what am I looking at?" without triggering extracts. Every
  CLI subcommand has `--json`.
- **Parquet cache.** A re-open reuses extracted tables; the cache key
  invalidates on any edit or library change.

See [AGENTS.md](AGENTS.md) for the full agent guide and a copy-paste
`system_prompt()` you can drop into your LLM's context.

> `ifcfast` was extracted on 2026-05-13 from the
> [`EdvardGK/ifc-workbench`](https://github.com/EdvardGK/ifc-workbench)
> scratch repo. See [`docs/history/origin.md`](docs/history/origin.md)
> for the trail back and what was renamed.

## What it gives you

| layer | format |
|---|---|
| Products (GUID, type, name, storey, parent, tag) | dict of parallel lists |
| Property sets (incl. enumerated / list / bounded / complex) | long-format `pandas.DataFrame` |
| Element quantities | long-format `pandas.DataFrame` |
| Materials (layer / constituent / profile sets) | long-format `pandas.DataFrame` |
| Classifications | long-format `pandas.DataFrame` |
| Per-product triangle meshes | `m.meshes()` → numpy `(vertices, faces)` |
| Sampled point clouds (+ normals) | `m.point_cloud()` → `pandas.DataFrame` |
| Geometric QTO (volume, area, orientation) + per-planar-surface table | `m.mesh_qto()` → `(products_df, surfaces_df)` |
| Triangle meshes (extrusion / mapped / face sets / BREP) | OBJ / glTF / CSV |
| Placement-vs-mesh drift report | `pandas.DataFrame` |
| Substrate (geometry + semantics) | GeoParquet (DuckDB-queryable) |

The Rust core is built for speed and bounded memory (mmap-based, no
geometry kernel loaded), but those properties are **not yet
benchmarked or independently verified** — don't treat any timing as a
promise. If you measure something surprising, please report it.

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

**Missing values:** STEP `$` fields surface as a missing-value sentinel
— either `float('nan')` (the common case, for numeric columns and
pyarrow-round-tripped string columns) or Python `None` (for some
object-dtype columns where pandas preserves the raw extractor output).
Both flavours are caught by `pd.isna()`; neither is caught by
`== None` or `is None`. Always test with `pd.isna()` or `.isna()`:

```python
m.classifications[m.classifications.identification.isna()]   # correct, catches both
m.classifications[m.classifications.identification == None]  # always False
[r for r in m.classifications.itertuples() if r.identification is None]  # misses NaN cells
```

If you're cross-checking against `ifcopenshell` (which returns `None`),
normalise NaN→None on the comparison side, or use
`pd.isna()` on the ifcfast side.


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

All length/area/volume columns are in SI (metres / square metres /
cubic metres) — column names carry the unit suffix so `m.drift` joins
to `m.mesh_qto()` results without any rescaling.

| column | type | description |
|---|---|---|
| `guid`, `entity`, `source` | str | identification |
| `triangle_count` | int | triangles in the product mesh |
| `surface_area_m2`, `volume_abs_m3`, `aabb_volume_m3` | float | geometric stats, SI |
| `placement_x_m/y_m/z_m` | float | what `IfcLocalPlacement` says, metres |
| `centroid_x_m/y_m/z_m` | float | where the mesh AABB centre actually is, metres |
| `drift_distance_m` | float | Euclidean distance from placement to centroid, metres |
| `max_extent_m` | float | largest AABB dimension, metres |
| `drift_ratio` | float | `drift_distance_m / max_extent_m`, unitless |
| `drift_severity` | str | `ok` / `info` / `warn` / `error` |
| `mesh_quality` | str | `closed` / `open_shell` / `degenerate` |

Severity rule (computed against SI values — unit-independent):
- `ok` when `drift_ratio ≤ 2.0` or `drift_distance_m < 0.010` (rounding noise)
- `warn` when `2.0 < drift_ratio ≤ 10.0`
- `error` when `drift_ratio > 10.0` and `drift_distance_m > 0.010`
- `info` when the per-row drift was demoted by the world-coordinate-baked
  detector (see below) — these rows would otherwise be `warn`/`error`
  but are part of a model-wide authoring convention rather than per-element
  bugs.

**World-coordinate-baked detector.** Common on Tekla / IFC2X3
structural exports: most products have identity `IfcLocalPlacement`
and geometry authored directly in world coordinates. Under a naive
per-row drift check this surfaces as model-wide "error". When ≥ 80 %
of meshed products are placed at the origin (within 1 mm), the file
is flagged `world_coordinate_baked=True` (queryable via
`m.world_coordinate_baked`) and the per-row severity of the
origin-placed products is demoted to `info`. The model-level fact is
the actionable signal; per-element drift would just be noise.

A 100 m wall placed at one end has ratio 0.5 (legitimate). A 50 mm sensor
100 m from its placement has ratio 2000 (clear authoring bug).

## Spatial hierarchy & relationships

The tier-1 index exposes three long-format relationship tables and a
small set of traversal helpers. No graph library required — the tables
are plain `pandas.DataFrame`s with string-guid columns and feed
directly into NetworkX, PyArrow or a custom three.js scene if you want.

```python
m = ifcfast.open("model.ifc")

m.contained_in     # IfcRelContainedInSpatialStructure (product → spatial container)
                   # columns: product_guid, container_guid, container_kind
                   #          (kind ∈ site / building / storey / space)
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

Coverage today: `IfcRelAggregates` (decomposition),
`IfcRelContainedInSpatialStructure` (spatial), `IfcRelVoidsElement`
(opening ↔ host — `m.voids` DataFrame), and `IfcRelDefinesByType`
(product ↔ type — populates `type_guid` / `type_name` /
`type_source` on each product, plus `m.type_objects` as the
catalogue). `IfcRelConnectsElements` and other relationship types
are still on the next-tier list — file an issue with a sample if
you need one. `IfcSpace` is surfaced as `m.spaces` (rooms / zones
kept separate from "things you build").

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
only to cross-check output in tests.

## Reveal-all geometry stance

When the mesh pipeline meets a composite solid (`IfcBooleanResult`,
`IfcBooleanClippingResult`, `IfcCsgSolid`) it does **not** perform the
boolean. Both operands are emitted as their own visible mesh segments
with compound tags like `boolean_first_operand|extrusion` (the host
wall) and `boolean_second_operand|halfspace_bounded` (the door clip).
You see the file as authored — the host volume AND the clip volume,
not a curated "wall minus opening" summary. The glTF emitter writes
each segment's `(start, count, source)` into per-node
`extras.segments` so the viewer can colour, split, or filter by role.

Representation types we don't tessellate yet (e.g.
`IfcRevolvedAreaSolid`, `IfcSurfaceCurveSweptAreaSolid`,
`IfcCsgPrimitive3D` leaves) surface in `mesh_stats.by_source` as
`unhandled:IFCXXX` entries so you can see exactly what the file
contained that we couldn't reveal — never a silent drop.

## What it doesn't do

- Write or modify IFCs. Read-only by construction. (Round-trip
  editing is the next major milestone — see AGENTS.md "North star".)
- True boolean / CSG composition. By design — we reveal BOTH
  operands instead.
- Schema validation. Trusts the file's syntax. Use
  [bsi-validator](https://github.com/buildingSMART/IFC) for conformance.
- Curved-surface tessellation for `IfcAdvancedBrep` — face loops are
  triangulated as polygons (tagged `advanced_brep_approx`).
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
