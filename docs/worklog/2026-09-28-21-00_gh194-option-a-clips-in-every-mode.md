## Agent signature
- **Agent**: `claude-fable-5-1` (coordinator; opus agents for the Rust contract change, the oracle harness, the review and the review fixes; sonnet for site sync)
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `7c3015d` → `c03d113` (`38106d9` harness nocut mode, `c03d113` feature + review fixes, amended)
- **Session scope**: GH #194 option A — half-space clipping bodies applied in every mesh mode, with a no-cut oracle sweep mode as the new gate
- **Touched paths**: crates/core/src/mesh/{boolean,bounded_clip (new),halfspace_clip,cut_openings,cut_validate,qto,styles,placement,mod}.rs, crates/core/src/{lib.rs,bundle/record.rs}, crates/core/tests/{clip_contract_194.rs (new),cut_openings_integration.rs,mesh_reveal.rs,cut_openings_proptest.rs}, crates/wasm/{src/analysis.rs (schema 37),test/clip194.mjs (new)}, python/ifcfast/{header.py,model.py}, tests/test_clip_contract_194.py (new) + 7 fixtures, tests/oracle/class_sweep.py, .claude/skills/oracle-gate/SKILL.md, AGENTS.md (+data copy), CHANGELOG.md, scratch/g55/baselines/*_nocut.json (local)
- **Parallel sessions observed**: none on origin/main
- **Supersedes / superseded by**: none

## Summary

Ed chose **A** on #194 ("A half-space clip is part of the element's own shape, not an
opening"). Shipped `c03d113`, cache schema 36 → 37, #194 closed.

**Mechanism.** `boolean::boolean_result::clip_first_operand` applies IfcHalfSpaceSolid /
IfcBoxedHalfSpace / IfcPolygonalBoundedHalfSpace second operands to every host fragment
in the fragment frame (instance × rep_origin, f64), chains at any depth. New pure-Rust
`mesh/bounded_clip.rs` (column-plane splits with edge cache, IN/ON/OUT band, flat-face
orientation rule, rim-chain closure + earcut, closed-manifold + volume-bound checks, eps
retries, Hertel–Mehlhorn convex pieces) so wasm clips too; Manifold only as `csg`
fallback; otherwise host left unclipped, tagged, counted (`halfspace_clip_unapplied`),
`volume_reliable=False`, carried as `ProductMesh.clip_unapplied` (review fix: the
opening-cut pass collapses segments and was erasing the tag). Plain planes read exactly
from BaseSurface.Position (old slab-centroid plane was half a thickness off). Clipped
fragments keyed by the boolean node id. `keep_cutters` is a compatibility no-op except
for half-spaces nested in a UNION. Pre-existing placement bug fixed on the way:
`IfcAxis2Placement3D` RefDirection=$ with Axis ∥ X gave a singular frame; now
IfcFirstProjAxis (0 changed frames on ARK/RIB). ifcopenshell 0.8.5 deviates for +X → #212.

**Evidence.** G55_RIB 38 clipped products: 18 > 0.1 % off ifcopenshell(openings off)
before, 5 after (4 solid-operand by contract, 1 known −4 % beam); walls +86 %/+95 % now
exact; every half-space case ≤ 0.0004 %. ARK 209 clipped: all half-space within 0.1 %,
cut mode 0 drift on 12 221. Fixtures: single PBHS 37.7600 vs 37.7596, chain-3 23.8737 vs
23.8721, boxed = plain, mapped clip shared by 2 instances (one mirrored, EXT instancing
groups both), IFC2X3 default axes, diagonal plane, unresolvable + opening.

**Gate (twice, gate 4 pre-review-fix and gate 5 post):** cut sweeps zero drift ×4;
**new `class_sweep --mode nocut`** (vs ifcopenshell `disable-opening-subtractions`,
baselines captured on the pre-change .so by the harness agent, marker-file handshake so
the Rust agent could not rebuild first): RIB IfcWallStandardCase 1.0691 → 1.0000, ARK
IfcWallStandardCase 1.0027 → 1.0000, all else unchanged (ARK/RIB IfcSlab remain 1.0147 /
1.0384 = solid-operand slabs, by contract); 5 Solibri clash truth rounds no regression;
mesh_roundtrip OK ×4; corpus pytest 834 green (**13.9 min on the release .so vs 79 min
debug**); wasm clip194/stream/limits green. nocut baselines rewritten post-gate.

**Review (opus, read-only) → 1 blocking (tag erased in cut mode) + 5 must-have tests,
all applied; non-blocking → #209 mapped re-clip per instance, #210 f32 plane at far
origin, #211 wasm counters.**

## Gotchas
- A bash script overwritten while an old instance still runs it: bash reads scripts
  incrementally → the old shell executes lines of the NEW file. Killed the stale gate5
  shell; always write to a new path.
- `target/{maturin,debug}/lib_core.so` 0-byte glitch recurred twice (maturin ELF parse
  error); `touch crates/core/src/lib.rs` + rebuild under the lock.
- The gate's own `maturin develop` step failed on that glitch but the installed .so was
  already the right release build (checked mtime vs all `crates/core/src/**/*.rs`).

## Next
1. CI on `c03d113` (watcher), site sync to cache v37 (agent) → then release **v0.6.2**
   (contract change + cache bump; bundle with anything landing before).
2. Reporter to re-check Snowdon Towers (only on the edkjo box) on v0.6.2.
3. #209–#212 follow-ons; #207 lazy IDS wasm; IDS slice 5; #187, #185, #182, #117, #191.
