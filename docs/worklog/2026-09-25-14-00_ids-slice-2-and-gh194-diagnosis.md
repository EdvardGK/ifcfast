## Agent signature
- **Agent**: `claude-fable-5-1` (coordinator) with opus sub-agents (2A refactor, semantics spec, 2B facets, #194 diagnosis)
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `7792bdc` → `cc5d9b2` (commits: `d8bf1d1` slice 2A, `cda4f78` slice 2B, `93669e6` worklog, `c843201` csg-smoke fix, `cc5d9b2` worklog CI note)
- **Session scope**: IDS slice 2 (Property / Classification / Material facets, PropertyGraph + UnitTable) shipped to main; GH #194 diagnosed (contract, not math)
- **Touched paths**: `crates/core/src/extractors/{property_graph.rs (new),psets.rs,quantities.rs,materials.rs,classifications.rs,mod.rs}`, `crates/core/src/units.rs` (new), `crates/core/src/indexer.rs`, `crates/core/src/lib.rs`, `crates/core/src/ids/{graph.rs (new),eval,compile,report,restriction,candidates,mod,schema_tables}.rs`, `crates/core/tests/ids_eval.rs`, `crates/core/tests/fixtures/ids/props_units.ifc`, `.gitignore`, `scripts/{gen_schema_tables.py,dump_tables_ab.py (new)}`, `python/ifcfast/{ids.py,model.py}`, `AGENTS.md` + `python/ifcfast/data/AGENTS.md`, `docs/ids/{facet-semantics-slice2.md (new),ambiguities.md}`, `docs/plans/2026-09-24_ids-validation-design.md`
- **Parallel sessions observed**: GH #194 filed 2026-09-24 15:54 by Ed from the edkjo box (signed "edkjo, revit-plugin/personal"); no commits on origin/main from others
- **Supersedes / superseded by**: none

## Summary

**Slice 2A — shared substrate (`d8bf1d1`).** `extractors::property_graph::PropertyGraph`
is the typed truth (list boundaries, IfcValue wrappers, Unit refs, material property sets
incl. IFC2X3 `IfcExtendedMaterialProperties`); `psets::build` and `quantities::build` now
emit their tables FROM it. `crate::units::UnitTable` resolves every unit assignment entry
(SI prefix^dim, conversion-based recursive, derived products, offset units carry offset,
monetary/context = NoScale); the indexer's length scale is routed through it with a
test-only legacy oracle. Four latent extractor bugs surfaced and were filed, not changed:
#195 `value_type` casing, #196 type-own psets missing, #197 nested/offset length units,
#198 `unit_step_id` fallback rule.

**Facet semantics spec** (`docs/ids/facet-semantics-slice2.md`): rule → deciding suite case
→ IfcTester line → status for all 174 property/classification/material/tolerance cases.
Coordinator decisions recorded in `ambiguities.md`: A14 (type-inherited values get
dataType/unit checks), A16 (complex/reference = absent, `PROP_UNSUPPORTED`), A26 (unresolved
unit stays a typed error via `on_unsupported`, never a fabricated fail), D8 (bound
restrictions hold for ALL values), `real_eq` widened 1 ulp (two suite pass cases needed it).

**Slice 2B — facets (`cda4f78`).** `ids/graph.rs` lazy data layer; property (pset name
over PropertySet + ElementQuantity + material psets, bounded = any of L/U/SetPoint,
list/enum = any, table columns by dataType, D8, A14, A16, SI conversion via UnitTable with
the property's own Unit else project unit), classification (system via ReferencedSource
chain, parent codes count, no prefix match, direct IfcClassification, per-system type
override), material (any of set name / layer / constituent / profile / material Name or
Category, usage → set, type inheritance). Reason codes now emitted: all but `PARTOF_*`.
Follow-ups filed: #199 IfcQuantityNumber, #200 CamelCase dataType labels for slice-4 parity.

**GH #194 diagnosed, not fixed (Ed's call pending).** The clip math is right; default
`mesh()`/`meshes()` are reveal-all no-cut and strip half-space cutters (GH #66), so
`IfcBooleanClippingResult` walls come back unclipped; `cut_openings=True` (= `mesh_qto`
default) matches ifcopenshell within 1e-4 at chain depth 1/2/3. Consequences: substrate →
`clash()` sees unclipped walls; wasm too; `mesh_qto(cut_openings=False)` flags
`volume_reliable=True` on 18/38 RIB clipped products that are wrong by > 0.1 % (two by
> 85 %); the oracle gate only sweeps cut mode. Proposed contract change (apply half-space
clips in every mode inside `boolean_result`; solid differences + voids stay under
`cut_openings`) vs stopgap (`volume_reliable=False` when cutters stripped) posted on #194.
Fixture `scratch/g194/clip_single_pbhs.ifc`. Separate unattributed −4 % residue on two RIB
beams in both modes.

## Evidence

| Gate | Result |
|---|---|
| Parquet A/B `scripts/dump_tables_ab.py` (psets, quantities, materials, classifications, unit_scale) on 8 models (4×G55, 2×clinic, KNM_RIB, quantities fixture) | **BITWISE IDENTICAL** after 2A and after 2B |
| Rust column-equality on 25 fixtures + 30 real files (2A, temporary harness) | identical |
| Length-scale legacy oracle | 56 files, bit-equal + identical warnings |
| Rust `ids_eval`, 9 folders | 334 cases, **0 mismatches**; only partof (34) Unsupported |
| Python harness both engines | **278 green, 34 unsupported_facet, 0 ifcfast_bug**, 22 ifctester_bug (ifcfast agrees with truth on every one) |
| `cargo clippy --all-targets -D warnings`, `cargo fmt --check` | clean |
| `cargo test -p ifcfast-core` | 24 targets, 552 passed, 0 failed |
| quick pytest (harness, parse differential, agent surface, guide, drift, mcp) | 676 passed, 13 skipped |
| `pytest tests/` with G55 corpus | 727 passed, 3 skipped (debug build, 82 min) |
| CI on `c843201` | `ci` + `csg-smoke` green (runs 36181555909, 36181555826); first push `93669e6` failed csg-smoke on dead_code for two IDS-only fields in a `--no-default-features --features csg` build → `c843201` cfg_attr gate |

## Ledger
No spruceledger node changed an action this session.

## Next
1. **Ed decides #194**: contract change (recommended; needs pure-Rust bounded cut for wasm,
   cache bump, full gate incl. new no-cut sweep + 5 clash rounds) or stopgap flag. Snowdon
   Towers IFC lives on the edkjo box if the 107-wall check is wanted.
2. **Slice 3** (#192): PartOf + IfcRelNests / group / fills capture on the indexer,
   `m.nests` / `m.groups`, cache schema 34 → 35. Then bundle slices 1–3 into **v0.6.0**.
3. Slice 4: CLI / MCP / wasm / `to_ifctester_json` (needs #200 CamelCase map; A28 excludes
   material failure text from parity; wasm size budget).
4. #199 IfcQuantityNumber; #195–#198 extractor fixes (each a cache bump — batch them).
5. Ed: PR #24 closing note (`scratchpad/pr24-reply.md`), close PR #90.

## Gotchas
- `git add -A . ':!scratch'` aborts on the ignored path and silently skips the commit; use `git add -A`.
- `~/.cache/ifcfast/` bundle dirs are hash-named; the IDS suite is under `ids-testcases/<sha>/`.
- Debug `psets::build` is 15–25 % slower after 2A (one Vec per PropDef + wrapper decode); check release timing before quoting.
- **csg-smoke builds `--no-default-features --features csg`** (no `ids`): any extractor field whose only reader is `ids::*` needs `#[cfg_attr(not(feature = "ids"), allow(dead_code))]` or CI fails on every platform. Reproduce locally with `cargo check -p ifcfast-core --no-default-features --features csg` before pushing.
- Full corpus pytest is 73–82 min on a debug `.so` and pushes the box into swap; run detached with `setsid nohup` and watch the log with a 30-min Monitor.
