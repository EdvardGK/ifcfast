# Session: Fast parser v2 — ownership taken, end-to-end shipped

## Summary

Took ownership of the long-running fast-parser effort. Audited the existing code (stale `core/fast_parser.py` parallel iterator, unreachable from broken CLI), surveyed alternatives (web-ifc tape-reader, Ara3D, ifc-lite Rust/WASM 1259 MB/s), researched IfcOpenShell 0.8's `hybrid-cgal-simple-opencascade` kernel, and shipped a working tiered parser with persistent compressed cache.

## What works

A new `ifc_workbench.fastparse` package with five tiers and a working `ifc-fastparse` CLI:

- **Tier 0** — STEP header read in pure Python (~10–20 ms even on 270 MB files). Returns schema, authoring app, content-hash cache key.
- **Tier 1** — `ifcopenshell.open()` + product/storey index. Cold parse 6–17 s for 80–270 MB models — dominated by Python binding cost of `open()`.
- **Tier 1.5** — Geometry catalog: walks `IfcRepresentationMap` and `IfcExtrudedAreaSolid` to find shape reuse. S8A_RIV's top fitting is reused 1,396×.
- **Tier 2** — Parametric bbox per element, no OCCT. World-space AABB from `IfcExtrudedAreaSolid` profile×depth or `IfcMappedItem`-cached source, transformed through `ObjectPlacement`. 84-100% coverage; 16-30s cold.
- **Tier 3** — zstd-compressed parquet cache keyed by content hash + classifier signature. Cache invalidates on file edit, schema/classifier change, or ifcopenshell version bump.

Cache hit times: **33-105 ms** loading full index + bboxes, regardless of model size. 130-290× speedup over cold.

## Cold/hot numbers, real

| File | Size | Tier 0 | Tier 1 cold | Tier 1.5 | Tier 2 | Hot (1+2) | Bboxes | Cache |
|---|---|---|---|---|---|---|---|---|
| G55_ARK | 80 MB | 12 ms | 5.94 s | 813 ms | 10.78 s | **33 ms** | 11,465 | 0.5 MB |
| S8A_RIV | 138 MB MEP | 19 ms | 13.65 s | 2.63 s | 16.02 s | **105 ms** | 20,769 | 3.2 MB |
| ST28_ARK | 271 MB | 24 ms | 16.98 s | 1.98 s | 30.68 s | **59 ms** | 17,461 | 1.5 MB |

Cache compression: 0.5–2.3% of source IFC size for the index+bboxes.

## What's archived (not deleted)

`sidequests/_archive_2026-05-10-fast-parser-v1/` contains:
- The old `fast_parser.py` (parallel iterator that always tessellated)
- `fast_csv_extract.py` (computed bbox *after* tessellation)
- `docs/performance/*.md` (aspirational write-up referencing scripts that never existed)

README explains why each was superseded. v1 still has reference value but no live imports.

## Classifier audit + patches

`qto.classify_element` had real coverage holes. Added to MEASURE: `IfcReinforcingBar/Mesh/Element`, `IfcTendon/Anchor/Conduit`, `IfcProxy`, `IfcCivilElement`, `IfcGeographicElement`. Added to COUNT: `IfcDiscreteAccessory`, `IfcFastener`, `IfcMechanicalFastener`, `IfcCableCarrierFitting`, `IfcElementAssembly`, `IfcBuildingElementPart`, `IfcTransportElement`, `IfcVibrationIsolator/Damper`, IFC4X3 transport devices. Added to SKIP: `IfcDistributionPort`/`IfcPort` (was implicit), `IfcAlignment*`, `IfcReferent`, IFC4X3 facility wrappers (`IfcBridge`, `IfcRoad`, `IfcRailway`, `IfcMarineFacility`).

Remaining IFC4X3 infrastructure gap (earthworks, geotechnical, civil bearings/kerbs/pavements) documented but deferred — no current project needs it.

## Open items for v3

1. **Cold parse is bottlenecked by `ifcopenshell.open()`.** That's the Python binding cost. Native STEP tokenizer in Rust (ifc-lite hits 1259 MB/s) is the right v3 lever.
2. **`hybrid-cgal-simple-opencascade` kernel** is wired into the design but not yet used — comes online with Tier 4 (on-demand mesh).
3. **Tier 2 coverage gaps** on large architectural models (~16% of ST28 elements deferred). Likely CSG / advanced BREP / swept disk. Each is a small extents-extractor addition.
4. **Pset persistence** (planned `psets.parquet`).
5. **Tier 4 mesh on demand** — `ifcopenshell.geom.iterator` with hybrid kernel, restricted to a query, cached as zstd-compressed npz blobs in sqlite.
6. **CLI repair** — the legacy `ifc_workbench.cli` imports a nonexistent `ifc_analyzer.*` namespace. fastparse ships its own entry point until that's fixed.

## Files

- Plan: `docs/plans/2026-05-10-fast-parser-v2.md`
- Spec: `ifc_workbench/fastparse/SPEC.md`
- Classifier audit: `docs/research/2026-05-10-classifier-coverage.md`
- Benchmark report: `docs/research/2026-05-10-14-28_fastparse-benchmark.md`
- Code:
  - `ifc_workbench/fastparse/__init__.py`
  - `ifc_workbench/fastparse/header.py` (Tier 0)
  - `ifc_workbench/fastparse/index.py` (Tier 1)
  - `ifc_workbench/fastparse/catalog.py` (Tier 1.5)
  - `ifc_workbench/fastparse/extents.py` (Tier 2)
  - `ifc_workbench/fastparse/cache.py` (Tier 3)
  - `ifc_workbench/fastparse/cli.py`
  - `scripts/benchmark_fastparse.py`
- `ifc_workbench/quantities/qto.py` — classifier patches
- `pyproject.toml` — added `ifc-fastparse` entry point

## Methodology notes

- Verified the qto.py classifier by walking every IfcProduct subtype in three schemas rather than trusting it.
- Verified bbox values against real walls (reasonable extents in mm; Revit's GUID-prefix artifact noted and confirmed harmless).
- Benchmark uses real files at three sizes; no copy-pasted historical numbers.
- ffmpeg-style storage philosophy explicit in the spec: encode once (parse), decode many (cache reads). zstd everywhere, shape-domain dedup via catalog.
