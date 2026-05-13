# Session: fastparse v3 — native Rust tier-1, multi-semantic bboxes, project-agnostic federation, full ITO validation arc

## Summary

Took the fastparse worklog's whole 7-item menu and shipped 4 of them, with the centerpiece being a from-scratch **Rust STEP tokenizer (`crates/ifcfast/`) that's 20–27× faster than `ifcopenshell.open()`** on the LBK Building C set and byte-identical in parity. End-to-end ITO validation now hits **100% Name, 100% Federated Floor, 92.3%/72.4% bbox** (vs 56.1% federated baseline). Module-level cleanup pulled all project-specific code out of `federated_floors.py` into a generic config-driven design — LBK lives in a project YAML, not in the parser. PR #3 open at `9487227`.

## Changes

### Multi-semantic bbox columns — cache v6 → v7
- `extents.py` `BBox` dataclass + `_transform`: now emit `local_x/y/z` (raw element-local, not reordered) AND `solibri_length/width/height` (axis-reordered by world-vertical alignment)
- `extents_variants.py`: completely rewritten — V1B/V1AB classes deleted as redundant (base extractor now emits both framings unconditionally), tuple grew from 10 to 13 elements
- `cache.py`: `CACHE_VERSION 6 → 7`, parquet schema adds `solibri_length`, `solibri_width`, `solibri_height`
- `SPEC.md`: documented new schema + variant table
- `validate_against_ito.py`: geometric agreement section reports against all three framings (local raw / Solibri reorder / world AABB) — surfaces that Solibri's ITO oracle uses the local-Solibri-reorder framing, world AABB is great for height but tanks on L/W (3.2% — rotated walls inflate XY)

### CGAL hybrid kernel
- `extents_variants._run_iterator`: `geometry_library="opencascade"` → `"hybrid-cgal-simple-opencascade"`. Per agent-B investigation: 1.5–13× on tessellation vs pure OCCT, correctness within 1.8 µm. **DON'T use bare `cgal-simple`** — silently drops 0.4% of elements; the hybrid form falls back to OCCT for what CGAL can't simplify.

### Federated floor synthesis — `fastparse/federated_floors.py`
- Greedy 1-D elevation clusterer (configurable tolerance, default 100 mm)
- `make_prefix_rule(prefix, overrides, idempotent_labels, apply_drop_leading_zero)` factory
- `load_rule_from_yaml(path)` for project configs
- `drop_leading_zero()` generic helper exported
- Rule registry (only `default` ships pre-registered)
- **Refactor**: started with hardcoded `lbk_rule` + `LBK_OVERRIDES` table grown during validation; user objected ("LBK is a specific project, this should be made for IFC"). Pulled it all out. LBK config now lives in `data/projects/lbk-building-c.yaml`. Module is project-agnostic.

### Native Rust tier-1 indexer — `crates/ifcfast/`
- New PyO3 + maturin crate. `Cargo.toml` dual `crate-type = ["cdylib", "rlib"]`; `python` feature gates PyO3 so the standalone `ifcfast-bench` binary builds without libpython linkage
- `src/lexer.rs`: byte-level STEP tokenizer (string-aware terminator scan via memchr SIMD), decodes single-quoted strings with ISO-10303-21 escapes: `''` (literal quote), `\X\HH` (5-char Latin-1, no terminator — Revit/MagiCAD/Archicad form), `\X2\HHHH...\X0\` (UTF-16BE), `\S\C` (high-bit Latin-1: `C | 0x80`, used by Tekla for uppercase Norwegian)
- `src/indexer.rs`: tier-1 attribute extractor for ~150 IfcProduct subtypes + IfcBuildingStorey + IfcSite + IfcBuilding + IfcProject + IfcApplication + IfcRelContainedInSpatialStructure + IfcRelAggregates + IfcUnitAssignment/IfcSIUnit. Output: column-major `Vec<u64>` / `Vec<String>` / `Vec<Option<String>>`. Relationships stored as parallel `Vec<u64>` (not `Vec<(u64,u64)>` — saves a tuple alloc per row)
- `src/lib.rs`: PyO3 bindings under `#[cfg(feature = "python")]`. Reports `open_ms` (mmap), `index_ms` (Rust pass), `marshal_ms` (PyDict construction, separately so callers can see when this dominates — turns out it's 0.03–1.4% across all tested files, NOT the bottleneck)
- `src/bin/bench.rs`: standalone Rust benchmark binary, pure-Rust path, no PyO3
- `ifc_workbench/quantities/qto.py`: added `classify_by_name(entity, schema)` — strings-only port of `classify_element` using `ifcopenshell.schema_by_name(schema).declaration_by_name(...)` for inheritance fallback (no file open needed)
- `ifc_workbench/fastparse/index.py`: `_open_fast_native()` builds an `IndexedModel` from the native dict, resolves step_id → guid for relationships, leaves `_ifc=None`. Falls back to ifcopenshell on import failure or any exception
- `open_fast()`: new `use_native=True` kwarg (default ON); env `IFC_WORKBENCH_NO_NATIVE=1` forces ifcopenshell path

### Dashboard verification panel — `fastparse/server.py` + static
- New `POST /api/benchmark` runs `_ifcfast.index_ifc` and `ifcopenshell.open` sequentially on the same file, returns timings + parity (products, storeys, top-12 type histogram diff)
- New "native tier-1 verification" panel in `index.html` with Run-benchmark button, three-column native/ifcopen/verdict layout, speedup multiplier, parity verdict
- `/api/health` reports `native_indexer` + `native_version`

### Tests
- `tests/test_federated_floors.py`: rewrote after refactor; 19 cases covering clustering, default_rule, make_prefix_rule, YAML loader, registry, and an integration test that loads the shipped LBK YAML

## Technical Details

### Performance results

| file | size | ifcopenshell.open | _ifcfast | speedup |
|---|---:|---:|---:|---:|
| LBK_RIE_C | 29 MB | 1.5 s | 76 ms | **20×** |
| LBK_RIBp_C | 45 MB | 2.6 s | 175 ms | 15× |
| LBK_RIVv_C | 128 MB | 6.4 s | 332 ms | 19× |
| LBK_ARK_C | 201 MB | 7.8 s | 366 ms | 21× |
| LBK_C_RIVr (MEP) | 598 MB | 30 s | 1.14 s | **26×** |
| ST28_RIV (off-suite) | 834 MB | OOM on 8 GB box | 2.4 s | — |
| Sannergata_RIV (Windows, MagiCAD) | 824 MB | ~74 s | 1.5–2.0 s | **30–47×** |

### ITO validation arc — 4 runs across the session
| run | name | federated_floor | cold parse | key change |
|---|---:|---:|---:|---|
| baseline (worklog) | 100% | 56.1% | 230 s | ifcopenshell tier-1, no federation |
| 14:09 | 65.3% | 0% | 167 s | first full v3 run — found two regressions |
| 14:20 | 90.1% | 82.8% | 146 s | `a4510cf`: `\X\HH` 5-char + idempotency |
| 14:28 | **100%** | **100%** | 157 s | `c5484ed`: `\S\C` + any-member override |
| 14:55 | **100%** | **100%** | 147 s | `bc1076d` refactor (YAML config) — held |

### Bugs found during validation (the substantive ones)

1. **`\X\HH` short form is 5 chars, NOT 6**. The official ISO-10303-21 spec uses `\X\HH\` (6 chars with closing backslash) but actual Revit / MagiCAD / Archicad exports drop the trailing `\`. Decoder needed `i += 5` not `i += 6`. Without this, `dør` came out as the literal string `d\X\F8r`.
2. **`\S\C` escape entirely unhandled**. Tekla/Revit use this form where the escaped ASCII char gets its high bit set: `\S\E` → Å (0x45 | 0x80 = 0xC5), `\S\X` → Ø. Common for uppercase Norwegian in MEP family names (`STÅLSØYLE`).
3. **`lbk_rule` not idempotent** — `'Hav'` became `'C - Hav'`, `'C - U1'` became `'C - C - U1'`. Fixed with prefix-already-present guard.
4. **`lbk_rule` only checked most-common cluster name** — at ~600 mm elevation the cluster had `Plan U1` (RIE) + `C - U1` (ARK + RIVr) + `2. C - U1` (RIVv). Most-common was `C - U1` (3×) so the rule emitted `C - U1`, but `Plan U1` was in `LBK_OVERRIDES → Hav`. New rule scans ALL cluster members for overrides.

### Honest observation on the perf wins from the decoder fix

Edvard reported on Issue #5 that AFTER the `\X\HH` + `\S\C` fixes, Norwegian-heavy files got **faster** (Sannergata_RIV −9%, OBF/SM ARK −28% to −40%). Explanation: the old buggy path emitted 5 literal ASCII bytes (`\X\F8`) per escape into the output String; the new path emits 2 UTF-8 bytes (`ø`). Fewer allocations, less heap pressure. A correctness fix that's accidentally a perf win.

### marshal_ms diagnosis (Edvard's hypothesis disconfirmed)

Edvard's Issue #2 conjectured PyO3 marshalling dominates on the 144K-product Sannergata_RIV. We instrumented and measured: marshalling is **1.1%** of total. The bottleneck is in the Rust extract path itself — `extract_product`'s string allocation per product GUID, HashMap lookups, the post-pass storey-ids filter. Next perf work goes there.

### Other notable architectural decisions

- **Cache version bump = forced re-parse**. v7 schema invalidates v6 caches automatically. No backward-compat reader for old caches; classifier signature plus cache version cover it.
- **Tier-2 still goes through ifcopenshell**. Native path returns `_ifc=None`; `compute_bboxes()` reopens via `ifcopenshell.open()` lazily. The 8 GB local box OOMs on the 598 MB MEP file's tier-2; full validation completed nonetheless after the third try. ST28_RIV (834 MB) successfully native-parsed; ifcopenshell-comparison killed by OOM (exit 144).
- **Parallel `Vec<u64>` relationship columns** were originally Edvard's Issue #2 suggestion for marshalling perf. Turned out to deliver a side-effect 24% Rust-side speedup on the 834 MB file because the internal post-pass filter and storey_building scan no longer allocate `(u64, u64)` tuples either. Always do the refactor even when the original justification turns out wrong.

## Next

1. **Issue #6 (open)**: Validation pipeline test against a second production Solibri ITO. Generality check. Edvard's timing.
2. **Issue #2 followup (carried forward in close comment)**: Flamegraph `extract_product` + storey-ids HashSet filter. Two candidate fixes flagged: arena-allocated strings, or eliminate filter pass by tracking storey membership during the main walk.
3. **PR #3 review**: branch at `9487227`, 7 commits, ready.
4. **Optional next-tier items** still in the original worklog menu: boolean subtraction (gross/net for walls with openings), pset persistence (`psets.parquet`), tier-4 on-demand mesh.
5. **3D viewer**: pinned in `docs/research/2026-05-11-12-00_3d-viewer-options.md`. ThatOpen Components recommended. Conversion-side bottleneck is exactly the kind of thing the Rust tokenizer extends toward (native Fragments emitter).

## Notes

- LBK_C 955 MB IFC samples + the 18 MB ITO Excel are gitignored. The bbox-diag parquets under `docs/research/` may grow large; `*_bbox-bakeoff-*.json` and `*_ito-validation*.parquet` excluded via .gitignore.
- The OOM lesson: don't run `ifcopenshell.open()` on >500 MB files on this 8 GB box. Native tokenizer is mmap-based and stays well under 1 GB resident. For full pipeline test of the largest files, defer to edkjo SSH (32 GB) or Edvard's Windows i7.
- Edvard validated all four major asks (Issue #1 warnings, Issue #2 followups, Issue #4 ITO validation, Issue #5 re-bench). Confirmed `marshal_ms <2%` across 7 production IFCs and 100% Norwegian-decoder parity (143,704 names compared byte-for-byte) on the 824 MB MagiCAD file.
- All session memory-relevant facts are mirrored into the project memory at `~/.claude/projects/-home-edkjo-workspace-inbox-ifc-workbench/memory/`.
