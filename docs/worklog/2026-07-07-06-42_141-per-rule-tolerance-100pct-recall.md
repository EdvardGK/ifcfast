# #141 per-rule tolerance → 13/13 recall + selection-scoped supplemental (10.5h → 27s)

## Agent signature
- **Agent**: `claude-fable-5`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `ba2cf5e` → `2439d00` (1 commit this session)
- **Session scope**: clash-oracle per-rule tolerance + scaling prep
- **Touched paths**: tests/oracle/clash_sweep.py, tests/oracle/test_clash_sweep_rules.py, scratch/g55/baselines/clash_tmk13_plan5.json, scratch/g55/solibri/{tmk13_rie_riv_ark_report,bcf_census,design_143_judged,design_50_federation}.json
- **Parallel sessions observed**: none (`git log origin/main --since="2026-07-06 18:00"` shows only this session's commit atop the prior worklog commit)
- **Supersedes / superseded by**: continues 2026-07-06-16-36_clash-oracle-bringup-142-escape-fix.md

## Summary
Per-rule tolerance semantics shipped in the clash-oracle harness (`2439d00`): each Solibri truth pair/topic is judged against ITS OWN rule's tolerance via engine `min_distance_m`. TMK13 3-model gate now **pair recall 13/13 (100%), topic recall 47/49**, no regression, baseline promoted. The two remaining topic misses are attributed, not failed. Along the way the supplemental band run was cut from **10.5 hours to 27 seconds** (bit-identical, asserted) by scoping to selection guids instead of classes, and a parallel multi-agent workflow produced: a full BCF truth census (12 rounds), a code-verified implementation plan for #143, and an implementation-ready federation design for #50 — all posted to the tracker.

## Evidence (numbers, not diagnoses)
- **Mechanism verification correcting the handoff**: the two t=0 missed pairs measure 54.04 mm (rule `10.1. RIE - RIVv`) and 3.74 mm (rule `RIVv`) mesh distance — the inherited "1.34 m non-contact" label was wrong for the clean pair. Engine distances agree with an independent numpy point-triangle probe (54.0 mm / ≤4.5 mm upper bound).
- **Recall @ rule tols 10.1=0.1 m, RIVv=0.01 m (provisional)**: 13/13 pairs, 47/49 topics. Missed topics: `6c39d702` (free-text rule "Skal disse gå opp i veggen?", 4/5 guids not in the RIE+RIV+ARK federation), `c971ef58` (RIVv, closest candidate 20.7 mm > 10 mm band — NOT tuned away; unverified hypothesis: Solibri RIVv counts insulation).
- **Benchmark for #143** (11h full run, engine-reported): base t=0: 314k AABB candidates → 92,064 intersecting pairs, ~30–36 min. Band t=0.1 class-scoped: ~315k candidates → 123,309 pairs, 10.5 h. Derived: `distance()` ≈ 1.15 s CPU/pair vs `intersection_test` ≈ 47 ms CPU/pair (~25×). Prior worklog's "92k pairs" = OUTPUT count, not candidates (reconciled).
- **Mini-bundle equivalence**: matched set + all 13 pair distances bit-identical between the 10.5 h class-scoped run and the 27 s selection-scoped run (asserted in-session, `report_miniscope.json` vs canonical report).

## Changes
- `tests/oracle/clash_sweep.py` — `--rule-tol 'RULE=METRES'` (exact / `prefix*`); base run unchanged (context + regression parity); ONE supplemental run at max band tol on a `write_selection_bundle()` mini-bundle (band-topic guids only); `pair_matches()` judging; topic misses printed + in report JSON with AABB-gap lower bound; `outside_rule_tolerance` diagnosis.
- `tests/oracle/test_clash_sweep_rules.py` — 11 unit tests for parse/match/judge helpers (incl. f32 band-edge).
- Baselines/reports (outside git): baseline promoted with rule tols recorded; census + judged #143 design + #50 design saved under `scratch/g55/solibri/`.

## Tracker activity (all signed, same session scope)
- #141: census comment (sweepable rounds, download groups, rule census 116 clean pairs) + results comment (13/13, attribution, harness perf fix).
- #143: judged implementation plan (Step 1 single-`distance()` bit-identical on tol>0 branch; Step 2 contact-prediction reject-only, oracle-gated; Step 3 Qbvh with O(N²) as differential oracle) + hard benchmark numbers; noted both-sides `include_classes` gap.
- #50: implementation-ready federation design (Python `federate()`, `source_model` column → cache v29, `clash([a,b])` sugar, parity gate plan).

## Next
1. **#143 Step 1** — single-`distance()` on the tolerance>0 branch (`crates/core/src/clash/engine.rs:316-322`), oracle-gate, measure vs 36 min / 10.5 h before-numbers. Then Step 3 (Qbvh) with proptest set+order equality vs O(N²).
2. **#50** — implement per design comment.
3. **Truth expansion** — ACC downloads per census groups (RIV+RIE@2026-03-09 unlocks 2 rounds; 03-18/03-23 pair unlocks 2 more; Alle_saker Sept-2025 = 39 clean pairs).
4. **Ed**: full Solibri checking export (Excel, per-rule results) → precision reconciliation + real rule tolerances (settles the 20.7 mm case).

## Notes
- Workflow ops note: 2 of 3 #143 design agents died on StructuredOutput retry cap; judge compensated (verified the surviving design's claims against pinned parry3d 0.17.6 source, refuted two of them, synthesized the corrected plan). Failure explicitly accepted, not silently dropped.
- The mini-bundle trick is recall-exact ONLY because judged pairs live inside BCF selections; full-set context (extra-pair attribution) still needs the base run.
