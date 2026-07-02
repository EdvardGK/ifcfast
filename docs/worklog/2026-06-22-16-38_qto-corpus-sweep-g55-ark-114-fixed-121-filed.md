## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `fb9b07d` → `fb9b07d` (no commits — investigation + verification + issue filing; native `.so` rebuilt, not committed)
- **Session scope**: Ran the v0.5.0 geometry-oracle corpus gate on G55_ARK — verified #114 fixed on ARK, surfaced + filed the open-shell prism-fallback QTO over-count (#121).
- **Touched paths**: none in repo source (read `crates/core/src/mesh/qto.rs`, `stats.rs`, `tests/oracle/_geom_adapter.py`; rebuilt `python/ifcfast/_core.abi3.so`; scratch in `/tmp`). GitHub: commented #114, created #121.
- **Parallel sessions observed**: none on origin/main during the session window.
- **Supersedes / superseded by**: none.

## Summary
Started from "what's next" → the v0.5.0 correctness leap. A long scope detour (user briefly conflated this session with a sprucelab-frontend session) resolved to: **ifcfast is this session's lane; sprucelab is a downstream consumer I ignore until asked to consult; I own the engine/data contract and consumers defer to it.** Then executed the actual next step from `next-steps.md` #1 — run the #59 geometry oracle over a real corpus model. Result: **#114 (wall volume under-report) is confirmed fixed on G55_ARK**, and the same sweep exposed the real QTO frontier — `mesh_qto` over-counts open-shell doors/windows/railings 40–66× by substituting a loose prism bound for a signed-tetra volume it already computes correctly. Filed as **#121**.

## Changes
- **No source edits.** Rebuilt the native module: released `0.4.39` `.so` was 2 min stale (built 10:36, `add0465` committed 10:38), and an earlier build had left a **0-byte `target/debug/lib_core.so`** (OOM/interrupt during link) that cargo's fingerprint treated as current. Trashed the corrupt `target/maturin/lib_core.so`, touched `crates/core/src/lib.rs`, `maturin develop` (3.2s debug) → fix now in the editable install.
- **GH #114**: posted G55_ARK verification (`IfcWallStandardCase 2714.53 vs ios 2712.88 = +0.06%`), recommended close. `add0465` closes ARK as well as RIB.
- **GH #121 (new)**: open-shell prism-fallback over-count, full evidence + proposed tiered-reliability fix.

## Technical Details
- Oracle harness: `tests/oracle/_geom_adapter.diff_geometry_volumes()` — ifcfast `mesh_qto(cut_openings=True).volume_m3` vs a single `ifcopenshell.geom.iterator` pass, keyed by GlobalId. Import gotcha: sprucelab's CLI ships a `tests` package that shadows ours on sys.path → loaded `tests.oracle.*` modules explicitly via `importlib` under forced names.
- G55_ARK sweep: ifcfast meshed 12777 elements in 29s, ios 14078 in 94s. Structural clean (Slab +1.25%, Wall +0.06%, Column −0.14%, Member/Plate/Space ~0%). Arch detail blew up: Railing +1987%, Window +482%, Door +69%.
- Root cause (triaged with trimesh on ifcfast's OWN mesh): every over-reporter is `mesh_quality=open_shell` → `volume_reliable=False` → `volume_method=prism_fallback` → `volume_m3` carries `volume_prism_bound_m3`. But `volume_mesh_m3` (signed-tetra of our own triangles) **equals the ios kernel exactly** for door (0.655) and railing (0.029). The prism bound — meant to catch garbage tetra that *exceeds* physical bounds — is overriding a valid sub-bound tetra. Window is the hard residue (even `volume_mesh_m3` 0.899 over-counts ios 0.094 — double-sided glazing).
- ~1300 `missing_in_ours` (ios meshes, ifcfast emits no row): separate coverage facet, noted in #121.

## Next
1. **Land the #121 tiered-gate fix in `qto.rs`**: when `open_shell` and `0 < |volume_mesh_m3| ≤ min(volume_prism_bound_m3, aabb_volume_m3)`, prefer `volume_mesh_m3` for `volume_best_m3` (new `volume_method="mesh_open"` / grade). Re-run the ARK oracle to confirm door/railing totals collapse to kernel parity. Recovers door/railing immediately; windows stay open.
2. **Close #114** (user's call — they opened it; ARK + RIB both verified).
3. **Windows / genuine open glazing**: needs shell-closing (#62) or surface-pair detection — even the mesh volume over-counts.
4. **~1300 missing_in_ours**: scope which reps ifcfast skips that ios meshes (mapped-item / coverage gap).
5. **Extend the sweep** to G55_RIV / Sannergata for the full v0.5.0 damage report before tagging.

## Notes
- Scope discipline this session: do NOT touch sprucelab — separate app, separate agent; ifcfast is the engine it consumes. Re-engage only on explicit consult request.
- OOM forensics: a stale 0-byte cdylib that cargo considers "current" is a silent trap — `git tag --contains <fix-sha>` to check whether the *released* wheel even has a fix, and stat the `.so` mtime vs the commit time. Debug `maturin develop` stayed ~3s and safe on 16 GB.
- `volume_mesh_m3` is the unsung correct column — already in the `mesh_qto` output, just not chosen as the headline for open shells. The fix is selection logic, not new geometry.
