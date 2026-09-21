## Agent signature
- **Agent**: `claude-fable-5-1` (coordinator) + opus implementer / sonnet auditor / opus reviewer sub-agents
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `c00f81b` → `72b0f95` (1 commit this session)
- **Session scope**: #188 far-origin f32 quantization fix — model-level global shift, Local bake + f64 rebase on wasm / to_gltf / drift
- **Touched paths**: crates/core/src/mesh/{rebase.rs (new), mod.rs, gltf.rs, cut_openings.rs}, crates/core/src/lib.rs, crates/core/tests/{mesh_anchor_188.rs (new), cut_openings_proptest.rs}, crates/wasm/{build.sh, src/analysis.rs, src/lib.rs, test/stream.mjs}, python/ifcfast/{header.py, model.py, data/AGENTS.md}, AGENTS.md, tests/fixtures/{far_origin_duct_mm.ifc, far_origin_unplaced_first_mm.ifc} (new), tests/test_far_origin_gltf_188.py (new); ifcfast-site `ef2f72d` (wasm sync + Duplex sidecars); scratch/g55/baselines/G55_{ARK,RIB,RIE,RIV}.json rewritten (local only)
- **Parallel sessions observed**: none (`git log origin/main --since=2026-09-18T15:00` shows only this session's commit)
- **Supersedes / superseded by**: none

## Summary

Tester (ifc-check / Skiplum, KNM_RIV MagiCAD mm NTM model) filed #188:
wasm `streamMeshes` quantizes far-origin geometry to an 8 mm / 128 mm
f32 lattice (Ø400 duct wobbles ±4.4 mm, 9–13 % zero-area faces) and
`streamShiftJson()` reported `[0,0,0]`.

Diagnosis: `BakeFrame::World` casts to f32 at absolute magnitude
(`mesh/mod.rs` bake site); wasm sinks and `write_gltf` subtracted the
shift AFTER the cast. `m.meshes()` was already correct (Local bake +
f64 reposition, #179), so wheel and wasm disagreed on the same build.

Shipped `72b0f95`:
- `mesh::rebase` — one `global_shift_for` (f64 factor), one
  Local→shifted-world encode, used by extract_meshes / point_cloud /
  write_gltf / analyse_drift / wasm stream+batch.
- **Model-level shift pin (phase 1d)**: review found the shift was
  pinned from the first EMITTED product; a grid-/`$`-placed first
  product resolves to anchor [0,0,0] and silently zeroes the shift for
  the whole model (likely the tester's `[0,0,0]`). Now: scan resolved
  placements before first emission, pin rounded origin of lowest-step-id
  product beyond 10 km, deliver via `ProductSink::on_global_shift` +
  `MeshStats.global_shift`. 0.4 ms on G55_RIV. First-emitted-anchor
  stays as fallback for georef baked into representation geometry.
- glTF: `WriteOptions {global_shift, unit_scale}`; instanced TRS
  translation from f64 `InstancePart.anchor` (new field) — also fixed
  instanced nodes 1000× out on non-metre files (pre-existing, masked
  because the site forces instancing off); `asset.extras.ifcfast.global_shift`.
- Unit factor f64 at every metre cast → `m.meshes()`, `iter_meshes()`,
  `point_cloud()`, `to_gltf()["global_shift"]`, wasm `shiftJson()` all
  report one identical value. `qto::compute` left f32 (shared with
  substrate; would move clash/QTO baselines).
- `m.drift` → Local frame (translation-invariant columns; centroid /
  placement re-added in f64). Cache schema 32 → 33.

## Evidence

| fixture `far_origin_duct_mm.ifc` | outer radius spread | zero-area tris |
|---|---|---|
| before (World bake) | 105.05 mm | 134/262 (51 %) |
| after, all paths | 0.00002–0.00014 mm | 0 |

Ship gate: cargo core 459 / wasm 16; pytest 350 pass 6 skip; wasm
limits 5/5, stream 22/22, parity 18/18 (Duplex sidecars regenerated,
no verdict flipped, worst rel delta 1.95e-5). Class sweep ARK/RIB/RIV:
no class past tolerance. **Per-element A/B vs released 0.5.2 wheel
(throwaway venv): mesh_qto bit-identical on 48 255 elements.** The
sub-tolerance sweep drifts (IfcWall +0.0004 ARK) are #177's, which the
Sep 7 baselines predated → all four baselines rewritten.

Review deviation kept: re-pinning `mesh_anchor` from the first
triangle-bearing fragment MOVES geometry (it is the Local bake origin) —
implementer's test caught it; per-part `anchor` field instead.

## Issues
- #188 closed with measurement table. #189 filed (KHR_mesh_quantization
  undeclared on all-instanced glTF). #117 rescoped: substrate composite
  reps still bake absolute f32; `instances` transform/bbox/centroid/
  placement columns f32-only (0.18 m off at 6.7e6 m) — fix direction
  = Local bake + f64 anchor columns + bundle-level global_shift.

## Next
- Release: bundle #188 with the next tag (v0.5.3). Not tagged.
- #117 substrate work (clash parity gate is bitwise → baselines).
- #170 32-segment ceiling binds on Ø2500 (6 mm sagitta) — separate
  issue if tester reports it.
- ifcfast-site pushed `ef2f72d` (wasm + sidecars) → Vercel deploy.
