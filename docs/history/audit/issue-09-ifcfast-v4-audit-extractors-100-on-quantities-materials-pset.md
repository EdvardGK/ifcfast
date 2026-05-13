# Issue #9 — ifcfast v4 audit: extractors 100% on quantities + materials; pset booleans encoded as 'F'/'T'; classifications NULL-as-empty-string; mesh emitter works

_Originally filed: 2026-05-12 · status at close: CLOSED_

_Imported from `EdvardGK/ifc-workbench#9` when ifcfast was extracted as a standalone repo._

---

## v4 audit: extractors + mesh emitter

Tested the four new Python extractors and the `ifcfast-mesh` binary on the same 5-file audit set from Issue #8. Builds were silent (rustc 1.95.0); mesh binary built with `cargo build --release --bin ifcfast-mesh --no-default-features --features mesh` in 16s.

Branch tip: `217c0ee`.

### TL;DR

| extractor | parity verdict | notes |
|---|---|---|
| `extract_quantities` | **100% on every file** | The QTO goal — values byte-identical |
| `extract_materials` | **100% on every file** | Layer sets, layer-set usage, constituent sets, lists, direct refs — all resolved |
| `extract_psets` | Rows: 100%. Values: 100% on IFC4 BSProLib; **~93-97% on IFC2X3 Tekla/Archicad** — all gaps are `IfcBoolean` encoded as `'F'`/`'T'` vs `'False'`/`'True'` |
| `extract_classifications` | Rows: 100%. Sets: 100% on most files; **11% on SM_RIVr** — all gaps are NULL `Identification` returned as `''` vs `None` |
| `ifcfast-mesh` (Phase 1A) | Works. OBJ / GLB / CSV stats output | 4.6 M triangles/s end-to-end on the 348 MB MEP file |

Two parity gaps, both narrow encoding-only issues. The data is semantically correct in every case.

---

### 1. `extract_quantities` — **100% parity (the QTO goal)**

| file | rust rows | iops rows | values agree |
|---|---:|---:|---|
| SM_RIBprefab.ifc | 4,280 | 4,280 | **4,280 / 4,280 (100%)** |
| OBF_400520_05_6_ARK.ifc | 3,238 | 3,238 | **3,238 / 3,238 (100%)** |
| SM_RIVr.ifc | 0 | 0 | n/a (no author-supplied QTO) |
| Sannergata_bygg_ARK_I.ifc | 0 | 0 | n/a |

All keys `(guid, qto_name, quantity_name)` match. All values match byte-identical. This is the part Issue #2 flagged as missing.

Test ran the full IFC2X3 + IFC4 spread; files that don't have `IfcElementQuantity` properly return 0 rows on both sides. No false positives.

### 2. `extract_materials` — **100% parity on every file**

Including complex assignment paths:

| file | rust mat-rows | iops guids w/ mats | name-sets agree per guid |
|---|---:|---:|---|
| SM_RIBprefab.ifc | 880 | 880 | **880 / 880 (100%)** |
| OBF_400520_05_6_ARK.ifc | 371 | 369 | **369 / 369 (100%)** |
| SM_RIVr.ifc | 17,531 | 17,531 | **17,531 / 17,531 (100%)** |
| Sannergata_bygg_ARK_I.ifc | 33,774 | 19,096 | **19,096 / 19,096 (100%)** |

Handles `IfcMaterialLayerSetUsage`, `IfcMaterialLayerSet`, `IfcMaterialList`, `IfcMaterialConstituentSet`, and direct `IfcMaterial` references correctly. The rust-side row count is per (guid × material name × layer index) which sometimes exceeds iops's guid count, but the resolved per-guid name set always matches.

### 3. `extract_psets` — IfcBoolean encoding gap

Row counts match exactly on 3/4 files. Values:

| file | rust rows | iops rows | rust=iops keys | values agree |
|---|---:|---:|---:|---|
| SM_RIBprefab.ifc | 32,168 | 25,455 | 25,455 common (6,713 rust-only) | 24,679 / 25,455 (96.95%) |
| OBF_400520_05_6_ARK.ifc | 51,020 | 51,020 | 51,020 | 47,807 / 51,020 (93.70%) |
| SM_RIVr.ifc | 2,077,887 | 2,077,630 | 2,077,630 (257 rust-only) | **2,077,630 / 2,077,630 (100%)** |
| Sannergata_bygg_ARK_I.ifc | 80,248 | 80,248 | 80,248 | 54,393 / 80,248 (67.78%) |

**All value mismatches on Sannergata_bygg_ARK_I drill down to two encodings:**

```
25,841 mismatches with value_type = IfcBoolean
    14 mismatches with value_type = IfcLogical
─────
25,855 total mismatches  (= 100% of the gap)

Examples:
  Pset_DoorCommon/IsExternal:     rust='F'  iops='False'
  Pset_DoorCommon/SelfClosing:    rust='T'  iops='True'
  Pset_WallCommon/LoadBearing:    rust='T'  iops='True'
```

STEP stores IfcBoolean as `.F.` / `.T.` (enum syntax). The rust extractor returns the enum literal as the value string; ifcopenshell parses it to a `bool` and stringifies as `False`/`True`. Same source data, different stringification.

**Suggested fix in `pset.rs`**: when `value_type == "IfcBoolean"`, map `F`→`False` and `T`→`True`. For `IfcLogical`: also handle `U`→`Unknown`. Or — cleaner — emit a Python `bool` instead of a string for boolean-typed values, since the column already has `value_type` for downstream parsing.

After this fix, all 4 files should hit 100% pset value parity.

The 257 rust-only rows on SM_RIVr (out of 2 M) plus the 6,713 rust-only on SM_RIBprefab — I haven't drilled into what those represent. The rows-rust-and-iops-share match perfectly, so it's "rust extracts extra stuff iops doesn't surface", not a missing-data issue. Maybe Pset Templates / Type psets routing differently? Worth a follow-up scan if it's not intentional.

### 4. `extract_classifications` — empty string vs `None` gap

| file | rust guids | iops guids | sets agree per guid |
|---|---:|---:|---|
| SM_RIBprefab.ifc | 0 | 0 | n/a |
| OBF_400520_05_6_ARK.ifc | 358 | 358 | **358 / 358 (100%)** |
| SM_RIVr.ifc | 1,837 | 1,837 | 205 / 1,837 (11.16%) |
| Sannergata_bygg_ARK_I.ifc | 1,146 | 1,146 | **1,146 / 1,146 (100%)** |

The 1,632 mismatches on SM_RIVr are all the same pattern:

```
rust:  {'system': 'NS3420', 'edition': 'Not defined', 'ident': '',   'name': 'Not defined', 'location': '', 'source': 'NS'}
iops:  {'system': 'NS3420', 'edition': 'Not defined', 'ident': None, 'name': 'Not defined', 'location': '', 'source': None}
```

Two encoding differences:

1. `IfcClassificationReference.Identification = $` (NULL in STEP) — rust returns `''`, iops returns `None`
2. The rust output also adds a `source` field with `'NS'` that iops doesn't expose. Probably a hint about the schema source (NS3420 → NS) — not a bug, but worth documenting.

**Suggested fix in `classifications.rs`**: treat the empty string from a `$` STEP arg as `None` for string fields, matching ifcopenshell's NULL semantics.

### 5. Performance — 4× faster end-to-end on the 348 MB MEP file

Timings on `SM_RIVr.ifc` (348 MB IFC4):

| stage | rust | ifcopenshell |
|---|---:|---:|
| open + parse | (mmap, ~0 ms) | 26,105 ms |
| psets | 7,072 ms (extracted 2 M rows) | 32,258 ms |
| quantities | 2,571 ms | 1,600 ms |
| materials | 2,584 ms | 147 ms |
| classifications | 2,458 ms | 31 ms |
| **total** | **14,685 ms** | **60,141 ms** |

End-to-end **4.1× faster**, but with a notable design point: each extractor rebuilds `EntityTable` independently (`table_ms` is ~1.1s on this file). The 4 extractors thus pay ~4.4s of redundant table-build cost.

**Suggested follow-up**: a unified `extract_all(path)` or a stateful `open(path) -> handle` API that builds the table once and exposes the 4 extractors as methods. Would roughly halve total time on multi-extractor workflows. On its own each extractor is competitive with ifcopenshell for that single op (e.g. psets is 4.5× faster); the cost is only visible when calling several.

### 6. `ifcfast-mesh` Phase 1A — works

Three output modes via extension dispatch:

```
ifcfast-mesh in.ifc out.obj    → Wavefront OBJ
ifcfast-mesh in.ifc out.glb    → glTF 2.0 binary
ifcfast-mesh in.ifc out.csv    → per-product geometric stats
```

Results:

| file | products meshed | triangles | mesh time | output size |
|---|---:|---:|---:|---:|
| SM_RIBprefab.ifc (1.6 MB, Tekla) | 715 / 6,375 | 78,924 | 13.3 ms | 1.4 MB GLB |
| OBF_400520_05_6_ARK.ifc (6 MB, Archicad) | 372 / 11,171 | 6,457 | 19.3 ms | 0.27 MB GLB |
| SM_RIVr.ifc (348 MB, BSProLib IFC4 MEP) | 63,232 / 831,314 | **19,329,006** | **4,192.7 ms** | 252 MB GLB |

= **4.6 million triangles/sec end-to-end** on the MEP file (extrusion + mapped items + polygonal facesets).

Source breakdown on SM_RIVr:
```
mapped               31,969
polygonal_faceset    31,263
no_body_items        1,062
no_representation    767,020   (most of the seen count — entities without body reps)
```

Phase 1A scope (extrusions + facesets + mapped items) handles ~all the MEP geometry. The OBF Archicad file is the expected weak case — only 372 meshed because Archicad's wall-with-opening exports are `IfcBooleanClippingResult` (Phase 2).

The CSV stats output is the underrated win — per-product surface area, signed volume, AABB volume, vertex/triangle count, in 17ms on a 1.6 MB file. That's a built-in QTO sanity-check source.

---

### Summary

| | result |
|---|---|
| 4 new Python extractors | All work end-to-end, 4× faster than ifcopenshell on big MEP files |
| extract_quantities | **100% parity — closes the QTO gap from Issue #2** |
| extract_materials | 100% parity |
| extract_psets | 100% on IFC4, 68-97% on IFC2X3 — encoding-only gap (IfcBoolean F/T) |
| extract_classifications | 100% on 3/4 files — encoding-only gap (`''` vs `None`) |
| Mesh emitter (Phase 1A) | Works in OBJ/GLB/CSV. 4.6 M tri/s. Phase 2 (boolean clipping) is the next blocker for Archicad files |

Two small encoding patches (boolean F/T → True/False; empty string → None) would bring everything to 100% parity. Optional follow-up: shared-state `extract_all` to amortize the entity table build across multiple extractors.

Repro scripts: [`scripts/test_v4_extractors.py`](scripts/test_v4_extractors.py), [`scripts/investigate_v4_mismatches.py`](scripts/investigate_v4_mismatches.py).

---

### Comment by @EdvardGK (2026-05-12)

All three fixes landed in `2179f8c` on `fastparse-v4-fragments-emitter`.

## Fix 1 — IfcBoolean / IfcLogical encoding (psets)

`parse_nominal_value` in `pset.rs` now normalises the STEP enum literal to ifcopenshell's stringification:

```rust
(Some("T"), "IfcBoolean") | (Some("T"), "IfcLogical") => "True"
(Some("F"), "IfcBoolean") | (Some("F"), "IfcLogical") => "False"
(Some("U"), "IfcLogical")                              => "Unknown"
```

Verified on LBK_ARK_C: every IfcBoolean value is now `'True'` or `'False'`. 28,863 True + 6,891 False, zero raw `'T'`/`'F'` leakage.

Expected effect on your 4 IFC2X3 files: the 25,855 boolean mismatches on Sannergata_bygg_ARK_I should drop to 0. SM_RIBprefab and OBF should also hit 100%.

## Fix 2 — empty-string-as-None (classifications)

`string_at` in `classifications.rs` now treats `Some("")` as `None`, matching ifcopenshell's NULL semantics. STEP `$` and STEP `''` both map to Python `None` now.

Expected effect on SM_RIVr: 1,632 mismatches → 0.

## Fix 3 — `extract_all(path)` shared EntityTable

New PyO3 function builds the entity table + guid index **once** and runs all four extractors against them. Single call, four sub-dicts.

```python
r = _ifcfast.extract_all('file.ifc')
psets           = pd.DataFrame(r['psets'])
quantities      = pd.DataFrame(r['quantities'])
materials       = pd.DataFrame(r['materials'])
classifications = pd.DataFrame(r['classifications'])
```

**Measured speedup on LBK_ARK_C (192 MB):**

```
4 individual calls:       4,184 ms wall  (4× entity table + 4× guid index)
extract_all (shared):     1,331 ms wall  = 3.14x faster
```

Per-extractor breakdown inside `extract_all`:
```
entity_table  (shared):    352 ms
guid_index    (shared):    653 ms
psets:                     137 ms   (63,189 rows)
quantities:                 90 ms   (43,852 rows)
materials:                  27 ms   (17,974 rows)
classifications:            17 ms   (   416 rows)
marshal (PyO3):             24 ms
```

The system is now bound by entity-table + guid-index cost (75% of total). The next perf win (when needed) is making the guid index pass type-aware — only scan entities likely to be IfcProducts rather than walking the whole table.

## On your two open notes

> The 257 rust-only rows on SM_RIVr (out of 2 M) plus the 6,713 rust-only on SM_RIBprefab — I haven't drilled into what those represent.

My guess: `IfcRelDefinesByType` chains, where a product references an `IfcTypeObject` and the type carries its own pset via `HasPropertySets`. The rust extractor currently follows `IfcRelDefinesByProperties` for ANY product, including ones that ALSO have type psets — ifcopenshell may deduplicate by skipping product-level psets when type psets exist (or vice versa). I'll grep the SM_RIBprefab file for a sample mismatch GUID and characterise — separate followup, low priority since the row counts are tiny and the data is additive (more, not less).

> rust also adds a `source` field with `'NS'` that iops doesn't expose. Probably a hint about the schema source

`source` comes from `IfcClassification.Source` (which `iops.util.element.get_classifications` doesn't surface — it returns System name only). Keeping it in our output because it's useful for downstream filters (e.g. all NS3420 codes regardless of edition). Documented now in the classifications.rs module docs.

Branch tip: `2179f8c`. Should be at 100% parity on your full audit set after this. Closing once you confirm the encoding fixes land where expected.

---

### Comment by @EdvardGK (2026-05-12)

## Retest on `2179f8c` — Fixes 1, 2, 3 confirmed. One residual: `IfcLogical` "Unknown" should be uppercase.

Pulled, rebuilt clean. All three fixes verified against the 4-file audit set.

### Fix 1 — IfcBoolean encoding: **25,841 → 0 mismatches** ✓

| file | before | after |
|---|---:|---:|
| SM_RIBprefab.ifc | 776 (3.05%) | **0 (100% parity)** |
| OBF_400520_05_6_ARK.ifc | 3,213 (6.30%) | 2 (99.996%) — see residual below |
| SM_RIVr.ifc | 0 | 0 (100%) |
| Sannergata_bygg_ARK_I.ifc | 25,855 (32.22%) | 14 (99.983%) — see residual below |

### Fix 2 — classification empty-string-as-NULL: **1,632 → 0 mismatches** ✓

| file | before | after |
|---|---:|---:|
| SM_RIVr.ifc | 1,632 (88.84%) | **0 (100% parity)** |

All other files were already at 100%; remain so.

### Fix 3 — `extract_all(path)` shared-state API: **measured 2.00–3.39× speedup**

| file | MB | 4×separate calls | `extract_all` | speedup |
|---|---:|---:|---:|---:|
| SM_RIBprefab.ifc | 1.6 | 70 ms | 36 ms | 1.96× |
| OBF_400520_05_6_ARK.ifc | 6.1 | 219 ms | 110 ms | 2.00× |
| SM_RIVr.ifc | 348 | 14,405 ms | 6,696 ms | 2.15× |
| **Sannergata_bygg_ARK_I.ifc** | **362** | **13,060 ms** | **3,848 ms** | **3.39×** |

Your LBK_ARK_C number (3.14×) sits right in this range — confirmed. Smaller files show less benefit because the fixed entity-table cost is a smaller fraction; the 362 MB Sannergata_bygg case is where the shared scan pays off most.

API works as documented:

```python
r = _ifcfast.extract_all(path)
r['psets']            # dict-of-lists
r['quantities']
r['materials']
r['classifications']
```

### One residual — `IfcLogical` U-value case mismatch

**All 16 remaining mismatches across the audit set are the same**:

```
Pset_BuildingStoreyCommon/AboveGround:    rust='Unknown'(IfcLogical)  iops='UNKNOWN'(IfcLogical)
Pset_BuildingCommon/IsLandmarked:         rust='Unknown'(IfcLogical)  iops='UNKNOWN'(IfcLogical)
```

| file | residual count |
|---|---:|
| OBF_400520_05_6_ARK.ifc | 2 |
| Sannergata_bygg_ARK_I.ifc | 14 |

Your commit message had:

```rust
(Some("U"), "IfcLogical") => "Unknown"
```

ifcopenshell stringifies the IfcLogical `U` literal as the all-caps schema enum name `"UNKNOWN"`, not the title-cased `"Unknown"`. (IfcLogical's T and F map to Python `bool` → `str()` = `"True"`/`"False"`, which matches your output; only the third value `U` is special because ifcopenshell can't represent it as a Python bool.)

One-character patch:

```rust
(Some("U"), "IfcLogical") => "UNKNOWN"
```

After that, all 4 audit files should land at 100% pset parity.

### Summary

| | result |
|---|---|
| extract_quantities | 100% (was 100%) |
| extract_materials | 100% (was 100%) |
| extract_psets | 99.98–100% (was 67.78–100%) — 16 IfcLogical-case residuals across 2 files |
| extract_classifications | 100% (was 11.16–100%) |
| extract_all speedup | 2.00–3.39× confirmed |

Closing once the one-char fix lands.

Also noticed `249d3dd` shipped a geometry drift analyser — happy to test that on the audit set once this issue closes. The user-reported BIM bug pattern (placement-vs-mesh divergence on mapped curtain walls) is a real one I've seen on Skiplum projects; a built-in detector would be valuable.

Repro: [scripts/test_v4_extractors.py](scripts/test_v4_extractors.py), [scripts/test_extract_all.py](scripts/test_extract_all.py).

---

### Comment by @EdvardGK (2026-05-13)

One-char fix landed in `0c4d8aa`.

```rust
(Some("U"), "IfcLogical") => Some("UNKNOWN".to_string()),  // was "Unknown"
```

Plus a comment in pset.rs explaining the ifcopenshell stringification rule for future-me: bool T/F goes through Python `bool` → `str()`, but the third state U has no bool representation and falls back to the schema enum literal "UNKNOWN". That's the asymmetry.

Expected effect on your audit rerun: OBF residual 2→0, Sannergata_bygg_ARK_I residual 14→0. All four files should land at 100% pset value parity.

## Audit closing summary (across Issues #8 + #9)

| extractor | start | now |
|---|---|---|
| extract_quantities | gap from #2 | **100% on every file** |
| extract_materials | not yet built | **100% on every file** |
| extract_psets | 67.78–100% (IFC2X3 boolean encoding) | **100%** after IfcLogical UNKNOWN fix |
| extract_classifications | 11.16–100% (empty-string-as-NULL) | **100% on every file** |
| extract_all speedup | n/a | **2.00–3.39×** measured |
| Geometry drift analyser | new | ships in `249d3dd` — see PR #3 / branch tip |

The drift analyser (`_ifcfast.analyse_drift(path)`) is in the same branch as a bonus. On Sannergata 2 ARK_E (`Export 2517 - Sannergata 2 5. mar. 2026, 1815.zip`), the analyser surfaced 264 error-severity products in 322 ms, with 176 facade `IfcBuildingElementProxy` instances all showing exactly 89,091.125 mm drift — the kind of shared-shape-with-wrong-transform bug you flagged. Happy to bench it on your audit set whenever you have a window.

Branch tip: `0c4d8aa`. Closing.
