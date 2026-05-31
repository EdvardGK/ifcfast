# IDS validation (`ifcfast[ids]`)

Fast IDS 1.0 checking on ifcfast columnar indexes, with **IfcTester** loading `.ids` XML and optional **IfcOpenShell** fallback per specification.

## Install

```powershell
pip install -e "C:\code\ifcfast[ids]"
```

## CLI

```powershell
ifcfast ids model.ifc requirements.ids --json -o report.json
ifcfast ids model.ifc requirements.ids --engine auto
```

Engines:

| Engine | Behaviour |
|--------|-----------|
| `ifcfast` | Columnar engine only; fails specs with unsupported facets |
| `ifctester` | Full IfcTester validation (slow, reference) |
| `auto` | ifcfast when all facets are supported; else per-spec IfcTester |
| `rust` | Native Rust engine on `IndexedFile` + `PsetTable` (no pandas on check path) |

## Rust engine

The `rust` engine compiles IDS to a `CompiledIds` JSON IR (IfcTester or `compile_ids_xml` in `_core`), then runs `validate_ids_native` in one native pass:

```python
from ifcfast.ids import validate

report = validate("requirements.ids", "model.ifc", engine="rust")
```

Compile IDS for offline/bench use:

```powershell
python -m ifcfast.ids.compile C:\path\pack.ids -o pack.compiled.json
cargo run --bin ifcfast-ids-bench -- model.ifc pack.compiled.json
```

Timings in the native dict: `open_ms`, `scan_ms`, `index_ms`, `pset_extract_ms`, `entity_table_ms`, `object_map_ms`, `base_ms`, `validate_ms`, `prepare_ms`.

**Cold start (bench IDS on Snowdon ~144 MB):** entity+attribute-only specs use a single tier-1 scan (`IndexProfile::Tier1Validate`, products-only loop). With a **release** native extension, `prepare_ids_session` / `validate_ids_native` cold prepare is ~**130–160 ms**; a **debug** `_core.pyd` is ~**2 s** on the same machine (LLVM unoptimized). Install release bits before benchmarking:

```powershell
.\scripts\bench\install_core_release.ps1
# or: maturin develop --release
# Optional: $env:IFCFAST_BENCH_IFCS = "C:\path\to\model.ifc"
python scripts\bench\timing_breakdown_snowdon.py C:\path\to\model.ifc
```

Fork-only bench scripts live under `scripts/bench/` (see `docs/UPSTREAM_PR.md`).

### Session API (fast repeat validation)

`ifcfast.open()` keeps a native [`IfcFileIndex`](python/ifcfast/model.py) so IDS prepare reuses the tier-1 scan instead of parsing the file again.

After opening a model, prepare once and validate many IDS documents without re-indexing:

```python
import ifcfast
from ifcfast.ids import validate_loaded, load_ids

m = ifcfast.open("model.ifc")
m.prepare_ids_session(compiled=...)  # pass compiled IDS to skip unused extractors / EntityTable
doc = load_ids("rules.ids")
report, _ = validate_loaded(m, doc, engine="rust")
report2, _ = validate_loaded(m, doc2, engine="rust")  # repeat: overlay or tier-1 fast path only
```

Low-level:

- `_core.open_ifc_index(path)` — one tier-1 scan; `.prepare_ids_session(compiled_json)` for IDS.
- `_core.prepare_ids_session(path, compiled_json)` — opens IFC when not using `open()`.
- `IdsSession.validate(compiled_json)` — entity+attribute-only IDS may use a tier-1 columnar fast path (no `EntityTable` / full object pool).

## Facet support matrix

| Facet | Fast path | Fallback trigger |
|-------|-----------|-------------------|
| Entity | `products_df.entity` | — |
| Attribute | `products_df` columns (`Name`, `Tag`, `ObjectType`, …) | Attribute name not materialised |
| Property | `psets` join | `dataType` set (unit/IFC type checks) |
| Classification | `classifications` | — |
| Material | `materials` | — |
| PartOf | `contained_in`, `aggregates`, `contained_in_space` | Nest, group, void/fill, etc. |

Rust engine (`engine=rust`): Entity, Attribute, Property, Classification, Material, PartOf (supported relations per `support.py`). Unsupported attribute columns or PartOf relations raise at validate time — use `auto` or `ifctester` for those specs.

### buildingSMART IDS 1.0 alignment

| Topic | Behaviour |
|-------|-----------|
| Applicability cardinality | `minOccurs` / `maxOccurs` on `<applicability>` (required / optional / prohibited spec) |
| Requirement cardinality | **required** — must exist and match; **optional** — pass if absent, else must match; **prohibited** — must not satisfy constraint |
| Entity names | `simpleValue` or `xs:enumeration/@value` lists (H29 packs) |
| Property `dataType` | XML attribute on `<property>` (e.g. `IFCLABEL`) |
| `ifcVersion` | Space-separated list (`IFC2X3 IFC4`) |
| Restrictions | enumeration, pattern (full match), bounds, length — compiled via IfcTester with XSD pattern translation |
| XML load | IfcTester (`load_ids`) or Rust `compile_ids_xml` / `parse_ids_xml` |

Parity target is **IfcTester** on supported facets; IDS-Audit-tool remains optional for `.ids` file linting.

Full facet/engine matrix: [IDS_SUPPORT_MATRIX.md](IDS_SUPPORT_MATRIX.md).

buildingSMART official TestCases: `scripts/run_buildingsmart_ids_conformance.py` (set `IDS_TESTCASES_ROOT`).

## Scope

- Applicability is evaluated on **IfcProduct** rows in `products_df`, not every `IfcObjectDefinition`. Specs that target non-products may differ from IfcTester unless `engine=auto` falls back.
- Clear ifcfast cache after indexer changes: `%USERPROFILE%\.cache\ifcfast`.

## Python API

```python
from ifcfast.ids import validate

report = validate("requirements.ids", "model.ifc", engine="auto")
print(report.passed_specifications, report.failed_specifications)
```

## Compliance

- IDS XML: IfcTester (+ optional buildingSMART IDS-Audit-tool on `.ids` files).
- IFC results: parity tests vs IfcTester on fixtures and H29 packs (`tests/ids/`).
