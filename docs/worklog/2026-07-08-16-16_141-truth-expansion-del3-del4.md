## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `1bd4b94` → `1bd4b94` (no code commits; baselines live outside repo, 2 GH issues filed)
- **Session scope**: #141 clash-oracle truth-expansion — download version-matched G55 models from ACC and sweep the TMK12/TMK13 Del3+Del4 BCF rounds
- **Touched paths**: scratch/g55/solibri/models_del3/, scratch/g55/solibri/models_del4/, scratch/g55/baselines/clash_tmk1{2,3}_plan*_del{3,4}.json, scratch/g55/report_*.json, docs/worklog/ (this file)
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: none

## Summary

Expanded the clash oracle from 1 sweepable round (TMK13_Plan5) to **5** by
downloading version-matched IFCs from ACC (Skiplum Backup project) and
running `tests.oracle.clash_sweep` per round. Every downloaded model's
STEP `FILE_NAME` header was verified to exactly match the BCF-referenced
timestamp before sweeping.

### Rounds now baselined (corrected numbers)

| round | pairs | topics |
|---|---|---|
| tmk13_plan5 (prior) | 13/13 100% | 47/49 95.9% |
| **tmk13_plan5_del3** | 2/2 100% | 6/6 100% |
| **tmk13_plan5_del4** | 1/2 50% | 6/7 85.7% |
| **tmk12_plan2_del3** | 13/15 86.7% | 38/42 90.5% |
| **tmk12_plan2_del4** | 13/19 68.4% | 33/40 82.5% |

Base tolerance 0.0 m + rule tolerances `10.1. RIE - RIVv=0.1`, `RIVv=0.01`
(same as the shipped TMK13_Plan5 baseline). All baselines under
`scratch/g55/baselines/` (outside repo — client data).

### ACC version map (verified exact header matches)

models_del3/ (TMK13_Del3 uses RIV+RIE; TMK12_Del3 uses all four):
- G55_RIV.ifc  = ACC v115 (header 2026-03-09T19:01:48)
- G55_RIE.ifc  = ACC v116 (header 2026-03-09T15:29:58)
- G55_ARK.ifc  = ACC v110 (header 2026-03-06T15:53:51)
- G55_RIB_Prefab.ifc = ACC v90 (header 2026-02-26T13:25:05; exported 02-26, uploaded 03-02 — no 02-26 upload exists)

models_del4/ (TMK13_Del4 uses RIV+RIE; TMK12_Del4 uses all three):
- G55_RIV.ifc = ACC v119 (header 2026-03-18T16:01:42)
- G55_RIE.ifc = ACC v124 (header 2026-03-23T14:44:29)
- G55_ARK.ifc = ACC v114 (header 2026-03-19T18:02:27)

version_map.json written into each folder. ACC lineage URNs: RIV
`dm.lineage:6b8ZiyN1Rw6004h1_ZVHwg`, RIE `iCxcorRdTlWSvreHKamtiA`, ARK
`-cgOI_8HSDScej43jc-kcA`, RIB_Prefab `7C4ylX5mTAWlVbdsvWSalA`. Project
`b.bdb8892d-fae6-4838-ba4a-c92ee9b8c863`, hub `b.ec5b025b-3fab-4641-8606-bc38c8719f44`
(Skiplum AS EMEA), IFC folder `fs.folder:co.GzF-gZS7SzOIsbABI6StWw`.

## Two findings filed

**GH #144 (harness correctness bug):** `ensure_bundle` in clash_sweep.py
keys the per-model bundle cache on `ifc.stem` and only checks
`ifcfast.__version__`/schema for freshness — **never file content**. Two
different *versions* of `G55_RIV.ifc` collide. Sweeping Del4 (v119/v124)
after Del3 (v115/v116) on the shared default `--cache-dir` silently served
the stale v115/v116 bundles → TMK12_Plan2_Del4 collapsed to a false 3/19.
Verified: cached `bundles/G55_RIV` had 29679 rows = the v115 product count;
sprinkler `0daah76dXElfMclleQBGX4` (v119-only, absent in v115) reported
`guid_not_in_federated_bundle`. Workaround: isolated `--cache-dir`
per model-version set (`scratch/clash_oracle_cache_del4` for Del4). The
Del4 numbers above are the corrected (isolated-cache) re-runs. Del3 numbers
were always valid (bundled fresh on first use).

**GH #145 (engine recall signal):** 5 genuine `narrow_phase_miss` — Solibri
flags a clash, AABBs overlap (gap 0.0), ifcfast's narrow phase finds no
intersection. Always an MEP terminal/flow-control family on the non-pipe
side: Damper (TMK13_Del4), FlowTerminal ×3 + DuctSilencer ×1 (TMK12_Del4).
Straight FlowSegment×PipeSegment match fine. Hypothesis (unverified):
mapped/type geometry (IfcRepresentationMap+MappedItem) baked incompletely,
OR Solibri over-reports. Needs per-element tessellation triage to classify
engine-bug vs over-report; gate any fix on corpus differential.

Other misses across the 4 new rounds are attributed, not failures:
`guid_not_in_federated_bundle` (truth references main G55_RIB, which the
Del BCFs don't list among their models — rules 7.1/7.2/7.4 RIB Bjelker/
Søyler/Vegger) and `outside_rule_tolerance` at the provisional 0.01 m RIVv
band.

## Next
1. **Bigger rounds still un-downloaded** (heaviest ROI = Alle_saker, 39
   clean pairs, needs the Sept-2025 6-model set incl. 168 MB ARK).
   TMK14_Plan6_Del2/Del3 and TMK15_Plan7_Del2 also pending. Always
   isolate `--cache-dir` per version set until #144 is fixed.
2. **Fix #144** — content-key the per-model bundle cache (hash/size/mtime
   or the STEP header timestamp in bundle meta).
3. **Triage #145** — extract one Damper×FlowSegment pair, tessellate in
   ifcfast + ifcopenshell, decide engine-bug vs Solibri-over-report.
4. Still blocked on Ed exporting the full Solibri checking report for real
   per-rule tolerances (the 0.01 m RIVv band is provisional).
