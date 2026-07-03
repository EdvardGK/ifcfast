# Session: GH #62 profile winding fix — windows +482% → exact oracle parity + oracle-gate tooling

## Agent signature
- **Agent**: `claude-fable-5`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `eed795b` → `759316a` (3 commits this session)
- **Session scope**: GH #62 window QTO residue root-cause + fix; oracle-gate tooling/skill; orchestration convention
- **Touched paths**: crates/core/src/mesh/profile.rs, crates/core/tests/mesh_winding.rs (new), python/ifcfast/header.py, AGENTS.md, CLAUDE.md, .gitignore, tests/oracle/class_sweep.py (new), .claude/skills/oracle-gate/SKILL.md (new), docs/worklog/ (committed last night's untracked entry), ~/.claude/CLAUDE.md (global, uncommitted config)
- **Parallel sessions observed**: none — all commits on origin/main this window are this session's (9cc775d, f926053, 759316a)
- **Supersedes / superseded by**: none

## Summary
Root-caused and shipped the #121/#62 window residue (`9cc775d`): the +482% was never shell-openness — Revit authors `IfcArbitraryProfileDefWithVoids` void polylines **clockwise**, and `profile.rs` blindly `reverse()`d them, inverting every hole-wall normal so the divergence-theorem volume ADDED voids instead of subtracting. All 208 G55_ARK windows now at **exact ifcopenshell parity** (was mesh 8× kernel → prism_fallback), doors +9.3% → parity as a bonus, zero regressions (A/B-proven on slabs, RIB untouched). Also promoted the corpus-sweep tooling into the repo + an `/oracle-gate` skill, and codified the multi-agent orchestration framework (model-fit per agent; fable sub-coordinators one level; opus freely when fable unavailable) into global + project CLAUDE.md.

## Changes
- `crates/core/src/mesh/profile.rs`: `profile::extract` now enforces the Polygon2D invariant (outer CCW, holes CW) via f64 shoelace `normalize_winding` at its single exit; blind `h.reverse()` removed from `arbitrary_with_voids`. Also fixes CW-authored OUTER loops (cap/wall normals un-inverted) and makes `revolved.rs`'s documented outer-CCW assumption actually hold.
- `crates/core/tests/mesh_winding.rs` (new): 4-combination ring test (outer × hole, CW × CCW) asserting volume 3.0 + closed manifold — the minimal repro that proved the mechanism, verbatim as regression test.
- `python/ifcfast/header.py`: `_CACHE_SCHEMA_VERSION` 24 → 25 with changelog; `AGENTS.md` v25 entry added per convention (review agent caught the omission).
- `tests/oracle/class_sweep.py` (new): per-class corpus differential as checked-in CLI — JSON cache, `--baseline` diff, nonzero exit on drift. Previously rebuilt in scratchpad every session.
- `.claude/skills/oracle-gate/SKILL.md` (new, shared via git — `.gitignore` now `/​.claude/*` + `!/.claude/skills/`): the geometry ship gate as an invocable skill (suites → rebuild → sweep vs baselines → A/B attribution → review).
- Local (not committed): baselines `scratch/g55/baselines/{G55_ARK,G55_RIB}.json` from the validated post-fix sweeps; global `~/.claude/CLAUDE.md` orchestration + debugging-discipline sections.
- GH: **#138 filed** (−Z extrusion inversion, exact arithmetic evidence), #62 updated (window residue fixed; remaining scope = exact-footprint refinement only, deprioritized).

## Technical Details
- **Dissection path** (aggregate → element → component → primitive): window SUM 5.8×/8× ios → per-element ratios 3.2–9.6× all prism_fallback → ifcfast mesh has SAME 76 tris as ios but 3.25× volume → pane solid exact, frame-ring component wrong → profiles are WithVoids with CW-authored holes → synthetic 2×2/1×1 ring repro: CCW hole = 3.0 exact, CW hole = 4.33 + open_shell. One mechanism, proven.
- **Validation** ran as a 3-agent fan-out (sub-coordinator corpus sweep, code-reviewer, sonnet pytest): windows 208/208 parity (104 `mesh` + 104 `mesh_open`), doors 35 elements → parity, covering 17/19 moved toward oracle, slabs 0/155 changed (stash → pre-fix rebuild → A/B), RIB all classes within noise, pytest 253 green, cargo suite green.
- **A/B side-catch → #138**: covering `28Yq_B2DnDqOcKkFUBXkmu` −14.2% and 4 divergent ARK slabs (+5–15%, +115.6 m³) all trace to `extrude_polygon` assuming +Z winding — a `(0,0,−1)` extrusion comes out inside-out and cancels in multi-item bodies / corrupts CSG subtractors. Pre-existing, exact arithmetic in the issue.
- **Agent-ops lesson**: background agents stop at "waiting for my job" checkpoints — both needed a resume nudge with an explicit completion contract. Codified in global CLAUDE.md.
- Gotchas hit: `maturin develop` fails when VIRTUAL_ENV+CONDA_PREFIX both set (`env -u CONDA_PREFIX`); class_sweep cache keyed by model name (delete before sweeping a new build); scratch-ARK re-exported ~Jul 1 so June baseline percentages aren't comparable.

## Next
1. **Release v0.4.41** — 4 unreleased commits (mutate #133, #132 hardening, #62 fix, oracle tooling) = natural bundle; version bump + `git push origin v0.4.41`, CI publishes. Ed left the tag decision open at session end.
2. **#138 −Z extrusion** — first work item for the next (opus) session; mechanism proven, gate = 5 named GUIDs to parity via `/oracle-gate`, minimal-repro-first per the mesh_winding.rs pattern.
3. **#122 coverage gap** (~1300 ARK rows) — discovery fan-out by representation type from the class_sweep cache's fast-only/ios-only sets.
4. #123 degenerate partial-collapse; #131 trust-band design call; #62 rest-scope (exact footprint) deprioritized — consider close-with-comment after #138.

## Notes
- Full opus-oriented handoff (recipes, gotchas, agent-model assignments) in the project memory `next-steps.md` — written deliberately for sessions WITHOUT fable.
- The "blocked on shell-closing" label in the old handoff was a wrong diagnosis that survived two sessions — worklogs/next-steps should record evidence, not theories (now a global CLAUDE.md rule).
- No blockers.
