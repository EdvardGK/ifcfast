## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `e5f0dba` → `281db9d` (this phase; full session `e784f9d` → `281db9d`, 8 commits)
- **Session scope**: authoritative-file QTO validation (ACC files + Solibri ITO benchmark), root-cause of the `IfcFaceBasedSurfaceModel` 8× volume bug, and the volume-reliability tripwire + hybrid-routing design/example
- **Touched paths**: crates/core/src/mesh/brep.rs, python/ifcfast/header.py, crates/core/tests/cut_openings_integration.rs, AGENTS.md, README.md, python/ifcfast/__init__.py, examples/hybrid_qto_routing.py
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: **corrects** the GH #53/#57 conclusions in `docs/worklog/2026-06-06-17-47_*` (which were computed on STALE local file copies)

## Summary

Validated ifcfast's QTO against the **authoritative** G55 IFC files + the
Solibri ITO benchmark (both fetched from ACC), which overturned the
prior worklog's stale-file conclusions. ifcfast matches the ITO
benchmark within 5 % on 568/747 (RIB) and 411/452 (Prefab) products. The
session's lasting output is a **design**: ifcfast self-labels
untrustworthy volumes via a provable tripwire and a pipeline routes only
the flagged ~0.3 % to ifcopenshell — shipped as a runnable example, with
the whole tool reframed as a speed-first *companion* to ifcopenshell.

## Changes

- **`brep.rs` + `header.py` (`d292b3a`)** — GH #53 fix: `mesh_face` now
  honours inner `IfcFaceBound` holes (Newell-projection + earcut), fixing
  hole-bearing faceted breps. Cache 14→15. (Landed early; attribution
  corrected below.)
- **`cut_openings_integration.rs` (`4abd491`)** — gate
  `WALL_WITH_THROUGH_CUT_ROTATED` on `prism-csg-fast` (a `-D warnings`
  regression from the W9 commit).
- **AGENTS.md / README.md / `system_prompt()` (`7bca5f6`, `281db9d`)** —
  strengthened the experimental/WIP + verify-QTO disclaimer (no
  responsibility for bad numbers; benchmark + cross-check vs
  ifcopenshell/Solibri; report discrepancies in detail), and reframed
  ifcfast as a **speed-first companion to ifcopenshell, not a
  competitor**.
- **`examples/hybrid_qto_routing.py` (`281db9d`, new)** — the canonical
  fast-pass-then-escalate pattern.
- **Memory** `acc-g55-ito-benchmark.md` — ACC paths + ITO column map.
- **GH issues** — #53 (corrected: misdiagnosed cut bug → brep holes),
  #56 (resolved by W4), #57 (reopened: real `IfcFaceBasedSurfaceModel`
  bug), all with authoritative-file evidence.

## Technical Details

- **Stale-file trap (the meta-lesson).** My first #53/#57 "resolved"
  used `~/dev/oldpc1` copies where the GUIDs map to *different geometry*
  than the files the tester used. Authoritative files + the Solibri ITO
  benchmark live on **ACC → Skiplum Backup → 10027 Grønland 55** (`1. IFC`
  and `B_Leveranser/06_ITO/20260604_ITO_SkiplumStandard_G55.xlsx`, Volume
  = col 16 by GUID). Always validate against ACC, not local copies.
- **#57 root cause (corrected twice).** The slab is an
  `IfcFaceBasedSurfaceModel`. ifcfast = 1504 m³, ifcopenshell = 131.69
  (watertight, verified via `util.shape.get_volume`), Solibri = 180.2.
  (1) NOT the cross-product cut path (8/9 #53 walls have no voids).
  (2) NOT non-convex fan-triangulation — I implemented the earcut-for-
  non-convex fix, it changed the volume by ZERO, and **reverted it**:
  for a *planar* face the signed-tetra volume is triangulation-invariant
  (fan overlaps cancel in the signed sum); it only affects *area*. The
  real cause: the shell is genuinely **open** (682 border edges even
  fully meshed; ifcfast vol > AABB → provably garbage). ifcopenshell sews
  it watertight (OCC `BRepBuilderAPI_Sewing`) → 131.69. Solibri's 180.2 =
  `footprint × bbox-height` (reverse-engineered EXACTLY: union-XY
  footprint 400.5 × 0.45 = 180.2) — a prism over-estimate, NOT true
  geometry. So the three numbers are three different definitions; ifcfast
  is the only one *wrong*.
- **Rendering is unaffected** — the open/winding corruption breaks
  *volume* only. glTF area matches ifcopenshell (856.7 = 856.8); materials
  are `doubleSided:true` + viewer computes face normals, so winding
  inconsistency causes no culling/lighting holes. Disclaimer correctly
  targets *quantities*, not rendering.
- **The design (co-developed with the user).** Two provable upper bounds:
  loose `V ≤ aabb` (already computed as `mesh_quality`, but NOT exposed
  to Python!), tight `V ≤ footprint × z_extent`. The tight bound's VALUE
  doubles as the prism fallback (= Solibri's number). Agent-first:
  ifcfast emits the flag, the *pipeline* routes — ifcfast must never call
  ifcopenshell internally (kills the kernel-free speed story).
- **Benchmark**: ifcfast `mesh_qto` is **14–46×** faster than
  ifcopenshell's iterator (8-proc) — 582 ms vs 26.8 s on G55_RIB.

## Next

- **Implement the volume-reliability columns** (the real next step):
  `volume_reliable` (bool), `volume_method` (`mesh`/`prism_fallback`),
  `volume_mesh_m3`, `volume_prism_bound_m3`. Compute the union-XY
  footprint (raster) ONLY when the loose bound fails (volume > aabb) so
  the hot path stays fast; substitute `footprint × z_extent` as the
  volume when flagged. Expose the already-computed `mesh_quality` too.
  Cache bump 15→16, AGENTS.md columns, tests, update the example to use
  the column instead of deriving it inline. → file as a GH issue.
- **W6** — `IfcBooleanClippingResult`+`IfcPolyline` polygonal-bounded
  clip under-applied (+30–159 % on a scatter of real walls;
  ifcopenshell matches Solibri). Higher element-count impact than the
  FBSM slab. Blueprint in the plan doc. Tracked under GH #58.
- **Website (ifcfast.com)** — lives in a separate repo; add the same
  experimental/verify-QTO + partnership banner there.
- Phase-1 CSG release (W3+W4+#53+W9, +W6) still pending; cache at 15.

## Notes

- `volume_reliable` flag (loose/bbox tripwire) is implementable today
  from existing columns (`|volume_m3| > aabb_volume_m3`) — the example
  derives it inline so it runs on any released wheel. The columns just
  make it first-class + add the tighter bound + better fallback value.
- `.venv` has the `csg`+`prism-csg-fast` wheel built; `/tmp/g55_qto/` has
  the authoritative G55_RIB / G55_RIB_Prefab IFCs + the ITO xlsx.
- The non-convex earcut work is the right fix for surface *area* (and
  rendering of notched faces) — reverted here only because it doesn't
  touch *volume*; re-propose with an area/render justification if wanted.
