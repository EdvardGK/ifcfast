## Agent signature
- **Agent**: `claude-fable-5-1`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `5df793a` → `1867a69` (1 commit this session, + this worklog)
- **Session scope**: full-codebase review → 19 issues filed (#147–#165) → fix sweep → ship gate → GH #167 found and fixed
- **Touched paths**: crates/core/src/** (lexer, indexer, entity_table, source, guid, extractors/*, mesh/*, geom/*, clash/*, bundle/*, doc/mutate.rs, lib.rs, bin/*), crates/core/tests/{mesh_reveal,doc_mutate,mesh_item_local}.rs, python/ifcfast/{__init__,cache,classify,clash,cli,federate,header,mcp_server,model}.py, python/ifcfast/data/AGENTS.md (new), tests/test_{agents_guide,cache_manifest_integrity,python_small_fixes_162,federate_parity}.py, AGENTS.md, CHANGELOG.md, pyproject.toml, .github/workflows/ci.yml, .gitignore, docs/plans/2026-06-05_cut-openings-manifold-replacement.md, scratch/g55/baselines/* (untracked)
- **Parallel sessions observed**: none (origin/main was at 5df793a for the whole session)
- **Supersedes / superseded by**: none

## Summary

Ed asked for a codebase review, then "file them but also solve them".
Five read-only review agents (parse core, geometry kernel,
write/bundle/clash, Python layer, hygiene) produced ~60 findings; the
verified ones became GH #147–#165. Four fix agents worked disjoint file
sets with no builds; builds, tests and the oracle gate ran serialized at
the coordinator. Committed as `1867a69`, cache schema 29 → 30.

## What the gate found (evidence)

- **GH #167** — the clash oracle regressed 7 previously matched Solibri
  pairs after the #153 brep rebase. Attribution: exact f64 distance
  between the two meshes as stored in the federated bundle was 13–134 mm
  (not touching); applying the stored `transform` to the stored local
  vertices landed 421 mm from the stored centroid. Cause: `InstancePart.
  instance_transform` never folded `rep_origin`; `bundle/record.rs`
  derives the substrate transform from it. Pre-existing for polygonal
  facesets (the RIV AirTerminal on the other side of the pair showed a
  516 mm residual in the pre-sweep cache), widened by #153 to every
  IFC2x3 brep/SBSM. Fix: fold at construction. Regression test fails at
  138 564 units without the fix. Post-fix rounds (clean-pair recall):
  TMK13_Plan5 13/13 (=), Del3 2/2 (=), Del4 2/2 (was 1/2), TMK12_Del3
  14/15 (was 13/15), TMK12_Del4 18/19 (was 13/19). Pair total on
  TMK13_Plan5 92 064 → 101 253. Baselines rewritten.
- **G55_RIV first sweep** (no baseline existed): IfcFan 0.344, IfcCoil
  1.245, IfcPump 1.022 vs ifcopenshell. A/B against the pre-sweep build
  (stash → rebuild → `mesh_qto`): 0 of 35 789 products moved → pre-existing,
  filed as **#168**. RIE: 24 IfcFlowSegments moved ~1e-5 relative (f32
  precision from #153). ARK/RIB: zero drift; IfcWall 0.9998 → 0.9996
  (within gate, from #147/#153 class of change).
- **Machine**: two session crashes. (1) Four oracle processes in
  parallel → kernel OOM-killed pytest at 14:48. (2) Corpus pytest lane
  (3.7 GB) with 4.6 GB of its own temp dirs on tmpfs /tmp + swap full →
  OOM at 17:20. Fix: serial gate scripts, `--basetemp` on disk, tmpfs
  cleared with Ed's approval. Memory updated.

## Gate results (release .so, serialized)

cargo test 420/0 · clippy `-D warnings` clean · fmt clean ·
corpus pytest 313 passed / 3 skipped · oracle pytest 21/0 ·
class sweeps ARK + RIB zero drift, RIE clean, RIV → #168 ·
mesh round-trip ARK + RIB OK · five clash rounds ≥ baseline.

## Decisions

- Cache-write failure routes through `_strict_signal` but on the WARN
  arm regardless of `strict` (no wrong numbers are produced; a read-only
  HOME must not break every open).
- Revolved Angle defaults to radians when no PLANEANGLEUNIT is declared
  (schema default, ifcopenshell parity) — zero drift on G55.
- Clippy: structural lints (too_many_arguments, large_enum_variant,
  type_complexity, doc list indentation) allow-listed in lib.rs with
  rationale; everything else fixed.
- Issue #145 not closed from here (opened by a prior session): commented
  that its Del4 misses were mostly #167.

## Next

- Release: 20 + 1 commits unreleased since v0.4.42 with cache schema
  v30 — bundle as v0.4.43 / v0.5.0 (Ed's call on the number).
- #168 RIV fan/coil attribution (open-shell family; per-element).
- #166 (filed by the Python agent): expose `MeshStats.by_source` in the
  `extract_meshes`/`mesh_qto` dicts.
- Cache dirs created under scratch/g55/ this session
  (cache_sweep*_ / cache_clash*_ / pytest-tmp, several GB) — ask Ed
  before removing.
