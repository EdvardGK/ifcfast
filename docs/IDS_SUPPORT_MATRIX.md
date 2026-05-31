# IDS 1.0 support matrix (ifcfast)

This documents what **`engine=ifcfast`** and **`engine=rust`** support vs **IfcTester** (reference) and **buildingSMART IDS 1.0**.

**Conformance harness:** `scripts/run_buildingsmart_ids_conformance.py`  
**Official tests:** [buildingSMART/IDS TestCases](https://github.com/buildingSMART/IDS/tree/development/Documentation/ImplementersDocumentation/TestCases)  
Set `IDS_TESTCASES_ROOT` to the cloned `TestCases` folder.

## Engine summary

| Engine | Role |
|--------|------|
| `ifctester` | IfcOpenShell + IfcTester — full IDS 1.0 reference |
| `ifcfast` | Columnar Python on `Model` (products + spatial) |
| `rust` | Native check on `IndexedFile` + `PsetTable` |
| `auto` | `ifcfast` per spec when possible; else IfcTester fallback |

## Facet matrix

| Facet | IDS 1.0 | ifcfast | rust | Notes |
|-------|---------|---------|------|-------|
| **Entity** | ✅ | ✅ | ✅ | Products + **spatial containers** (Building, Storey, Space, Site, Project) |
| **Attribute** | ✅ | ⚠️ | ⚠️ | Name, Tag, ObjectType, GlobalId, PredefinedType on indexed columns; Description not on spatial |
| **Property** | ✅ | ⚠️ | ⚠️ | `IfcPropertySingleValue` via `PsetTable`; not all IfcProperty* subtypes |
| **PartOf** | ✅ | ⚠️ | ⚠️ | Transitive aggregate, nest, group, void, containment on fast path; predefinedType on spatial partial — see roadmap |
| **Classification** | ✅ | ⚠️ | ⚠️ | `classifications` table + system/identification restrictions; optional cardinality always passes (IfcTester parity) |
| **Material** | ✅ | ⚠️ | ⚠️ | `materials` table + name match / restrictions; optional cardinality always passes |

Legend: ✅ supported · ⚠️ subset · ❌ not on fast path

## Spatial containers (IfcBuilding, Storey, Space, Site, Project)

| Capability | ifcfast | rust |
|------------|---------|------|
| Entity applicability on spatial types | ✅ `objects` = products ∪ spatial | ✅ unified `object_*` index |
| Property requirements on spatial | ✅ if pset rows exist for GUID | ✅ same `prop_lookup` |
| Attribute on spatial | ⚠️ Storey `Name`; Space/Building often no name in index | ⚠️ same |
| H29 applicability lists including `IFCSPACE` | ✅ spaces in candidate pool | ✅ |

Removed fallback reason `entity:spatial_container` — spatial specs run on the fast path when other facets allow.

## Restrictions (value constraints)

| Constraint | ifcfast / rust | IfcTester parity |
|------------|----------------|------------------|
| `simpleValue` | ✅ | ✅ |
| `enumeration` | ✅ (+ coercion) | ✅ |
| `pattern` | ✅ full match + XSD translate at compile | ✅ |
| bounds (min/max inclusive/exclusive) | ✅ | ✅ |
| length / minLength / maxLength | ✅ | ✅ |

## Specification-level semantics

| Feature | ifcfast | rust |
|---------|---------|------|
| Applicability `minOccurs` / `maxOccurs` | ✅ via IfcTester-loaded spec | ✅ from compiled JSON / XML |
| `ifcVersion` list / space-separated | ✅ | ✅ |
| Requirement cardinality required/optional/prohibited | ✅ IDS 1.0 rules | ✅ |
| Prohibited specification (`maxOccurs=0`) | ✅ | ✅ |

## XML / compile paths

| Path | Validates `.ids` XSD | Use |
|------|---------------------|-----|
| IfcTester `ids.open(validate=True)` | ✅ | `load_ids`, `compile.py` |
| Rust `parse_ids_xml` | ⚠️ subset | H29-style packs without Python |
| [IDS-Audit-tool](https://github.com/buildingSMART/IDS-Audit-tool) | ✅ `.ids` only | CI optional (`IDS_AUDIT_TOOL_PATH`) |

## Known gaps (not “full IDS 1.0”)

1. **Classification / Material** — Python facets exist; **extractor GUID scope** and inherited/type semantics missing.  
2. **PartOf** — missing relations (nest, group, void) and **transitive** aggregate/container walks.  
3. **Property** — not all IFC property entity types; no unit conversion like IfcTester.  
4. **Entity** — no IFC subclass inheritance (explicit type match only).  
5. **Applicability universe** — IfcTester uses broader `IfcObjectDefinition` filter; ifcfast uses indexed products + spatial.  
6. **Spatial attributes** — limited name/tag on Space/Building unless present in index/psets.  
7. **Rust** — unsupported PartOf relations and non-indexed attribute columns fall back in `auto` only.

## How to claim conformance

| Level | Requirement |
|-------|-------------|
| **A** Valid IDS files | `ids-tool audit` + IfcTester load |
| **B** Checker correct | Official TestCases: ifcfast status == IfcTester; pass/fail matches filename |
| **C** Production | H29/Nobel packs: 0 failed-GUID diff on `engine=auto` for product specs |

**100% IDS 1.0** = level **B** green on full TestCases corpus (all facets you care about) + documented exceptions in this matrix.

## CI commands

```powershell
$env:IDS_TESTCASES_ROOT = "C:\code\buildingSMART-IDS\Documentation\ImplementersDocumentation\TestCases"

# Quick sample (entity facet)
python scripts\run_buildingsmart_ids_conformance.py --facet entity --limit 40 --engine ifcfast

# Pytest smoke (10 entity cases)
pytest tests/ids/test_buildingsmart_conformance.py -v

# Rust engine on official TestCases
python scripts\run_buildingsmart_ids_conformance.py --engine rust --json reports\bsmart_conformance_rust.json
python scripts\summarize_conformance.py reports\bsmart_conformance_rust.json

# Rust unit + parity
cd crates\core && cargo test ids::
```
