## Agent signature
- **Agent**: `claude-opus-5-5` (coordinator; session resumed from a Fable 5.1 run; opus agents for the classifier and far-origin streams, sonnet for wasm/materials/IDS and site syncs)
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `789b084` → `a578c63` (`1689ee8` ship_gate.sh, `65d8c17` #185/#211/#193, `0fa7c2d` #210/#191, `8b79f0a` #187/#173 + docs, `b68d521` release v0.6.3)
- **Session scope**: post-0.6.2 backlog — mesh_quality classifier, far-origin clip plane, arc tolerance, wasm materials/clip reliability, duplicate-pset IDS rule; release v0.6.3
- **Touched paths**: scripts/ship_gate.sh (new), .claude/skills/oracle-gate/SKILL.md, crates/core/src/mesh/{qto,stats,indexed_curve,extrusion,boolean,profile}.rs, crates/core/tests/{far_origin_clip_210.rs,ids_eval.rs,fixtures/ids/**,fixtures/ids_own/**}, crates/wasm/{src/analysis.rs,src/lib.rs,test/parity.mjs,test/clip194.mjs}, scripts/generate_sample_sidecars.py, python/ifcfast/header.py, AGENTS.md (+data copy), CHANGELOG.md, docs/ids/ambiguities.md, tests/{test_materials_rollup_185,test_ids_duplicate_psets_193}.py, tests/fixtures/{materials_roles_185,*_210}.ifc, Cargo.toml, Cargo.lock, pyproject.toml
- **Parallel sessions observed**: none on origin/main; external comment on #192 by jonatanjacobsson
- **Supersedes / superseded by**: none

## Summary

"continue" after v0.6.2. Three parallel streams with file ownership, one gate over the
combined tree, one release.

- **External validation (#192):** jonatanjacobsson (author of declined PR #24) posted a
  real-model sweep — ifcfast 0.6.2 = IfcTester on 525/525 runs over 416 models, 2.9×
  faster; duplicate same-named psets is a stock-IfcTester divergence. Replied, logged on
  #193, fixture + rule A47 added (ifcfast: any set satisfies; prohibited across all;
  IfcTester differs on 5/12 spec×wall cases). Memory `ids-external-validation-jonatan`.
- **#187 + #173 mesh_quality (`8b79f0a`, cache 37→38):** `closed` = boundary-free chain
  (every edge balanced, raw or after 0.1 mm weld) with no face covered twice with the same
  winding; all-cancelling = `degenerate`. Plain balance was corpus-refuted (MagiCAD lists
  IfcPolygonalFaceSet faces 3×: 505 RIV duct fittings would go to 3.0× ifcopenshell); a
  winding-band test was also refuted (interpenetrating multi-item bodies match ios to 1e-6).
  #173 was a PRODUCER bug: absolute 1e-6 loop-closing dedup in `indexed_curve` left a
  zero-width seam quad on 55 mm pipes → relative dedup. Open-shell counts ARK 595→200,
  RIE 1603→85, RIV 27751→10936 (PipeSegment 10704→0); volumes unchanged except 3 RIV duct
  segments onto ifcopenshell (0.9217→1.0031).
- **#210 (`0fa7c2d`):** clip planes / PBHS boundaries beyond 10 km read in f64 and rebased
  before narrowing; far-origin fixture 2.1000 → 2.0220001 (ios 2.0220000). **#191:** arc
  coincidence on chord length (1 µm in file units) not 1e-9 rad. Near-origin bit-identical.
- **#185/#211 (`65d8c17`):** graph materials every role (Duplex 91→99), wasm clip counters +
  reliable/unreliable qto split. #213 filed (materials table has no set-name column).
- **`scripts/ship_gate.sh`** (`1689ee8`): the whole gate as one serialized, checked-in
  script with the 0-byte .so relink guard and a sources-newer-than-.so abort.

**Gate** (ship_gate.sh, release .so, 21 min): cut + no-cut sweeps zero drift ×4, 5 clash
rounds no regression, round-trip OK, corpus pytest 839 green. CI green. Site synced
(`bb30c7b`, parity 17/17). **v0.6.3** tagged `b68d521`, GH release created.

## Issues
Closed: #173, #185, #187, #191, #210, #211. Filed: #213 (materials set_name), ifcfast-site#6
(receipts open_shell counts stale). Commented: #192, #193.

## Next
1. ~~Verify PyPI~~ DONE: `ifcfast==0.6.3` resolved 2026-09-30 01:11 UTC (release workflow green on all lanes).
2. ifcfast-site#6: regenerate receipts on 0.6.3.
3. #213, #209, #212 (upstream report), #207, IDS slice 5 (needs Ed), #182, #117.
4. Ed by hand: close PR #24 with the personal note (Jonatan's sweep is a good occasion), close PR #90.

## Gotchas
- Scratch git worktree left by the classifier agent at
  `<scratchpad>/wt` (detached HEAD); `git worktree remove` it when convenient (not done:
  no-delete rule). Isolated build target at `~/.cache/ifcfast-geomprec-210/` (~1 GB) — trashable.
- A cold dev build filled the /tmp tmpfs quota; big scratch builds belong under ~/.cache.

## Session close (2026-09-30)
- v0.6.3 live on PyPI, GitHub release, ifcfast.com (site `bb30c7b`).
- Every Next item is already tracked: ifcfast-site#6, #213, #209, #212, #207, #182, #117, IDS slice 5 on #192. No new issues filed at close.
- knowledge.md not updated: this session's lessons are project-specific (memory `mesh-quality-classifier`, `omarchy-oom-multiagent`, `ids-external-validation-jonatan`).
- **Jonatan activity check (Ed asked, 2026-09-30):** no new ifcfast issues/PRs; PR #24 untouched since 2026-05-31. His `ifcpipeline` runs a dedicated ifcfast worker (Dockerfile, tasks, API gateway, benchmarks vs ifccsv). On 2026-09-29 he upgraded that pipeline to ifcopenshell 0.9.0 and released `byggstyrning/ifctester-revit` v1.4.0 on IfcTester 0.9 → our 0.8.5 IDS oracle pin needs a rerun on 0.9 (#214).
