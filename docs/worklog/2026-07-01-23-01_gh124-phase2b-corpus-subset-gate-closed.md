## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `f88f15e` → `4922bce` (1 commit this session)
- **Session scope**: Close the GH #124 Phase 2b full-corpus subset acceptance gate on real discipline-diverse G55 IFCs.
- **Touched paths**: `tests/test_subset.py`
- **Parallel sessions observed**: none (no commits on origin/main during the window)
- **Supersedes / superseded by**: none

## Summary
Closed the last open item on GH #124 Phase 2b (the `m.subset(guids)` write
primitive): the full-corpus acceptance gate Ed required before trusting the
writer. Pulled 4 discipline-diverse G55 IFCs from ACC and ran three gates —
Rust rel-field pinning, Rust subset closure, and a **new durable pytest
harness** — all green with ifcopenshell confirming 0-dangling + rooted
IfcProject on every emitted subset. The writer's untested rel branches
(notably 548 real `IfcRelFillsElement` in ARK) are now validated against real
Revit/MagiCAD output, not just synthetic fixtures.

## Changes
- `tests/test_subset.py`: added `test_subset_over_real_corpus_is_ifcopenshell_clean`
  — a corpus-gated (`IFCFAST_SUBSET_CORPUS=a.ifc:b.ifc`) pytest that drives the
  real Python `Model.subset()` over each file, seeds ~200 product GUIDs spread
  across storeys, writes the subset, reopens in ifcopenshell, and asserts zero
  dangling refs + exactly one rooted IfcProject. Folds the old scratchpad
  `validate_subset.py` into the suite as a `_oracle_check` helper. Skips
  cleanly when the env var is unset (`[NOTSET]` placeholder).
- Committed `4922bce`, pushed to main (autonomous per project convention).
- Filed **GH #126** for a completeness gap surfaced by the sweep.

## Technical Details
- **Corpus source**: ACC → Skiplum Backup project (`b.bdb8892d…`) →
  `10027 - Grønland 55` / `1. IFC`. Downloaded G55_ARK.ifc (170MB, v128),
  G55_RIV.ifc (130MB, v164), G55_RIE.ifc (43MB, v149) into `scratch/g55/`
  (gitignored). Local G55_RIB.ifc (2.8MB) already present.
- **Three gates, all green across 4 disciplines**:
  1. `cargo test -p ifcfast-core --no-default-features --test doc_rel_rules -- --ignored`
     with `IFCFAST_CORPUS=…` — every anchor/pull field index resolves on real
     records. Confirmed **548 IfcRelFillsElement** (ARK), 4058 CoversBldgElements
     (RIV), 1304 VoidsElement (ARK), etc.
  2. `--test doc_subset -- --ignored subset_across_corpus` with `IFCFAST_SUBSET_DIR`
     — 0-dropped-deps, wrote subsets to `scratch/subsets/`.
  3. New pytest — drives the Python surface end-to-end. Results: RIB 269 seeds→56038
     inst, ARK 201→28039, RIV 202→29708, RIE 203→134155; all 0 dangling, proj=1.
- **Build gotchas (unchanged, reconfirmed)**: `--no-default-features` for doc
  tests (dodges pyo3 link); `unset CONDA_PREFIX` before pytest/cargo; corpus
  tests need **absolute** paths (test binary CWD ≠ workspace root — relative
  paths failed NotFound first attempt).
- **Coverage note**: G55 is all-Revit → emits **no** IfcRelNests / IfcRelDeclares /
  IfcDistributionPort anywhere. Those subset rel branches remain
  synthetically-covered (via `pins_every_rule_field_index_exactly`) only —
  not real-corpus-validated. RIE was pulled specifically to chase Nests/ports
  and came up empty; documented, not a blocker.

## Next
- **Phase 3 mesh-hotswap** (the #124 north-star payload): repoint
  `IfcShapeRepresentation.Items` → new `IfcTriangulatedFaceSet`,
  new-id = max_id+1, inverse-index for orphan GC.
- **GH #126**: add subset anchor/prune rules for `IfcRelCoversBldgElements`
  (4058 in RIV), `IfcRelServicesBuildings`, `IfcRelConnectsPathElements` +
  pinning fixtures. Small; corpus is already downloaded and hot.
- Then QTO cluster: #123 (degenerate open-shell prism over-count), #62
  (windows +482%), #119/#120 (bounded-halfspace slivers).

## Notes
- No blockers. Writer is now corpus-gated and trustworthy for the rel types
  Revit actually emits.
- `scratch/g55/*.ifc` (343MB) + `scratch/subsets/` are gitignored — safe, but
  present on this tree if the next session wants to re-run gates without
  re-downloading from ACC.
- GH #126 also records that `IfcRelServicesBuildings` was a *conscious* Phase-2b
  deferral (anchor=building is always kept via spine → would drag every system
  into a one-wall subset; needs member-anchoring), per the writer memory.
