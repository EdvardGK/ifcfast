# ifcfast

Fast native IFC parsing, data extraction and geometric analytics. Python
on top, Rust underneath, no `ifcopenshell.open()` on the hot path.

Aimed at the "open an IFC, get the data out, run QTO / quality
queries" workflow. Tier-1 parse is byte-identical to `ifcopenshell` and
~20-27× faster on production files.

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

Needs Rust 1.95+ and Python 3.10+.

```bash
pip install maturin
maturin develop --release      # builds the Rust extension, ~30 s first time
```

For production: `maturin build --release` produces a wheel.

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
