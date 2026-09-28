## Agent signature
- **Agent**: `claude-fable-5-1` (coordinator; opus agents for streams A/C/review-fix, sonnet for docs)
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `cff9a36` → `57d86f2` (3 commits this session: `5d87b44` v35, `a7896c8` worklog, `57d86f2` wasm test fix; CI green on `57d86f2`)
- **Session scope**: HI90 tester issues #201–#203 + extractor batch #195–#200, shipped as cache schema v35
- **Touched paths**: crates/core/src/{indexer.rs,lib.rs,units.rs,body_rep.rs,schema_products.rs,mesh/mod.rs,clash/engine.rs,extractors/*,ids/eval.rs}, crates/wasm/src/analysis.rs, crates/wasm/test/parity.mjs, python/ifcfast/{model.py,header.py,whitelist.py,clash.py,data/schema_supertypes.py,data/AGENTS.md}, scripts/{gen_schema_supertypes.py,gen_defined_type_names.py,dump_tables_ab.py,generate_sample_sidecars.py}, AGENTS.md, CHANGELOG.md, tests/ (5 new files + fixtures)
- **Parallel sessions observed**: none on origin/main; issues #201–#203 were filed 2026-09-26 by a separate session signed "edkjo, hi90-mottakskontroll/personal-tool"
- **Supersedes / superseded by**: none

## Summary

Ed said "go" on the recommended order: answer #203, fix #201, fold #202 into
the same extractor pass as the #195–#198 batch under one cache bump. All nine
issues shipped in `5d87b44` (cache schema 34→35, unreleased). #195–#200 closed;
#201–#203 commented and left open for the reporter to confirm.

**What changed (see CHANGELOG [Unreleased] for the user-facing list):**
- Product whitelist is generated from the IFC schemas
  (`crates/core/src/schema_products.rs`, 206 classes, written by
  `scripts/gen_schema_supertypes.py`), pinned EQUAL to the Python closure. 62
  classes gain rows. Tag read from per-class schema position.
- `has_body` / `body_rep_type` on products, spaces, wasm products. One body
  definition (`body_rep.rs`, tiered) shared by indexer and mesher.
- Openings contract: products = reveal-all tier-1 index; AGENTS.md "Coverage
  boundary" rewritten + "Counting elements" note.
- Extractors: CamelCase `value_type` via generated defined-type table,
  type-own psets/quantities rows, recursive unit resolution (+ indexer now
  feeds WithOffset), `unit_step_id` via UnitTable, IfcQuantityNumber in IDS,
  CamelCase IDS `actual`.
- Clash: new spatial/structural/alignment/facility/port classes → `non_physical`.

**Review findings that changed the shipped shape (opus reviewer, verified):**
1. Indexer dispatch never routed `IfcConversionBasedUnitWithOffset` to the unit
   table, so #197 did not reach `m.unit_scale` — fixed, index-level test.
2. First-match body selection could prefer `Body-FallBack` / unnamed solid over
   a later `Body` — replaced with tiers; tier 1 reproduces the pre-v35 mesher
   pick exactly. Sweeps zero-drift before and after.
3. 62 new meshable classes reached the clash engine as hard clashes — added to
   `NON_PHYSICAL_CLASSES` (hand list, pinned to PRODUCT_TYPES by test).

**Gate (two full runs, second on the post-review tree):**
- cargo test 573 green; csg-only check + clippy `-D warnings` + fmt clean.
- class_sweep G55 ARK/RIB/RIE/RIV: zero drift both runs. New class
  IfcCooledBeam (RIV): 11 el, ratio 1.0521, unattributed → comment on #168,
  NOT written into baseline.
- Corpus pytest run 1: 794 passed + 1 expected fail (new #197 test vs pre-fix
  .so); run 2: 795 passed, 3 skipped (1:18 h on debug .so).
- Table A/B (`scripts/dump_tables_ab.py`, no cache): every diff attributed
  (value_type casing 223 215 cells; type-own rows ARK +177 / RIB +101; feet
  fixture unit_step_id null→3). IDS suite unchanged 278/334.
- wasm build OK (1.26 MB); parity 16/18, both failures = stale sidecars
  (cache_key/column counts, value_type casing). parity.mjs got a staleness
  guard for the two new product keys.

**Observed facts worth keeping:**
- Reporter's "#203 openings mode == measure" is not reproducible: classify
  checks SKIP_ENTITIES first; G55_ARK openings all 'skip'. Asked for repro.
- `target/` is 106 GB. Not cleaned (deletion needs Ed).
- Two stale agent worktrees under `.claude/worktrees/` at `88f2307`.

## Issues filed this session
- #204 hotswap accepts only `Body` identifier (disagrees with has_body/mesher).
- #205 `mesh::profile::resolve_length_scale_opt` is one-level (nested yard → 1.0 tolerance scale).
- Comment on #168 (IfcCooledBeam 1.0521).

## Next
1. **Release v0.6.0** once IDS slice 3 lands (bundle: IDS 1–2 + v35). Before
   tagging: regenerate wasm sample sidecars (`scripts/generate_sample_sidecars.py`
   → ifcfast-site `sync-wasm.sh`) so parity returns to 18/18.
2. IDS slice 3 (PartOf on indexer, `m.nests`/`m.groups`) — needs its own cache
   bump (35→36) or ride v35 if it lands before release.
3. #194 still waits on Ed's contract decision.
4. #204, #205 small follow-ons; #187, #186, #185, #182, #117, #191 queue.
5. Ed by hand: post `pr24-reply.md` on PR #24 and close; close PR #90.

## Release addendum (2026-09-28)

Ed: "get it out there". **v0.6.0 released** — `782d3e1` (release commit) tagged
`v0.6.0`; CI publish green (linux x86_64, windows, macOS arm64 + intel, sdist on
PyPI with attestations); GitHub release created from the CHANGELOG 0.6.0 section.
CHANGELOG got the missing IDS slices 1–2 "Added" entry. Site synced to the
v0.6.0 wasm + regenerated sidecars (ifcfast-site `52dd0cb`, parity 18/18, no
skips) — the parity gate caught **#206** (summary()/schemas listed 5 spaces
columns, frame has 7): fixed on main `cf09c70` + pinned to the live frame,
closed; v0.6.0 wheel carries the 5-column summary, next release carries the fix.
Local gotcha repeated twice this session: `target/{debug,maturin}/lib_core.so`
ends up 0 bytes after interleaved builds → `touch crates/core/src/lib.rs` +
`maturin develop` under the lock repairs it.
