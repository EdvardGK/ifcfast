## Agent signature
- **Agent**: `claude-fable-5-1` (coordinator) with opus sub-agents (review, architect, 4 implementers)
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `1db78ad` → `2187830` (commits this session: `ad251b1` design, `b9297f5` slice 1A, `2187830` slice 1B/1D)
- **Session scope**: External PR #24 triage → native IDS 1.0 design (roadmap #140 phase 3) → slice 1 (Entity + Attribute facets) shipped to main
- **Touched paths**: `crates/core/src/ids/**` (new), `crates/core/src/lib.rs`, `crates/core/src/indexer.rs` (1 visibility line), `crates/core/Cargo.toml`, `Cargo.lock`, `crates/core/tests/{ids_xml,ids_eval}.rs`, `crates/core/tests/fixtures/ids/`, `python/ifcfast/{ids.py,__init__.py,model.py}`, `python/ifcfast/data/AGENTS.md`, `AGENTS.md`, `scripts/{gen_schema_tables,fetch_ids_testcases}.py`, `tests/oracle/{ids_conformance,ids_parse_differential,test_ids_conformance,report}.py`, `tests/oracle/{ids_testcases.lock,ids_xfail.toml}`, `tests/test_schema_tables_drift.py`, `docs/ids/ambiguities.md`, `docs/plans/2026-09-24_ids-validation-design.md`, `pyproject.toml`
- **Parallel sessions observed**: none (`git log origin/main --since=2026-09-23` empty before push)
- **Supersedes / superseded by**: none

## Summary

**Triage.** Ed asked about "the old one from May/June": open external PR #24
(jonatanjacobsson, 2026-05-31, 10.6k lines, native Rust IDS engine on `bee2b8f`)
had sat unanswered; Ed had spoken to the author but it was his first external PR
and it drifted. Opus review against current main (202 commits later, 10
conflicting shared files): rule logic sound; shared-code changes (owned
EntityTable buffer copied on the mesh/bundle/wasm hot path, extra indexer pass
per extract call, O(n·m) classification expansion) and Python layer (IfcTester +
ifcopenshell at runtime, silent per-spec fallback, stale cached session giving
wrong answers on a second IDS, vacuous tests, personal SharePoint paths) not
mergeable. **Ed's decision: do not take the PR; build IDS natively on today's
architecture.** Closing note for Jonatan drafted in Ed's voice (scratchpad
`pr24-reply.md`); the auto-mode classifier denies PR comments, so Ed posts it.
PR #90 (ToghrolTP, superseded by #107) also awaits Ed's close.

**Positioning (Ed, recorded in memory).** IfcTester stays the reference
implementation; ifcfast IDS is the speed-first companion, exactly the
`mesh_qto` ↔ ifcopenshell relationship. Upstream give-back is a secondary,
periodic habit at release time, never a design driver or a release gate.

**Design** (`docs/plans/2026-09-24_ids-validation-design.md`, `ad251b1`, epic
**GH #192**). Findings that shaped it, all verified against main: the product
table is a whitelist (IDS candidates must come from EntityTable type tokens or a
non-whitelisted class silently passes); the emitted PsetTable is lossy for IDS
(lists joined with `", "`, no unit column) so slice 2 refactors the pset
extractor's first pass into a shared PropertyGraph; only LENGTHUNIT is resolved
today; IfcRelNests/groups are not indexed but their positions are pinned in
`doc/rel_rules.rs`; schema codegen emits Python only → extended to Rust.

**Slice 1 shipped (Entity + Attribute facets, end to end).**
- Rust `crates/core/src/ids/` behind default-on feature `ids`: strict roxmltree
  parser + schema-free audit, IR with canonical JSON, XSD→`regex` translator,
  restriction matching (tolerance.md rule `|x−v| ≤ 1e-6·|v| + 1e-6`, ORed
  patterns), generated per-schema tables (`scripts/gen_schema_tables.py`,
  ifcopenshell 0.8.5, 17k lines / 1.04 MB, drift test), compile (schema-dependent
  audit: unknown/derived/inverse attribute, pattern on numeric attribute…),
  candidates from type tokens, attribute reads by generated position,
  predefinedType via IfcRelDefinesByType (type first, per ifcopenshell
  `get_predefined_type`; IFC2X3 `<NAME>TYPE` mapping), eval with facet + spec
  cardinality, column-major report with IfcTester label templates.
- PyO3 `validate_ids` / `_ids_canonical_json`; `ifcfast.validate_ids(ids, ifc,
  on_unsupported="raise"|"mark", filter_ifc_version=False)`,
  `Model.validate_ids`; `IdsReport(specs, elements, failures)` DataFrames with
  `.ok`, `.to_parquet`; exceptions `IdsInvalidError` / `IdsUnsupportedError` /
  `IdsUnitError` ⊂ `IfcfastError`. AGENTS.md section + decision-tree row.
- Correctness harness: `scripts/fetch_ids_testcases.py` pins buildingSMART/IDS
  `development` @ `a670477` (334 cases, sha256 lock, CC BY-ND 4.0 so not
  vendored); `tests/oracle/ids_conformance.py` runs IfcTester 0.8.5 and ifcfast
  against filename truth, classifies green / ifcfast_bug (blocks) / ifctester_bug
  / test_case_drift / unsupported_facet; strict `ids_xfail.toml`;
  `ids_parse_differential.py` compares canonical parses.

## Evidence

| Gate | Result |
|---|---|
| Parse differential (canonical JSON vs IfcTester) | 321/321 byte-identical (13 invalid-* rejected by our audit, IfcTester's XSD accepts them) |
| Rust `ids_eval` over all 9 suite folders | 334 cases, **0 mismatches**; entity 33/33, attribute 56/56, restriction 25/25, ids 12/12 correct; 26 invalid-* rejected outright; 202 Unsupported (property/classification/material/partof/tolerance) |
| Python harness (`python -m tests.oracle.ids_conformance`) | 125 green, 202 unsupported_facet, **0 ifcfast_bug**, 7 ifctester_bug where ifcfast agrees with truth |
| IfcTester 0.8.5 vs filename truth | 22/334 disagree; **19 attributed to IfcTester** by source (14 tolerance: relative-only `is_x` facet.py:53-60; 2 regex-OR ANDed facet.py:1056-1058; optional-null attribute facet.py:327-357 ×2; IFC2X3 `IfcExtendedMaterialProperties.Properties` AttributeError facet.py:912-913; 3 float-literal-on-integer invalid cases accepted) → **GH #193**, all in `ids_xfail.toml` |
| `cargo fmt --check`, `cargo clippy --all-targets -D warnings` | clean |
| `cargo test -p ifcfast-core` (default features) | 24 targets, 532 passed, 0 failed |
| `pytest tests/ -q` with G55 corpus | 727 passed, 3 skipped (debug build, 73 min) |

**Design amendments from suite evidence** (all in the doc): `invalid-*` truth =
fail OR typed invalid error (suite `scripts.md`: "at least one requirement fails …
do not comply with the Audit tool"), a pass is the only bug; predefinedType
resolves type-first; ifcVersion filtering opt-in (suite pairs IFC4 files with
IFC2X3 specs and expects results; IfcTester `should_filter_version=False`);
predefinedType literal outside the enum is not rejected (`WALDO` must pass).

## Ledger
No spruceledger node consulted changed an action this session (no `.ifc`-specific
claims applied; IDS has no nodes yet).

## Next
1. **Slice 2** (#192): Property / Classification / Material facets + `units.rs`
   + PropertyGraph refactor of `extractors/psets.rs` pass 1. Gate: property,
   classification, material, tolerance folders green; PsetTable / QuantityTable
   / unit_scale **bitwise-identical** on the corpus (`/oracle-gate`).
2. Slice 3: PartOf + IfcRelNests/group/fills capture, cache schema 34 → 35.
3. Slice 4: CLI `ifcfast ids`, MCP `validate_ids`, wasm `validateIdsJson`,
   `to_ifctester_json()`; wasm size budget (+`roxmltree`+`regex`, file if > 400 KB).
4. Slice 5: G55 IDS vs Solibri (needs Ed's Solibri session) + release-build
   benchmark vs IfcTester.
5. Open ambiguity rows in `docs/ids/ambiguities.md`: A6, A7, A9, A10, label
   key-order parity (slice 4).
6. Ed: post `pr24-reply.md` on PR #24 and close; close PR #90.
7. Release: bundle slices 1–2 (or 1–3) into v0.6.0 rather than tagging now.

## Gotchas
- `IFCFAST_CORPUS` is a colon-separated absolute FILE list.
- Suite semantics override the design doc; amend the doc, do not xfail.
- The auto-mode classifier denies `gh pr comment` / `gh pr close`; issues are fine.
- `.venv` is the oracle env (ios 0.8.5, now ifctester 0.8.5); miniconda base has ios 0.8.3.
- `~/.cache/ifcfast/` holds bundle caches by hash; the suite lives in `ids-testcases/<sha>/`.
