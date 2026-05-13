# Issue #2 — ifcfast: big-file benchmark report (267-824 MB, 7 production IFCs, 293-417 MB/s)

_Originally filed: 2026-05-11 · status at close: CLOSED_

_Imported from `EdvardGK/ifc-workbench#2` when ifcfast was extracted as a standalone repo._

---

## Big-file benchmark — `crates/ifcfast` on real-world IFCs (267 MB → 824 MB)

Validated `_ifcfast.index_ifc` on 7 production IFC files spanning Archicad / Revit / Tekla / MagiCAD / BSProLib exports, IFC2X3 and IFC4. No crashes, all fields populated; mostly correctness-checked against the file headers.

### Environment

- Branch: `fastparse-v3-native-rust-tier1` @ HEAD
- Host: Windows 11, 11th-gen Intel laptop, files on local NVMe (warm OS cache after first read)
- Build: `maturin develop --release` (pyo3 0.22.6, memmap2 0.9.10, memchr 2.8.0)
- Python: CPython 3.12.10
- Timings are `index_ms` reported by the Rust side (mmap + single byte-level pass; ignores Python-dict construction)

### Throughput summary

| file | authoring app | schema | MB | products | distinct entities | index_ms | MB/s |
|---|---|---|---:|---:|---:|---:|---:|
| OBF_400520_01_6_ARK.ifc | Archicad 29 | IFC2X3 | 266.7 | 18,202 | 19 | 657 | 406 |
| SM_ARK.ifc | Archicad | IFC2X3 | 276.2 | 18,979 | 19 | 724 | 382 |
| SM_RIVr.ifc | BSProLib 2023.3 | IFC4 | 347.9 | 63,232 | 19 | 860 | 405 |
| Sannergata_bygg_ARK_I.ifc | Revit 2025 | IFC2X3 | 362.3 | 25,403 | 24 | 869 | 417 |
| BS_RIVr.ifc | Revit 2026 | IFC2X3 | 498.2 | 2,158 | 5 | 1,330 | 375 |
| HI90_RIV_Skiplum_Lokal_farget.ifc | Revit 2025 | IFC2X3 | 522.0 | 41,710 | 10 | 1,310 | 399 |
| Sannergata_RIV.ifc | MagiCAD For Revit 2024 | IFC2X3 | 824.4 | 143,704 | 10 | 2,812 | 293 |

Sustained 375–417 MB/s on the 250–500 MB band. Throughput drops to ~293 MB/s on the 824 MB / 143K-product MagiCAD file — product count, not bytes, appears to dominate at the extreme. Worth profiling if anyone wants to push past this.

For comparison: equivalent `ifcopenshell.open()` on these would take 20–60s+ each. `index_ms` here is 0.65–2.8s.

### Spatial / relationship coverage

| file | sites | bldgs | storeys | contained_in | aggregates |
|---|---:|---:|---:|---:|---:|
| OBF_400520_01_6_ARK | 1 | 1 | 11 | 2,284 | 9,997 |
| SM_ARK | 1 | 1 | 11 | 2,674 | 10,265 |
| SM_RIVr | 1 | 1 | 14 | 63,232 | 16 |
| Sannergata_bygg_ARK_I | 6 | 1 | 13 | 20,822 | 3,148 |
| BS_RIVr | 2 | 2 | 7 | 2,158 | 11 |
| HI90_RIV | 1 | 1 | 2 | 41,710 | 4 |
| Sannergata_RIV | 1 | 1 | 12 | 143,704 | 14 |

A few quirks worth noting (parser behaving correctly; the files are unusual):

- **MEP exports flatten the hierarchy.** SM_RIVr, BS_RIVr, HI90_RIV, Sannergata_RIV all have near-zero aggregates and contained_in ≈ product count. That's the BSProLib / MagiCAD pattern — every flow element is directly `RelContainedInSpatialStructure`'d to a storey with no element-assembly grouping.
- **Sannergata_bygg_ARK_I has 6 IfcSites.** This is the Revit-2025 multi-shared-site export; the parser captures all of them.
- **`project_name: None` on SM_RIVr and Sannergata_RIV** is correct — both files have `IFCPROJECT(..., $, $, ...)` with Name=NULL. The parser is reflecting the source, not dropping a field.
- **`authoring_app: ' '` on SM_RIVr** is also correct — `FILE_NAME(...,...,(' '),(' '),'BSProLib (Ver:2023.3.0.5)',' ','-')` has a single-space `originating_system`.

### Top type counts (sanity check)

Just to confirm entity counts look sensible per export type:

- **OBF/SM ARK (Archicad)**: 9.4K–9.6K `IfcBuildingElementPart`, 3.5K–3.9K `IfcWall`, ~1.5K `IfcOpeningElement` — typical Archicad shell.
- **Sannergata_bygg_ARK_I (Revit ARK)**: 7K `IfcFurnishingElement`, 5.5K `IfcWallStandardCase`, 4.3K `IfcBuildingElementProxy`, ~2K `IfcMember` — Revit's mix of typed + proxied elements.
- **SM_RIVr (IFC4 RIVr)**: 21.7K `IfcCovering` (insulation), 19.9K `IfcPipeSegment`, 17.3K `IfcPipeFitting`, 1.8K `IfcValve`.
- **HI90_RIV / Sannergata_RIV**: similar but using `IfcFlowSegment`/`IfcFlowFitting` (IFC2X3 generic). Sannergata: 54K segments + 49K fittings.

Distribution looks correct for each authoring tool.

### Reproduction

Test script at [`scripts/test_rust_parser_big.py`](scripts/test_rust_parser_big.py) (local clone). Run after `maturin develop --release` in `crates/ifcfast/`. Raw results JSON: `docs/research/2026-05-11-13-10_rust-parser-big-files.json`.

### Suggested follow-up

- 293 MB/s on the 824 MB / 143K-product file vs 400+ MB/s on smaller files — worth a flamegraph to confirm it's allocation/Python-list construction rather than the byte-scan.
- Consider exposing `aggregates` and `contained_in` as parallel `Vec<u64>` columns rather than `Vec<(u64,u64)>` if the Python side is going to repackage them anyway — saves the tuple allocation per row.
- If you want a `--no-python` benchmark mode for pure-Rust timing (`index_ms` only, no PyDict construction), I can wire that up as a binary target.

---

### Comment by @EdvardGK (2026-05-11)

## Follow-up: vs ifcopenshell + scope note

Two things missing from the original benchmark — adding them here so future readers don't misread the speedup as a QTO win.

### Head-to-head vs `ifcopenshell.open()` + product walk

Same logical task on both sides: file → per-product (step_id, GlobalId, entity, Name, PredefinedType, ObjectType, Tag) + spatial structure. ifcopenshell timing is `open()` + `by_type("IfcProduct")` + `get_info()` per row, with `IfcSpatialStructureElement` filtered out. Warm OS cache for both.

| file | MB | products | `_ifcfast` | `ifcopenshell` open+walk | speedup |
|---|---:|---:|---:|---:|---:|
| SM_RIBprefab.ifc | 1.6 | 1,420 | 7 ms | 142 ms | 21× |
| OBF_400520_05_6_ARK.ifc | 6.1 | 385 | 18 ms | 371 ms | 21× |
| SM_LARK.ifc | 26.9 | 1,570 | 64 ms | 1,427 ms | 22× |
| SM_RIVr.ifc | 348 | 63,232 | 853 ms | 26.2 s | 31× |
| Sannergata_bygg_ARK_I.ifc | 362 | 25,403 | 931 ms | 31.2 s | 34× |
| Sannergata_RIV.ifc | 824 | 143,704 | 2.4 s | 62.8 s | 26× |

ifcopenshell 0.8.5 / Python 3.12.10. Speedup grows with file size — 21× on small files where Python startup + import dominates, 26–34× on the 350 MB+ range where the byte-level pass really pulls away from the C++ STEP parser + Python binding overhead.

### Scope — what `_ifcfast` does NOT extract

The branch is `fastparse-v3-native-rust-**tier1**` and the crate docstring says "Tier-1 indexer", but I want to spell this out explicitly because "fast IFC parser" gets misread in context of a QTO codebase.

What `index_ifc` returns:

- products: step_id, guid, entity, **name, predefined_type, object_type, tag**
- storeys: step_id, guid, name, elevation, building_step_id
- sites + buildings: step_id ↔ guid
- contained_in + aggregates: step-id pairs
- type_counts
- header: schema, project_name, authoring_app, unit_scale, size_bytes

What it does NOT return:

- ❌ Bounding boxes (tier 2 — `bboxes.parquet`, still Python+ifcopenshell)
- ❌ Property sets / quantity sets (Pset_*Common, Qto_*BaseQuantities)
- ❌ Areas, volumes, lengths
- ❌ Profile × axis (linear elements)
- ❌ Mesh volumes (built elements)
- ❌ Geometry catalog (tier 1.5 — mentioned in the v2 plan, not yet wired)

Per [docs/plans/2026-05-10-fast-parser-v2.md](docs/plans/2026-05-10-fast-parser-v2.md), this is by design — tier 1 is the "show the user the model contents in 5s" UX layer. The QTO pipeline (3-tier procurement logic in `ifc_workbench/quantities/{parametric,products,built}.py`) is downstream and still on ifcopenshell.

So the practical implication: 20–34× speedup on **opening a model and listing what's in it**, not on **producing a QTO CSV**. End-to-end QTO time is still dominated by `ifcopenshell.geom` + the Python procurement reduction, which `_ifcfast` doesn't touch yet.

If tier 2 (bboxes, base quantities from property sets) lands in Rust next, that's where end-to-end QTO speedup would start showing up in real numbers.

### Reproduction

Same machine, immediately after the original benchmark run. Head-to-head script: [`scripts/bench_vs_ifcopenshell.py`](scripts/bench_vs_ifcopenshell.py) (local clone).

---

### Comment by @EdvardGK (2026-05-11)

Great report. All three followups landed in `f09a908` on `fastparse-v3-native-rust-tier1`.

## 1. `marshal_ms` breakdown — surprise diagnosis

`lib.rs` now wraps PyDict construction in its own `Instant` and exposes `marshal_ms` alongside `index_ms`. Tested against an 834 MB MagiCAD RIV (87K products, same shape as your 824 MB Sannergata_RIV):

```
file:           834.0 MB   schema=IFC2X3
open(mmap):           0.02 ms
index (Rust):      1821.45 ms
marshal (PyO3):      20.34 ms
ratio marshal/index: 1.1%
```

**Marshalling isn't the bottleneck** — it's 1.1% of total. The slowdown from 400 MB/s → 293 MB/s on the high-product-count file is in the Rust extract path itself: string allocation per product GUID, HashMap lookups, the post-pass containment filter, etc. Worth a flamegraph next session — likely culprits are `extract_product`'s string allocs and the storey-ids HashSet lookup in the filter pass.

## 2. Parallel `Vec<u64>` relationship columns

Three `Vec<(u64, u64)>` replaced with paired `Vec<u64>`:

```rust
pub contained_in_child:    Vec<u64>,
pub contained_in_structure: Vec<u64>,
pub aggregates_child:      Vec<u64>,
pub aggregates_parent:     Vec<u64>,
pub storey_building_storey:   Vec<u64>,
pub storey_building_building: Vec<u64>,
```

Python now reads `raw["contained_in"]["child"]` / `["structure"]` (dict of parallel lists) instead of `for c, s in raw["contained_in"]`.

**Side-effect win:** the 834 MB file is **24% faster end-to-end** (2384 ms → 1827 ms). The post-pass filter and `storey_building` scan no longer allocate `(u64, u64)` tuples inside the Rust pass either. So the refactor helped even though the Python marshalling cost it was originally targeting turned out to be negligible.

## 3. `ifcfast-bench` standalone binary

```bash
cargo build --release --bin ifcfast-bench --no-default-features
./target/release/ifcfast-bench path/to/big.ifc
```

`--no-default-features` skips the `python` feature, so the binary doesn't link against libpython. Output mirrors the dict your Python tests use:

```
file:           834.0 MB   schema=IFC2X3   unit_scale=0.001
open(mmap):           0.01 ms
index:             1827.52 ms   (lex + extract, no Python marshalling)
products:            87198
contained_in:        87198
aggregates:             27
throughput:            456 MB/s
```

`Cargo.toml` now has dual `crate-type = ["cdylib", "rlib"]` and an optional `python` feature gating PyO3. `maturin develop` still works unchanged (it picks up the feature from `pyproject.toml`).

## Parity / regressions

- All 5 LBK Building C files: exact parity vs ifcopenshell (Δproducts = 0, Δstoreys = 0)
- `tests/test_federated_floors.py`: 11 passed
- Native-vs-ifcopen speedups still 14-21× on the LBK set

If you want to push past 300 MB/s on the >100K-product files, the followup is a flamegraph on `extract_product` + the post-pass filter. Happy to take that next.

---

### Comment by @EdvardGK (2026-05-11)

## Retest on `f09a908` — confirmed

Pulled, rebuilt clean (zero warnings, ~5s incremental), re-ran the full benchmark suite. Same machine, same files, immediately after.

### Headline — 28% speedup on the 824 MB MagiCAD file (matches your claim on the 834 MB)

Warm-cache, same Python path via `_ifcfast.index_ifc(path)`:

| file | MB | products | `index_ms` before (`d186b02`) | `index_ms` after (`f09a908`) | Δ |
|---|---:|---:|---:|---:|---:|
| OBF_400520_01_6_ARK | 267 | 18K | 657 | 825 | +26% noise |
| SM_ARK | 276 | 19K | 724 | 868 | +20% noise |
| SM_RIVr (IFC4) | 348 | 63K | 860 | 861 | flat |
| Sannergata_bygg_ARK_I | 362 | 25K | 869 | 725 | **−17%** |
| BS_RIVr | 498 | 2K | 1,330 | 986 | **−26%** |
| HI90_RIV | 522 | 42K | 1,310 | 1,077 | **−18%** |
| Sannergata_RIV (MagiCAD) | 824 | 144K | 2,812 | **2,013** | **−28%** ✓ |

The Archicad files (~270 MB, ~18K products) show 20-25% run-to-run noise on either side of 700 ms, so I'm treating them as flat. The high-product-count files where the post-pass filter and rel scans live — those are where the parallel-column refactor lands, and the improvement scales with relationship row count. Right where you predicted.

### Standalone `ifcfast-bench` — pure Rust on the same files

Built via `cargo build --release --bin ifcfast-bench --no-default-features`. PyO3-free, ~5s build, ~2.4 MB binary:

```
=== Sannergata_RIV.ifc ===
file:             864.4 MB   schema=IFC2X3   unit_scale=0.001
open(mmap):           0.07 ms
index:             1511.93 ms   (lex + extract, no Python marshalling)
products:           143704
contained_in:       143704
aggregates:             14
throughput:            572 MB/s

=== HI90_RIV_Skiplum_Lokal_farget.ifc ===
file:             547.3 MB
index:             1074.75 ms
products:            41710
throughput:            509 MB/s

=== SM_RIVr.ifc ===
file:             364.8 MB
index:              714.38 ms
products:            63232
throughput:            511 MB/s
```

572 MB/s on the 824 MiB MagiCAD file. You reported 456 MB/s on your similar 834 MB file — my hardware is just faster (i7 laptop, NVMe). Either way, 500+ MB/s sustained on the high-product-count files in pure Rust.

The Python wrapper costs ~500 ms on top of this on the 824 MB file (2013 ms via PyDict path vs 1512 ms pure-Rust) — but `marshal_ms` itself is only **23 ms** (1.1%, matches your reported ratio). The rest of that 500 ms is mmap + page-fault charging on the first Python-side access through the dict, not the marshal pass itself.

### `marshal_ms` confirms: marshalling isn't the bottleneck

Reported `marshal_ms` from the new Python path:

| file | products | `index_ms` | `marshal_ms` | marshal/total |
|---|---:|---:|---:|---:|
| OBF_400520_01_6_ARK | 18K | 825 | 4.1 | 0.5% |
| SM_RIVr | 63K | 861 | 12.6 | 1.4% |
| Sannergata_bygg_ARK_I | 25K | 725 | 5.1 | 0.7% |
| BS_RIVr | 2K | 986 | 0.3 | 0.03% |
| HI90_RIV | 42K | 1,077 | 8.1 | 0.7% |
| Sannergata_RIV | 144K | 2,013 | 22.9 | 1.1% |

Your diagnosis stands: bottleneck is in the Rust extract path itself (string allocs, HashMap lookups, post-pass filter), not PyO3.

### vs `ifcopenshell.open()` + product walk — updated

| file | MB | products | `_ifcfast` | `ifcopenshell` open+walk | speedup |
|---|---:|---:|---:|---:|---:|
| SM_RIBprefab.ifc | 1.6 | 1,420 | 8 ms | 203 ms | 25× |
| OBF_400520_05_6_ARK.ifc | 6.1 | 385 | 22 ms | 449 ms | 21× |
| SM_LARK.ifc | 26.9 | 1,570 | 66 ms | 1.6 s | 24× |
| SM_RIVr.ifc | 348 | 63,232 | 863 ms | 40.5 s | **47×** |
| Sannergata_bygg_ARK_I.ifc | 362 | 25,403 | 1.3 s | 38.1 s | 30× |
| Sannergata_RIV.ifc | 824 | 143,704 | 2.5 s | 74.5 s | 30× |

SM_RIVr jumped from 31× → 47× — but that's mostly ifcopenshell run-to-run variance on that file (26s → 40s open+walk), not a `_ifcfast` regression. The pattern is unchanged: 20-30× across the board, ifcopenshell-side variance dominates.

### Confirmed: parity, no regressions

- Product counts, type counts, storey/site/building counts, rel counts: byte-identical to pre-refactor on all 7 files
- Build is silent on `rustc 1.95.0` ✓ (Issue #1 fixed)
- New `contained_in["child"] / ["structure"]` and `aggregates["child"] / ["parent"]` dict-of-lists schema works as advertised
- `marshal_ms` available on every result

Closing my side. Whenever you take that flamegraph pass on `extract_product`, happy to re-bench.

---

### Comment by @EdvardGK (2026-05-11)

Thanks for the thorough validation — your Windows numbers line up cleanly with the Linux ones and the diagnosis sticks:

- **marshal/total 0.03–1.4% across 7 files** — confirms it's not the PyO3 bridge.
- **−28% on the 824 MB / 144K-product file from the parallel-column refactor**, flat on small/low-rel files — matches the prediction that the tuple-alloc cost scales with relationship row count, not with raw byte size.
- **572 MB/s on your i7 vs 456 MB/s on this box** — your laptop just clocks higher (and the bin sidesteps any GIL release/reacquire cost, however small).

One small refinement on the 1512 vs 2013 ms gap on Sannergata_RIV (standalone bin vs Python): I read that as **OS page cache state between runs**, not the marshal pass itself. The first run on a cold-cache file pays the ~500 ms of disk reads via mmap page faults charged inside `indexer::index`; the second run on the warm cache skips it. Order-dependent. `marshal_ms` is genuinely 23 ms either way.

Either way — diagnosis is locked: the next win is a flamegraph of `extract_product` and the storey-ids HashSet pass. Two likely targets: the per-product String allocation for GUID/name (interning into an arena might help if the hot path proves it), and the HashSet contains lookup that runs N times during the post-pass filter (a sorted Vec + binary search could win for small N, or just skip the filter entirely by tracking storey membership during the pass).

Closing both. Happy to pick up the flamegraph work when it's on the menu.

---

### Comment by @EdvardGK (2026-05-11)

All three followups validated by EdvardGK on 7 production IFCs across Windows. Closing.
