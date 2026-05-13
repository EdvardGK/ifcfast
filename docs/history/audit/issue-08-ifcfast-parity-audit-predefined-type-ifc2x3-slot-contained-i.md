# Issue #8 — ifcfast: parity audit — predefined_type IFC2X3 slot, contained_in misses 33 walls on Archicad, aggregates missing IfcSite→IfcProject

_Originally filed: 2026-05-12 · status at close: CLOSED_

_Imported from `EdvardGK/ifc-workbench#8` when ifcfast was extracted as a standalone repo._

---

## Parity audit: 3 correctness findings from a full per-field + relationship-graph diff vs ifcopenshell

Ran a comprehensive per-product + per-relationship parity check against `ifcopenshell.open()` on 5 diverse files (Tekla / Archicad / Revit-IFC4 / Revit-IFC2X3 / MagiCAD), 1.6 MB → 824 MB, 234,144 products total.

**What's perfect:**

- Product GUID set: byte-identical on every file (zero rust-only, zero iops-only across all 234,144 products)
- Per-product `entity`, `name`, `object_type`, `tag`: 100% on every file
- Storeys / sites / buildings: GUID sets, names, elevations all match exactly (including the multi-site Sannergata_bygg_ARK_I edge case — both report 6 sites)
- `contained_in` on the 3 largest files (SM_RIVr, Sannergata_bygg_ARK_I, Sannergata_RIV): every relationship matches

Three real findings below. Each is reproducible from [`scripts/full_parity_check.py`](scripts/full_parity_check.py) + [`scripts/investigate_mismatches.py`](scripts/investigate_mismatches.py).

---

### 1. `predefined_type` reads a fixed slot regardless of IFC2X3 schema

The IFC2X3 schema doesn't have `PredefinedType` at the same field-position for several entities; the rust parser reads the slot anyway and returns whatever's there. ifcopenshell, knowing the schema, returns `None`.

| entity | what's actually in that slot (IFC2X3) | rust returns | iops returns | count in test set |
|---|---|---|---|---:|
| `IfcReinforcingBar` | `BarRole` (NOTDEFINED/MAIN/SHEAR/…) | `'NOTDEFINED'` | `None` | 104 (SM_RIBprefab) |
| `IfcStair` | `ShapeType` (STRAIGHT_RUN_STAIR/HALF_TURN_STAIR/…) | `'HALF_TURN_STAIR'` | `None` | 41 (Sannergata_bygg_ARK_I) |
| `IfcRamp` | `ShapeType` | `'NOTDEFINED'` | `None` | 6 (Sannergata_bygg_ARK_I) |
| `IfcDistributionPort` | `FlowDirection` (SOURCE/SINK/…) | `'SOURCE'` | `None` | 15 (Sannergata_bygg_ARK_I) |

100% parity on IFC4 (SM_RIVr, IFC4 schema standardised `PredefinedType` in these positions). Worst hit: SM_RIBprefab Tekla rebar file — 7.3% mismatch (104/1420).

**The value rust reads is real** — `'HALF_TURN_STAIR'` *is* what's in the file at that position; it's just labeled `ShapeType` in IFC2X3, not `PredefinedType`. So the field's content isn't wrong, but the column name is misleading for IFC2X3.

Two reasonable fixes:
- Rename the output column to something schema-neutral (`predefined_type_or_role`) and document the IFC2X3 semantics
- For IFC2X3, gate the slot per entity and return `None` for entities that don't define `PredefinedType` at that position

---

### 2. `contained_in` misses `IfcWall`/`IfcSlab`/`IfcGrid` children on smaller Archicad/Tekla files

| file | rust | iops | iops-only | missing-child breakdown |
|---|---:|---:|---:|---|
| SM_RIBprefab.ifc (1.6 MB) | 483 | 486 | 3 | 3× `IfcGrid` |
| OBF_400520_05_6_ARK.ifc (6.1 MB) | 325 | **358** | **33** | 32× `IfcWall`, 1× `IfcSlab` |
| Sannergata_bygg_ARK_I.ifc (362 MB) | 20,822 | 20,822 | 0 | — |
| SM_RIVr.ifc (348 MB) | 63,232 | 63,232 | 0 | — |
| Sannergata_RIV.ifc (824 MB) | 143,704 | 143,704 | 0 | — |

The large MEP files have very simple containment (every product → exactly one storey, no decomposition trees), so they're 100%. OBF has 33 walls/slabs that ifcopenshell sees as `IfcRelContainedInSpatialStructure` children but rust doesn't capture.

I haven't fully diagnosed this yet — could be:
- A second `IfcRelContainedInSpatialStructure` record referencing the same parent storey is being dropped (only the first is kept), **or**
- The wall/slab `step_id` isn't resolvable at the moment the rel is processed (defined later in the file?), **or**
- The wall is a member of `IfcRelAggregates` first and the containment is recorded on the assembly — rust may only record one or the other

The fact that **`IfcWall`s** show up here (32 of them) is the surprising part — they're definitely in `PRODUCT_TYPES`. Repro: run `investigate_mismatches.py` against OBF_400520_05_6_ARK.ifc.

---

### 3. `aggregates` is off-by-1 on every file — `IfcSite → IfcProject` is always missing

| file | rust | iops | missing pair |
|---|---:|---:|---|
| SM_RIBprefab.ifc | 936 | 937 | `IfcSite(2pxTI_$1H1xRnu1ZB2KTcj) → IfcProject(14ElMGiNHAEuojJy7oRiea)` |
| OBF_400520_05_6_ARK.ifc | 29 | 31 | `IfcSite(...) → IfcProject(...)` + `IfcSpace(...) → IfcBuildingStorey(...)` |
| SM_RIVr.ifc | 15 | 16 | `IfcSite → IfcProject` |
| Sannergata_bygg_ARK_I.ifc | 3,147 | 3,148 | `IfcSite → IfcProject` |
| Sannergata_RIV.ifc | 13 | 14 | `IfcSite → IfcProject` |

The rust parser tracks `step_id → guid` for products, storeys, sites, and buildings. **`IfcProject` is missing from that resolver**, so any `IfcRelAggregates` row where the parent is the project gets silently dropped during translation.

OBF additionally misses one `IfcSpace → IfcBuildingStorey` rel — same root cause, `IfcSpace` also isn't in the resolver.

Easy fix: extend the step_id resolver to cover `IfcProject` and `IfcSpace`.

---

### Why this matters

Throughput is excellent (Issue #2/#5). What this audit shows is that the parser's **product and spatial-structure outputs are byte-identical to ifcopenshell**, but the **relationship graph and `predefined_type` have small but real gaps**. Anything downstream that walks the spatial tree from the project root (e.g. "find all elements aggregated under any site of this project") will silently lose data on every file today.

Finding 1 is a documentation / column-rename call.
Findings 2 and 3 are straightforward parser fixes — small allocators and resolver-table additions, no architectural change needed.

### Reproduction

```bash
python scripts/full_parity_check.py        # full diff, prints parity table per file
python scripts/investigate_mismatches.py   # drills into per-entity breakdowns
```

Both scripts use the venv from `crates/ifcfast/` and just need `ifcopenshell` installed (`pip install ifcopenshell`).

---

### Comment by @EdvardGK (2026-05-12)

Findings 1 and 3 landed in `8782584` on `fastparse-v3-native-rust-tier1` (tip now `d3dbb72`).

## Finding 3 — IfcProject + IfcSpace added to step_id resolver

`IndexedFile` now carries:
```rust
pub project_step_id_to_guid: HashMap<u64, String>,
pub space_step_id_to_guid:   HashMap<u64, String>,
```

Exposed as `raw["projects"]` and `raw["spaces"]` (parallel `step_id` / `guid` lists, same shape as `sites` and `buildings`). Python `_open_fast_native` extends the unified `parent_step_to_guid` map with both before resolving `IfcRelAggregates` parents.

Expected effect on your audit table:
- Every file's `IfcSite → IfcProject` rel now resolves
- OBF's `IfcSpace → IfcBuildingStorey` rel now resolves (the wait, that one's child=Space not parent=Space — Space showing as the *child* in an IfcRelAggregates rel means the rel's RelatingObject is the storey; storeys are already resolved. If you can confirm OBF's missing rel #2 is `IfcSpace → IfcBuildingStorey` with Space as child, that's actually a containment vs aggregate question worth a separate look.)

## Finding 1 — IFC2X3 `predefined_type` slot suppression

`extract_product` now takes `is_ifc2x3: bool` and short-circuits the trailing-enum extraction for entities where the IFC2X3 trailing slot carries a different attribute:

```rust
fn is_predefined_type_unavailable_in_ifc2x3(entity: &[u8]) -> bool {
    matches!(entity,
        b"IFCREINFORCINGBAR"      // BarRole
        | b"IFCSTAIR"             // ShapeType
        | b"IFCRAMP"              // ShapeType
        | b"IFCDISTRIBUTIONPORT"  // FlowDirection
    )
}
```

For IFC4 files: unchanged behaviour (PredefinedType IS in the trailing slot for these entities).
For IFC2X3 files: returns `None`, matching ifcopenshell's schema-aware extraction.

Expected effect on your audit:
- SM_RIBprefab (Tekla IFC2X3, 1420 IfcReinforcingBar): mismatch should drop from 7.3% to 0%
- Sannergata_bygg_ARK_I (Archicad IFC2X3): 41 IfcStair + 6 IfcRamp + 15 IfcDistributionPort mismatches eliminated
- Other 4 files: no change (no affected entity types or IFC4 schema)

## LBK validation re-run after fixes

100%/100% held; cold parse 158 s (consistent with prior runs).
docs/research/2026-05-12-11-55_ito-validation.md for the full table.
predefined_type on LBK stays at 99.9% because none of the LBK files contain the 4 affected entity types — the fix is verifiable on your Tekla rebar file.

## Finding 2 — needs your help to repro

Without the OBF_400520_05_6_ARK.ifc file I can't reproduce the missing 32 walls + 1 slab. Three diagnostic asks if you have a window:

1. **The 33 missing GUIDs** — run a one-liner that prints what their RelatingStructure entity type is on the ifcopenshell side. If those walls are contained directly in `IfcBuilding` rather than `IfcBuildingStorey`, our post-pass filter (which keeps only storey-relating containment) would correctly drop them — and your iops counter would also need to filter consistently to compare apples to apples.

2. **A few sample step_ids** for the missing walls — could you drop ~5 lines from the IFC for one of them? Like:
```bash
grep -A 0 'GUID_OF_MISSING_WALL' OBF_400520_05_6_ARK.ifc
# And the RelContainedInSpatialStructure record that mentions it:
grep -E 'IFCRELCONTAINEDINSPATIALSTRUCTURE.*<step_id>' OBF_400520_05_6_ARK.ifc
```

3. **Multi-record-per-storey?** If OBF has multiple `IfcRelContainedInSpatialStructure` records all pointing at the same storey, our lexer captures each independently — but if the lists across them are very long (>1000 refs), I'd want to spot-check that `parse_ref_list` isn't hitting a corner case with Archicad's multi-line `(` formatting.

If you can share the 6.1 MB OBF file (or even just a small reduction of it that reproduces the miss), I can run the investigation locally. Otherwise the three diagnostic outputs above would let me find the bug from here.

Leaving Issue #8 open until Finding 2 is closed too.

---

### Comment by @EdvardGK (2026-05-12)

## Retest after `8782584` — Findings 1+3 closed. Finding 2 was a false positive (documented filter). One new IFC2X3 entity to add.

Pulled, rebuilt (silent), re-ran the parity script. Both fixes confirmed; Finding 2 root-cause investigation answered your three diagnostic questions and turned out to be the existing `contained_in` filter behaving as designed.

### Finding 1 — confirmed fixed (4/5 files now 100%)

| file | predefined_type before | after |
|---|---:|---:|
| SM_RIBprefab.ifc | 92.68% (104 IfcReinforcingBar) | **100%** ✓ |
| OBF_400520_05_6_ARK.ifc | 100% | 100% |
| SM_RIVr.ifc | 100% | 100% |
| Sannergata_bygg_ARK_I.ifc | 99.76% (41 stair + 6 ramp + 15 distport) | **100%** ✓ |
| Sannergata_RIV.ifc | 99.45% | 99.45% (see below) |

### Finding 3 — confirmed fixed (all 5 files 100%)

`aggregates` is now an exact match on every file. `IfcSite → IfcProject` resolves correctly. OBF's `IfcSpace → IfcBuildingStorey` rel also resolves now.

| file | aggregates rust | iops | match |
|---|---:|---:|---|
| SM_RIBprefab.ifc | 937 | 937 | ✓ |
| OBF_400520_05_6_ARK.ifc | 31 | 31 | ✓ |
| SM_RIVr.ifc | 16 | 16 | ✓ |
| Sannergata_bygg_ARK_I.ifc | 3,148 | 3,148 | ✓ |
| Sannergata_RIV.ifc | 14 | 14 | ✓ |

---

### Finding 2 — false alarm. The filter is documented, my parity check was wrong.

I drilled into all three of your diagnostic asks and tracked it down to `crates/ifcfast/src/indexer.rs:394-405`:

```rust
// Filter `contained_in` to storey-relating only. fastparse's existing
// Python tier-1 walk applies the same filter (it's the assumption the
// storey_lookup map encodes). We do it here so the Python side can
// use `contained_in` as a flat (child, storey_step_id) array pair.
let storey_ids: HashSet<u64> = out.storey_step_id.iter().copied().collect();
```

Diagnostic results that confirm this:

**OBF — 33 missing rels: ALL of them go to an `IfcSpace` parent.**

```
DIAGNOSTIC 1 — RelatingStructure entity types of missing rels:
  parent type IfcSpace: 33 missing rels
              child entity types:
              IfcWall: 32   IfcSlab: 1
  Missing rels belong to 1 distinct IfcRelContainedInSpatialStructure record (#4765)

Raw STEP record:
  #4765=IFCRELCONTAINEDINSPATIALSTRUCTURE('00$OJWd3...', #2, $, $,
    (#2672, #2677, ..., #2779), #2667);   ← #2667 is the IfcSpace
```

**SM_RIBprefab — 3 missing rels: ALL of them go to an `IfcBuilding` parent.**

```
DIAGNOSTIC 1:
  parent type IfcBuilding: 3 missing rels
              child entity types:
              IfcGrid: 3
  Missing rels: 1 IfcRelContainedInSpatialStructure record (#20910)

Raw STEP record:
  #20910= IFCRELCONTAINEDINSPATIALSTRUCTURE('2IhnP9sJ...', #5, $, $,
    (#20427, #20410, #20393), #43);   ← #43 is the IfcBuilding
```

**Diagnostic 3 — multi-record-per-storey: not applicable.** Both files have exactly one rel record per storey.

**Verdict**: rust is doing exactly what the comment at `indexer.rs:398` says — storey-relating only. Drops `IfcSpace`-relating and `IfcBuilding`-relating containment by design. My check was comparing rust's filtered `contained_in` against ifcopenshell's *unfiltered* `IfcRelContainedInSpatialStructure` walk. Apples vs apples-plus-oranges. Closing this part — not a bug.

One small follow-up worth a sentence in the SPEC.md (if it's not there already): downstream consumers should know that an element directly contained in an `IfcSpace` or `IfcBuilding` (rather than an `IfcBuildingStorey`) will not appear in `contained_in`. The 33 walls in OBF's "Plan U1" space + the 3 grids at building level are the kind of cases that go silent.

---

### New finding 1.5 — `IfcBuildingElementProxy` is IFC2X3-affected too (791 mismatches on Sannergata_RIV)

The fix in `8782584` covers `IfcReinforcingBar`, `IfcStair`, `IfcRamp`, `IfcDistributionPort`. There's a fifth entity with the same problem that the audit table missed:

```
=== Sannergata_RIV.ifc  predefined_type residual ===
Mismatches by entity type:
  IfcBuildingElementProxy            791  example 1IxI_cDWj32h7$TFsgii1k:
                                          rust='NOTDEFINED'  iops=None
```

In IFC2X3 `IfcBuildingElementProxy`:
- inherits `CompositionType` (ELEMENT/COMPLEX/PARTIAL) from `IfcSpatialStructureElement`-adjacent inheritance — *not* a `PredefinedType`
- IFC4 added `PredefinedType` (ELEMENT/PARTIAL/PROVISIONFORVOID/USERDEFINED/NOTDEFINED) at the same slot, which is why IFC4 files behave correctly

Sannergata_RIV is the MagiCAD IFC2X3 file we already bench against. 791 of its 791 `IfcBuildingElementProxy` products trigger this.

Suggested patch in `is_predefined_type_unavailable_in_ifc2x3`:

```rust
fn is_predefined_type_unavailable_in_ifc2x3(entity: &[u8]) -> bool {
    matches!(entity,
        b"IFCREINFORCINGBAR"
        | b"IFCSTAIR"
        | b"IFCRAMP"
        | b"IFCDISTRIBUTIONPORT"
        | b"IFCBUILDINGELEMENTPROXY"  // CompositionType, not PredefinedType
    )
}
```

After that, Sannergata_RIV should hit 100% `predefined_type` parity too.

Worth a quick scan of other entities for the same trap: `IfcAnnotation`, `IfcVirtualElement`, `IfcCovering` (Covering Type in IFC2X3?), and `IfcSpaceHeater` family — anything where IFC4 added `PredefinedType` at a slot IFC2X3 used for something else. The current audit set doesn't have enough authoring-tool diversity to surface those, but a synthetic file with one of each entity would.

### Repro

```bash
python scripts/full_parity_check.py        # 4/5 files: all-green
python scripts/diagnose_finding2.py        # answers all 3 of your diagnostics on OBF + SM_RIBprefab,
                                            # plus drills the residual Sannergata_RIV predefined_type
```

Once IfcBuildingElementProxy lands in the suppression list, the parity audit should be 100% across the board on tier-1 fields and storey-relating rels.

---

### Comment by @EdvardGK (2026-05-12)

Thanks for the deep dive on Finding 2 — that's a clean false-positive trace. Both halves of your latest comment landed in `001e7a4`.

## Finding 1.5 — `IfcBuildingElementProxy` added to IFC2X3 blacklist

```rust
fn is_predefined_type_unavailable_in_ifc2x3(entity: &[u8]) -> bool {
    matches!(entity,
        b"IFCREINFORCINGBAR"          // BarRole
        | b"IFCSTAIR"                 // ShapeType
        | b"IFCRAMP"                  // ShapeType
        | b"IFCDISTRIBUTIONPORT"      // FlowDirection
        | b"IFCBUILDINGELEMENTPROXY"  // CompositionType  ← new
    )
}
```

Verified the suppression triggers correctly (zero IfcBuildingElementProxy entries with non-null predefined_type on any IFC2X3 file in the local LBK set). Sannergata_RIV's 791 mismatches should now land at 0 — that's your call to confirm on Windows.

## Finding 2 — documented in SPEC.md

Added a `Containment scope` subsection to `ifc_workbench/fastparse/SPEC.md` directly under the `index.parquet` schema table:

> `storey_guid` only carries values for elements whose `IfcRelContainedInSpatialStructure.RelatingStructure` is an `IfcBuildingStorey`. Elements contained **directly** in an `IfcSpace` (e.g. Archicad walls bordering a labelled space), in an `IfcBuilding` (e.g. building-level grids), or in an `IfcSite` are intentionally **null** here. The decomposition tree to find their non-storey container lives in `parent_guid` + IfcRelAggregates chains, not in the storey index.

Plus the full IFC2X3 `predefined_type` suppression table in SPEC.md so downstream consumers know the rule, not just the empirical behaviour.

## Other entities worth scanning for the same trap

Your suggestion to scan `IfcAnnotation`, `IfcVirtualElement`, `IfcCovering`, `IfcSpaceHeater` family is right — but I don't have a Bonsai-emitted or synthetic IFC2X3 file with one of each on hand, and reasoning from the schema alone is brittle (e.g. `IfcCovering` actually carries `PredefinedType` in BOTH schemas, just with different enum types, so it's a non-trap). The pragmatic move: as you've been doing, run the parity script against each new project's authoring-tool diversity and flag whichever entity surfaces — additive entries to the blacklist are cheap. Synthetic test file with one of each entity is a separate task worth a follow-up issue when it becomes blocking.

## Branch state

`fastparse-v3-native-rust-tier1` now at `001e7a4`. With Findings 1 + 1.5 + 3 all fixed and Finding 2 documented, tier-1 should be at 100% parity across your full 5-file audit set. Closing this issue once you confirm Sannergata_RIV's predefined_type lands at 100%.

---

### Comment by @EdvardGK (2026-05-12)

Confirmed on `001e7a4`. Sannergata_RIV `predefined_type` mismatch is gone — full audit is now clean.

### Final parity table — 5 files, 234,144 products, 1.17 M field comparisons, all match

| file | products | entity | name | predefined_type | object_type | tag | aggregates |
|---|---:|---:|---:|---:|---:|---:|---|
| SM_RIBprefab.ifc | 1,420 | 100% | 100% | **100%** | 100% | 100% | 937/937 |
| OBF_400520_05_6_ARK.ifc | 385 | 100% | 100% | 100% | 100% | 100% | 31/31 |
| SM_RIVr.ifc | 63,232 | 100% | 100% | 100% | 100% | 100% | 16/16 |
| Sannergata_bygg_ARK_I.ifc | 25,403 | 100% | 100% | 100% | 100% | 100% | 3,148/3,148 |
| Sannergata_RIV.ifc | 143,704 | 100% | 100% | **100%** | 100% | 100% | 14/14 |

`contained_in` matches the filtered (storey-relating only) ifcopenshell baseline exactly on every file. The 33-walls-in-IfcSpace + 3-grids-in-IfcBuilding cases are explained by the new SPEC.md containment-scope note — downstream consumers can find those via `parent_guid` + IfcRelAggregates as documented.

Agreed on the "additive blacklist as new authoring tools surface" approach. The synthetic-IFC-with-one-of-each-entity test fixture would catch the long tail of IFC2X3 schema traps in one shot — happy to file that as a follow-up issue if/when it becomes blocking, but for now the parity script + real authoring-tool diversity has been doing the job.

Closing.

---

### Comment by @EdvardGK (2026-05-12)

Closing per your "Closing" mark on the previous comment.

**Final tier-1 parity audit result: 5 files, 234,144 products, 1.17 M field comparisons — 100% match.** Aggregates match exactly on every file. `contained_in` matches the storey-filtered baseline; the IfcSpace/IfcBuilding-relating cases are documented in SPEC.md.

Synthetic-IFC-with-one-of-each-entity test fixture flagged as a future follow-up; agreed on the additive-blacklist pragmatic approach for now.

Branch `fastparse-v3-native-rust-tier1` at `001e7a4` ready for PR #3 merge whenever it suits.
