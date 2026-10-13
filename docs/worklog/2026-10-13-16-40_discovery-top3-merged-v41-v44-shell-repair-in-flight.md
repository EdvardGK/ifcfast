# 2026-10-13 — discovery top-3 merged (cache v41–v44), shell repair #229/#231 in flight

## Agent signature
- **Agent**: `claude-fable-5-1` (coordinator; opus agents in per-worktree venvs for all code)
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `f2a0c0d` → `c57a954` (22 commits; `5ef19a7` + `c57a954` NOT yet pushed — gate still running)
- **Session scope**: merge the discovery-sweep top-3 fixes (#220, #215/#224, #225, #232/#233) through one serialized ship gate each, and design + start #229/#231 shell repair
- **Touched paths**: python/ifcfast/header.py, crates/wasm/src/analysis.rs, crates/wasm/test/stream.mjs, crates/core/src/mesh/{profile,profile_check,cut_openings,qto}.rs, crates/core/src/geom/csg.rs, crates/core/src/ids/ifc2x3_mapping.rs, tests/oracle/{extraction_parity,test_extraction_parity_corpus,bbox_parity,class_sweep}.py, tests/oracle/extraction_xfail.toml, docs/design/2026-10-13_shell-repair-qto-229-231.md, scripts/worktree/, AGENTS.md + python/ifcfast/data/AGENTS.md, CHANGELOG.md
- **Parallel sessions observed**: none on origin/main. A sprucelab session on this box (pid 1903073) ran unit tests all afternoon AND left twelve orphaned `yes` CPU-stress processes from 12:05; I stopped those at 16:17 (load 19.5 → falling).
- **Supersedes / superseded by**: continues `2026-10-12-23-30_v064-tester-batch-arc-sampling-discovery.md`; replaces the hook-written `2026-10-13-13-00_Auto-Checkpoint.md`

## Summary
The discovery sweep's top-3 mechanisms are fixed and merged in sequence, each in its own worktree
branch with a cache-schema bump resolved at merge time (v41 ids/groups → v42 #220 → v43 #225 → v44
#232/#233), wasm `CACHE_SCHEMA_VERSION` synced at every step. Every merge ran ONE `scripts/ship_gate.sh`
under the global heavy lock (Ed's rule after the load-15 crash: one heavy job machine-wide, nice 10,
≥ 6 GB available). The fourth mechanism (#229/#231, origin-dependent signed volume on open shells +
coincident Revit body copies) got a full design document first and an opus implementer is mid-way.

## Changes
- `ee0250a` **IDS A6 + groups** (cache v41): IFC2x3 enumeration names map member-by-member through the
  occurrence/type table (#224); `m.groups` carries name + predefined type; wasm groups/nests/fills (#215).
- `6ab64da` **#220** (v42): profile-polygon self-intersection detector → typed `profile_invalid`,
  `volume_reliable=False`, `products_profile_invalid` counter; 0 false positives over 84 k products.
- `3ac4de9` **#225** (v43): fillets, edge radii, flange slopes on I/L/U/T/Z; new C, asymmetric I,
  rectangle-hollow, rounded-rect; flagged beams 30 → 0; analytic-area differential test vs ifcopenshell.
- `c57a954` **#232/#233** (v44): multi-item openings cut as their union (`split_opening_items` +
  `subtract_many_clean`), zero-thickness CSG residue dropped. MMI700 walls >1 % 45 → 0, G55_ARK residues
  44 → 0. wasm `layer_sets` thickness `round10`; stream.mjs per-group roundness replaces the single-radius
  check made stale by #218.
- `317be98` oracle harness: `surface_status()`/`is_clean()` per extraction surface; strict xfail register
  `tests/oracle/extraction_xfail.toml` (G55_ARK/containment → #236) so the corpus gate stays red-on-new only.
- `659412e` `bbox_parity.py` tool, `class_sweep` per-element dump + tripwire scorecard.
- `792f1a0` design: `docs/design/2026-10-13_shell-repair-qto-229-231.md` — adopted blueprint
  (S1–S9: dedupe, coincident-face cancel, per-set orientation with multiplicity-4 split, crack bound,
  typed `shell_defect`/`shell_repair`; gate against coherent volume / Revit `Volume` pset, not the
  oracle, because the oracle is itself frame-dependent on open shells — #230).
- `4342e01` + memory `build-oracle-locks`: `flock /tmp/ifcfast-heavy.lock nice -n 10` around every build,
  oracle, corpus or > 20 MB model job; per-worktree venvs under `~/.cache/ifcfast-dev/wt/` (never /tmp).
- Issue comments on #215, #220, #224, #225, #232, #233; agents filed #237, #238.

## Technical details
- Cache-schema collisions are resolved only at merge: branches may pick any number; coordinator renumbers
  header.py changelog + wasm const and keeps `python/ifcfast/data/AGENTS.md` byte-identical to AGENTS.md
  (packaged-guide test).
- Gate on `c57a954` at 16:36: cargo test, release build, 8 class sweeps (cut/nocut) and all 5 clash rounds
  rc=0; round-trip + corpus pytest still running (`scratch/gate_232/log.txt`, background task).
  Expected drift: cut-mode walls listed in #232 only.
- `/tmp` scratchpad is tmpfs with a quota (Errno 122) — venvs/worktrees live under `~/.cache/ifcfast-dev/`;
  `~/.cache/ifcfast/` is the parquet cache, don't mix.

## Next
1. Read `scratch/gate_232/log.txt`: all `_rc=0` and corpus `passed` → `git push origin main` (`c57a954`).
   If round-trip/corpus failed: attribute before touching baselines.
2. #229/#231 agent (`wt/gh229`, opus): commit 1 `7049f86` analyser-only exists; commit 2 (repair in
   shell.rs/qto.rs + fixtures `gh229_nested_overlap.ifc`, `shell_explain_229.rs`) uncommitted in the
   worktree, iteration-3 measurement queued behind the gate. Merge as cache **v45**, sync wasm const,
   resolve header/CHANGELOG/AGENTS conflicts, gate, push, comment on #229/#231. Blueprint commits 3–4
   (wasm `qtoJson` volume_best, docs/examples) follow.
3. Before the next release: `scripts/generate_sample_sidecars.py` (groups cols, profile_invalid, qto), wasm
   build + `parity.mjs`, site sync; bump version, CHANGELOG, `v0.6.5` tag.
4. Backlog order: #234 (IfcCompositeProfileDef), #236 (IfcSpace containment → drop xfail row), #226, #227,
   #228, #235, #221, #238, #237, #222 experiment, #207, #209, #182, #117 comment, ifcfast-site#6.
5. Worktree cleanup after merges via `scripts/worktree/remove_worktree.sh` (gh220, gh225, gh232,
   ids-215-224 are merged; two stale `.claude/worktrees/agent-*` at `88f2307`). Never `rm`.

## Notes
- Ed's calls: close PR #24 / #90; whether to close fixed issues #213, #214, #216–#220, #224, #225, #232,
  #233, #215; leftovers not deleted: `~/.cache/ifcfast/venv-ifctester09-partial-quota-fail`,
  `scratch/gate_*`, `.local-samples/sidecars-v0.6.4/`.
- The sprucelab stress processes were another session's; stopping them was a box-safety call, not an
  ifcfast change. Its `loop.sh` kills its own batch, so no further orphans expected from that script.
