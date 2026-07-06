# Session: Clash oracle bring-up (#141) + #142 æøå escape fix

## Agent signature
- **Agent**: `claude-fable-5`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `a3f5aa7` → `bc360bc` (3 commits this session: `cf6a8b2` #142 fix, `e02c8b8` clash perf, `bc360bc` oracle harness)
- **Session scope**: clash-oracle harness bring-up (Phase 1 of #140) + tester issue #142 hotfix
- **Touched paths**: tests/oracle/{bcf_truth,federate,clash_sweep}.py, crates/core/src/clash/engine.rs, crates/core/src/doc/step_fmt.rs, tests/test_mutate.py, scratch/g55/solibri/* (ground truth, not committed)
- **Parallel sessions observed**: tester session ("edkjo, tester-in-chief", Windows) filed #142 mid-session against the 0.4.42 wheel; no foreign commits on origin/main
- **Supersedes / superseded by**: none

## Summary
Phase 1 of the Layer-2 roadmap (#140) is now real: the clash-oracle harness (#141) exists, ran end-to-end against genuine Solibri coordination-round ground truth, and produced its first reconciliation — **clean-pair recall 11/13 (84.6%), topic recall 45/49 (91.8%)** on TMK13_Plan5 vs a federated RIE+RIV+ARK set at the exact model versions Solibri checked. Both misses are attributed to rule semantics (a ~54 mm clearance-band pair and a ~1.34 m non-contact "RIVv" topic), not engine defects. Along the way the clash engine's narrow phase was found unusable at federation scale (3+ h, killed) and fixed to 16 min; and tester issue #142 (mutate() emitting raw UTF-8 → strict readers drop æøå) was fixed, gated, and closed same-day.

## Changes
- `tests/oracle/bcf_truth.py` — Solibri BCF zip → truth: topics, clean viewpoint pairs, per-rule tags (rule name = first line of topic description). BCF `Header/File Date` = the model's **internal STEP timestamp** — that's the version-matching key.
- `tests/oracle/federate.py` — schema-exact pyarrow merge of N bundles (rep_id namespacing, unit_scale equality enforced, guid-collision report). The #50 hand-merge done right; will gate first-class federation parity.
- `tests/oracle/clash_sweep.py` — CLI mirroring class_sweep: recall gate, AABB-gap miss diagnosis, per-rule recall table, extra-pair attribution, baseline drift (exit 1 on regression). Baseline at `scratch/g55/baselines/clash_tmk13_plan5.json` (outside git).
- `crates/core/src/clash/engine.rs` (`e02c8b8`) — narrow phase: class-filter before geometry, parallel TriMesh build, `intersection_test` first, `distance` only when tolerance > 0, rayon over pairs with order-preserving collect. Same output, 3+ h → 16 min on 2-model set.
- `crates/core/src/doc/step_fmt.rs` + `tests/test_mutate.py` (`cf6a8b2`) — #142: `encode_string` now emits canonical `\X2\`/`\X4\` escape runs for all code points outside 0x20–0x7E; tester repro is the regression test incl. ifcopenshell 0.8.5 read-back; corpus mutate gate 15/15 on 4 disciplines.

## Technical Details
- **Ground truth**: ACC Skiplum Backup → `10027 - Grønland 55/B_Leveranser/02_TMK/` — 12 Solibri BCF exports (TMK12–15 + Alle_saker), 388 topics, inventoried to `scratch/g55/solibri/bcf_inventory.json`. The `.smc` files are ZIP wrappers around a **Java-serialized** blob — not parseable at gate grade; BCF/xlsx exports are the only usable truth format.
- **Version matching**: an agent walked ACC version history and matched 7 IFC versions by internal STEP header timestamp (RIE v94, RIV v92, ARK v89, RIB v89, RIB_Prefab v1, both utsparinger models) — all exact; map in `scratch/g55/solibri/models_tmk13/version_map.json`. Validation: 291/303 TMK13 selection GUIDs resolve in RIE+RIV, 303/303 story with ARK.
- **Engine finding**: `min_distance` (parry global distance, exhaustive BVH traversal) ran per candidate pair even at tolerance 0, serially. Federated sets die on this: 33k instances → 79k candidate pairs → 3+ h. Measured meshes are small (p50 262 tris) — the cost was pure algorithm choice.
- **Rule semantics**: Solibri topic descriptions carry the checking-rule name; recall is only meaningful per rule (clash rule vs clearance rule vs manual comment). The 3-model per-rule table: `10.1. RIE - RIVv` 5/6, `2.3. ARK Div - RIE` 1/1, `RIVv` 3/4, free-text 2/2.
- Remaining scale problem: 3-model set (47k products) = 92k pairs in 34 min — O(N²) broad phase needs a grid/BVH accelerator, and tolerance>0 runs re-introduce the distance cost. Filed as engine work (see Next).

## Next
- **Tolerance-band semantics**: rerun the sweep at the rule's tolerance (probe suggests ~50–100 mm for rule 10.1) and encode per-rule tolerance in the harness so the 54 mm pair matches under its own rule's semantics.
- **Broad-phase accelerator + tolerance>0 distance cost** — needed before clash is a daily-loop tool at federation scale (34 min for 3 models won't fly). With #50/#67.
- **Ask Ed**: export a FULL Solibri checking report (Checking → Report → Excel, all results per clash rule, any recent round) → unlocks precision/count reconciliation; today's gate is recall-only because BCF rounds are triaged subsets.
- **#50 first-class federation** (`clash([a,b])` / `bundle([...])`) — `tests/oracle/federate.py` is the reference implementation and parity gate.
- Sweep the remaining TMK rounds (TMK12/14/15 + Del2–4) once per-rule tolerance semantics are in — 388 topics of regression truth waiting.

## Notes
- `ifcfast.clash()` include_classes filter is either-side (pair kept if EITHER class matches) — surprised me during a probe; a both-sides mode would make restricted runs far cheaper.
- Solibri BCF exports reference stray models (`NAV_Drammen.ifc`, `/dev/null`) in a few topic headers; the truth loader tolerates them.
- `G55_BCF-rapport_Updated_BCF.bcf` in the TMK folder is actually a Fiskebrygga (A4) export — mixed into the wrong project folder on ACC; don't use it as G55 truth.
- Local corpus IFCs in `scratch/g55/` are June-2026 versions — TMK truth needs the matched Feb versions in `scratch/g55/solibri/models_tmk13/`; don't mix them.
