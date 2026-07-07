# #143 Steps 1+2: band-capped narrow phase — sweep 21.5 s, band run 10.5 h → 4 min

## Agent signature
- **Agent**: `claude-fable-5`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `719445f` → `94869dc` (2 commits this session, pushed)
- **Session scope**: #143 clash narrow-phase speedup (Steps 1+2), oracle-gated
- **Touched paths**: crates/core/src/clash/engine.rs, crates/core/src/geom/narrow_phase.rs, crates/core/src/geom/mod.rs, crates/core/tests/probe_143_jitter.rs
- **Parallel sessions observed**: none (`git log origin/main --since="2026-07-07 09:00"` shows only this session's commits)
- **Supersedes / superseded by**: continues 2026-07-07-06-42_141-per-rule-tolerance-100pct-recall.md

## Summary
#143 Steps 1+2 shipped and pushed (`d1db47d`, `94869dc`). Step 1: the
tolerance>0 narrow phase drives off a single `distance()` (parry
ExitEarly==0.0 verified in pinned 0.17.6 source). Step 2: NEW mechanism
not in the judged plan — `geom::min_distance_within`, parry's distance
visitor through public `traverse_best_first_node` seeded `cap.next_up()`,
used REJECT-ONLY with a 5 mm/5 % pad; survivors re-run exact
`query::distance` for the emitted value. Output bit-identical at every
level (A/B pairs, sweep baseline, per-pair distances vs the 10.5 h-era
canonical report). Full #141 sweep: **21.5 s** wall; full-federation band
run @0.1 m: **244 s** (was 10.5 h for a class-scoped subset). Biggest
single factor: the venv `.so` was a DEBUG build all along — release alone
is ~200–285× on clash; memory `maturin-develop-debug-profile` records it.

## Evidence (numbers, not diagnoses)
- Debug-profile A/B (profile-matched to the old canonical numbers),
  bit-identical output: Step 1 = 1.16×/1.11×; Steps 1+2 = 1.56×/1.74×
  (mini 243 pairs / sample 93 pairs @0.1 m).
- Release .so vs pre-session debug .so: 285×/228× same workloads,
  bit-identical → float parity across profiles holds.
- Full sweep on release: 21.5 s wall; recall 13/13 + 47/49; 92,064 found
  pairs (= baseline); all 13 judged pair metas byte-equal to
  `tmk13_rie_riv_ark_report.json`. `no regression` exit.
- Full-federation band @0.1 m unscoped: 246,643 pairs
  (101,870 hard + 144,773 clearance), 244 s.
- **Jitter finding** (probe `tests/probe_143_jitter.rs`, real G55 pair):
  `query::distance(a,b)`=0.047500610 vs `(b,a)`=0.047498703; seeded
  traversal reproduces the other value → parry composite distance is
  schedule-deterministic only. First value-emitting cap attempt flipped a
  0.09999847 m pair out of a 0.1 m band (sample 93→92) — caught by the
  A/B gate, hence reject-only design.
- `closest_points(margin)` refuted as reject mechanism by source read:
  Disjoint leaves never tighten the best-first bound → beyond-band pairs
  traverse unpruned (same family of flaw the judge found in `contact`).
- Band-vs-t0 semantic note (pre-existing): band `hard` (d==0) counts
  101,870 vs 92,064 t=0 `intersects` pairs — GJK returns exact 0.0 on
  face-touching adjacencies `intersection_test` misses.

## Changes
- `crates/core/src/clash/engine.rs` — t>0 branch: padded
  `min_distance_within` reject probe → exact `min_distance` for emitted
  values; t=0 path untouched; comment block rewritten.
- `crates/core/src/geom/narrow_phase.rs` — `min_distance_within` (seeded
  best-first, REJECT-ONLY docs) + 4 unit tests incl. exact-cap
  `next_up` edge.
- `crates/core/tests/probe_143_jitter.rs` — ignored env-driven diagnostic
  documenting the asymmetry/jitter mechanism.

## Gates run
Rust suite 362/0 (twice); pytest test_clash_sweep_rules + test_smoke
44/44; #141 full sweep vs baseline (release .so) = no regression,
distances byte-equal; A/B bench bit-identity across before/Step1/Step2/
release. Full corpus pytest NOT run — change touches only the clash
narrow phase (no mesh/QTO surface). No AGENTS.md/cache-schema impact
(no agent-visible primitive changed).

## Tracker activity (signed "#143 clash narrow-phase speedup")
- #143: results comment — numbers, debug-.so discovery, jitter mechanism,
  Step 2 deviation rationale, Step 3 (Qbvh) defer recommendation,
  both-sides `include_classes` still open.
- #141: ops note — gate now 21.5 s, results unchanged, baseline untouched.

## Next
1. **#50 federation** — implement per design comment (Python
   `federate()`, `source_model` column → cache v29, `clash([a,b])`
   sugar, parity gate). Now the top build item.
2. **Truth expansion** — ACC downloads per census groups (RIV+RIE@
   2026-03-09 unlocks 2 rounds; 03-18/03-23 pair unlocks 2 more;
   Alle_saker Sept-2025 = 39 clean pairs). Sweeps are now 21 s each —
   no scheduling cost.
3. **Ed**: full Solibri checking export (Excel, per-rule results) →
   precision reconciliation + real rule tolerances (settles the 20.7 mm
   RIVv case).
4. Deferred on #143: Qbvh broad phase (streaming scale, #67), both-sides
   `include_classes` mode.

## Notes
- Perf measurement discipline: `maturin develop` = debug. Any quoted
  engine wall-clock needs `env -u CONDA_PREFIX CARGO_BUILD_JOBS=4
  maturin develop --release` first (incremental release is OOM-safe;
  cold is not).
- The venv currently holds the RELEASE .so of `94869dc`.
