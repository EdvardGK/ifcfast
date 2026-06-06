## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `e784f9d` → `d292b3a` (3 commits this session; the 3rd added post-worklog during real-file validation)
- **Session scope**: GH #58 W9 cross-product prism flush-wiring + W6 design blueprint; then real-file cut validation (GH #53/#56/#57) which produced a brep-hole fix
- **Touched paths**: crates/core/src/mesh/cut_openings.rs, crates/core/src/lib.rs, crates/core/tests/cut_openings_integration.rs, docs/plans/2026-06-05_cut-openings-manifold-replacement.md
- **Parallel sessions observed**: none (origin/main carried only this session's commits during the window)
- **Supersedes / superseded by**: none

## Summary

Wired the just-landed W9 prism-CSG primitives into the cross-product
`IfcRelVoidsElement` cut path: `CrossProductCut::flush` now attempts the
pure-Rust prism algebra before Manifold, behind `prism-csg-fast`. This is
the "primitivene er klare, mekanisk" slice from the prior session's
next-steps — but the real work was the F3-safe frame composition and
proving it against the Manifold oracle (including under rotation). Then
scoped W6 (the last Phase-1 piece) with a code-architect, filed the
blueprint, and surfaced the one architectural decision the blueprint
glossed (table-at-`apply`). Default builds stay byte-identical; both
feature sets green under `-D warnings`.

## Changes

- **`crates/core/src/mesh/cut_openings.rs`** (+274/−28):
  - `flush(unit_scale, table: Option<&EntityTable>)` — new `table` param.
    When the in-rep pass left the host body intact (`Passthrough`) and a
    table is supplied, the prism fast-path runs; else the existing
    manifold fold. Restructured the opening gather into an `arrived` id
    list shared by both branches.
  - `HeldOpening` struct replaces the `(Vec<f32>,Vec<u32>,[f64;3])` tuple
    — now also carries `rep_step_id` (single direct extrusion only) +
    `world_transform`, captured in `route()`, so `flush` re-derives
    opening params without the streaming table.
  - `try_prism_cut(host_mesh, openings, table)` — the fast-path:
    `extrude_params` → `PrismParams` with `local_xform = rot_only(world)·
    local_xform`, cutter rebased by the f64 anchor difference; dispatch
    `prism_csg::subtract_many` outcomes (Cut/Empty rewrite geometry,
    Unchanged→Passthrough, NotParametric→`None`=manifold fallback).
  - helpers: `single_direct_rep_step_id`, `is_identity_mat4`,
    `rewrite_host_geometry`, `rot_only`.
- **`crates/core/src/lib.rs`** (+34): `prism_table_for_flush(buf)` builds
  a 2nd `EntityTable` ONLY under `prism-csg-fast` (else `None`); passed to
  the two `BakeFrame::Local` flush sites (`mesh_qto`, `meshes`). The
  `BakeFrame::World` glTF site passes `None` (near-origin prism result
  would not align with world-coord host mesh).
- **`crates/core/tests/cut_openings_integration.rs`** (+248): through-cut
  + rotated + Z-pocket fixtures; `fold_one_host` + `signed_mesh_volume`
  helpers; differential tests proving prism == manifold == analytic.
- **`docs/plans/2026-06-05_cut-openings-manifold-replacement.md`** (+137):
  W6 implementation blueprint.

## Technical Details

- **F3 frame contract is the whole game.** The `BakeFrame::Local` bake
  computes host vertices as `transform_vector3(world)·p + frag_off` with
  `frag_off=0` for the anchor fragment and the world translation carried
  on f64 `mesh_anchor`. So the prism's working frame is
  `rot_only(world_transform)·local_xform` (translation column zeroed) and
  the result lands exactly where the baked host mesh sat. The cutter,
  baked in its own anchor frame, is moved into the host frame by
  `prism_csg::rebase_params(opening_anchor, host_anchor)` — only the small
  (<10 m) anchor *difference* ever touches f32. Verified far-origin-safe
  by the existing prism_csg unit test; verified end-to-end here by the
  rotated differential test (0.51 m³ invariant under a 30° world spin).
- **Why prism only at Local sites.** Considered making the prism path
  frame-aware; simpler and safer to gate at the call site — only
  `BakeFrame::Local` guarantees near-origin geometry, so only those sites
  pass the table. The glTF World-frame path keeps the manifold fold.
- **Why a 2nd table behind the feature, not threading the streaming
  table out.** Minimal blast radius: default `csg` builds are
  byte-unchanged; the double-build is confined to the experimental
  feature + the post-stream flush (not the hot loop). Threading the
  streaming table out is the promotion-time optimisation.
- **Existing pocket fixture is NOT a through-cut.** The original
  `WALL_WITH_CROSS_PRODUCT_OPENING` opening spans Z 500..2500 inside a
  0..3000 wall — a Z-pocket, so the prism path correctly returns
  `NotParametric` and falls back. Needed a NEW full-height interior-hole
  fixture to actually exercise the prism path. (Reminder: real
  doors/windows cut through wall *thickness*, perpendicular to the wall's
  Z sweep → also NotParametric. The same-axis through-cut is the
  full-height-slot case; the pocket/perpendicular cases are W9 Phase 2
  slab-decomposition, still deferred.)
- **W6 challenge surfaced:** the bounded halfspace is processed by
  `apply()` *inside the streaming sink* (no table in scope), unlike
  `flush` (clean post-stream hook). Recommended carrying the resolved
  `BoundedHalfspacePayload` on `ProductMesh` from tessellation rather than
  threading a borrowed table through the mutably-borrowed sink. See the
  blueprint.

## Next

- **W6 — tight polygonal-bounded halfspace** (last Phase-1 piece). Approach
  B (pure-Rust 2D reduction reusing `polygon_bool` + `SweepFrame`), behind
  `prism-csg-fast`, plane-clip fallback. Blueprint in the plan doc; settle
  the ProductMesh-payload-vs-table decision first (recommend payload).
  Tracked under GH #58.
- **Then bundle + release** W3+W4+W6+W9 as ONE `v*` tag (cache already at
  14, no bump). Flag the new `source` tokens
  (`boolean_union_operand`/`boolean_intersection_operand`) to the
  viewer-integrator (GH #20) when it lands.
- Deferred: W9 Phase 2 (slab decomposition for pockets/recesses), in-rep
  prism, the `TightPolygonalBoundaryIgnored` counter wiring (needs a
  warning channel orthogonal to the single per-product `Outcome`).
- GH #59 M1 oracle scaffold runs in parallel, no code conflict.

## Real-file validation (post-worklog) — GH #53/#56/#57

Tester lacks the Manifold/cmake toolchain, so ran the real-file cut
validation locally (built the wheel with `csg`+`prism-csg-fast`;
`ifcopenshell` 0.8.5 as oracle). Files: Sannergata ARK_E (local, 21 MB)
and G55_RIB / G55_RIB_Prefab (found non-zero copies under
`~/dev/oldpc1/...`; the acc-tree copies are 0-byte stubs). A test
sub-agent did the #53 deep-dive.

- **#53 — FIXED, shipped (`d292b3a`).** All 9 over-reporting walls were
  MISDIAGNOSED (by the reporter and my first read) as a cross-product
  cut-fold failure. Real cause: `mesh::brep::mesh_face` dropped inner
  `IfcFaceBound` holes and fan-filled the outer loop → over-filled brep
  faces by the hole area. 8 of 9 walls have no voids at all. Fix: honour
  inner bounds via Newell-projection + earcut-with-holes; cheap fan path
  kept for the hole-free majority. All 9 now match ifcopenshell to
  sub-rounding (was +6 … +122 %). 3 new brep unit tests; full suite
  green; cache 14→15. Benefits ANY baked-brep with punched faces.
- **#56 — resolved on current main, not by today's work.** All 5 Revit
  walls now match Solibri exactly; was ~0.0001 on 0.4.32. The brep fix
  can't raise volume, so this is the W4 operator-aware boolean fix
  (`f8b4cf2`): a `.UNION.` operand was being subtracted → sliver.
- **#57 — 8× over-report gone; ifcfast == ifcopenshell exactly.** Slab is
  an `IfcFacetedBrep` with 0 holes (today's fix is a no-op on it).
  ifcfast 131.74 == ifcopenshell 131.74 (was 1504 on 0.4.32, fixed by an
  earlier commit). Residual is ifcfast+ifcos vs **Solibri** 180.20 — a
  kernel disagreement, not an ifcfast defect; needs the reporter's exact
  file/Solibri solid to adjudicate (likely non-watertight brep where
  OCC-class vs tessellation kernels differ).

Takeaway: the brep-hole fix is the session's biggest correctness win and
should ride the Phase-1 bundle release. The `csg`+`prism-csg-fast` wheel
is built into `.venv` for further local validation.

## Notes

- The `cut_openings_proptest` still only exercises the in-rep `apply`
  path; extending it to the prism flush path needs IFC-text-per-case (for
  `extrude_params`), heavier than its microsecond/case design. The prism
  core's 13 volume tests + the 3 new differential integration tests cover
  correctness for the wiring; the proptest-prism variant + the
  ≥2×-speedup / ≤30%-fallback benchmark are promotion-gate concerns.
- No `AGENTS.md` / cache-schema change: no agent-visible primitive added
  (the `flush` signature is internal Rust; output segment + stats shape
  unchanged; the feature is off by default).
- GH #58 carries two landing notes from this session (W9 wiring, W6 plan).
