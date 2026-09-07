## Agent signature
- **Agent**: `claude-fable-5-1`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `5a34393` → `66711c4` (feature commit `66711c4`; this worklog fix-up follows)
- **Session scope**: GH #170 adaptive circle tessellation, GH #146 GUID-named glTF materials, GH #166 `by_source` on the Python surface — gated, bundled for 0.5.1
- **Touched paths**: crates/core/src/{entity_table.rs, lib.rs, mesh/{profile,indexed_curve,csg_primitive,boolean,curveset,gltf}.rs}, python/ifcfast/{model.py, header.py, data/AGENTS.md}, tests/{test_smoke.py, oracle/class_sweep.py, oracle/pipe_analytic.py}, AGENTS.md, CHANGELOG.md, scratch/g55/baselines/G55_*.json (local)
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: none

## Summary

### GH #170 — adaptive circle tessellation (cache schema 30 → 31)

Design, argued not assumed:
- Segment count per full turn `n = clamp(ceil(π / acos(1 − tol/r)), 8, 32)`,
  `tol` = 0.5 mm, `r` in metres through the declared `LENGTHUNIT`
  (new memoized `EntityTable::length_scale_or_init`, resolver mirrors the
  plane-angle one). Undeclared unit ⇒ 32 (today's output).
- **Inscribed sampling could not meet the oracle tolerance**: volume
  under-report is `2π²/(3n²)` — 0.64 % at 32, 2.6 % at 16, 10 % at 8 —
  so any n < 24 moves circular classes past ±0.005. Hence the
  **area-preserving radius** `k = sqrt(θ / (n·sin(θ/n)))` per sector
  (full turn: `sqrt(2π/(n sin(2π/n)))`), applied to circle / ellipse
  profiles, `IfcCircle` curves, trimmed conic arcs, `IfcArcIndex` 2D
  arcs, CSG cylinder / cone. Volumes become analytic for any n. 3D
  directrix arcs stay on the curve. Sphere: adaptive longitudes, latitude
  bands `clamp(n/2, 4, 12)` so it never gets heavier than before.
- First gate pass caught my own gap: open arcs were left inscribed and
  `IfcPipeSegment` on RIV (Revit two-semicircle profiles) drifted
  **−0.0082**. Fixed by the per-sector factor; re-sweep landed at
  +0.0064 (1.0027 → 1.0091), the predicted "ifcfast analytic, ios
  inscribed" signature.

Gate (release .so, `/oracle-gate`):
- Rust 430/0, clippy `-D warnings` clean, fmt clean.
- Class sweeps vs pre-change baselines: ARK / RIB / RIE **no drift**
  (IfcColumn +0.0030 / +0.0011 — circular columns, as predicted);
  RIV: IfcPipeSegment +0.0064, IfcDuctSegment +0.0057 (0.9968 → 1.0025,
  toward 1), IfcCovering +0.0046 (pipe insulation), Proxy +0.0025.
  **All attributed** to the area-preserving rule; baselines rewritten
  (`scratch/g55/baselines/pre170/` keeps the old ones).
- **Per-element proof** (new `tests/oracle/pipe_analytic.py`, no
  tessellation on the reference side): Clinic_Plumbing 2 882 pipes
  median ratio 1.00000; G55_RIV 10 704 hollow pipes 10 700 within 0.5 %,
  p1..p99 = 1.0000. Four centimetre stubs off → GH #173 (also: every
  annular pipe classifies `open_shell` although exact — same issue).
- pytest: full suite green incl. the 6 corpus-gated write-axis /
  round-trip tests (`IFCFAST_CORPUS` is a colon-separated FILE list,
  not a dir — the skill text is misleading, fixed in next-steps).
- Clash oracle, 5 Solibri rounds, fresh bundle caches: recall identical
  to baseline on every round (14/15, 18/19, 13/13, 2/2, 2/2 = 49/51);
  total pair counts −0.7 … −0.9 % (grazing pipe-wall contacts at
  tolerance 0 lost to coarser walls; no truth pair moved). Clash
  baselines rewritten to the new counts.

Measured effect (Clinic, `cut_openings=True`): pipes
(`IfcFlowSegment`) 2.3× fewer triangles; whole Plumbing model only
3.42 M → 3.22 M (−6 %) because valves / fittings are pre-tessellated
Revit family breps (one valve = 18 055 tris) → **GH #171** (instancing
is the free lever: `cut_openings=False` takes it 35 → 20 MB;
decimation for breps next).

### GH #146 — glTF materials named by product GUID again

`WriteOptions.per_product_materials` (default **true**): every baked
primitive gets its own material `"<guid>"` / `"<guid>#k"`, instanced
groups keep one colour-keyed material. `m.to_gltf(...,
per_product_materials=False)` for the colour-deduped `#rrggbb` output.
Materials list now carries names; three unit tests in
`gltf::material_naming_tests` (string-parsed JSON — no serde_json dep).
The site's sidecar post-process stopgap can retire once 0.5.1 ships.

### GH #166 — `by_source` on the Python surface

`set_mesh_stats` helper in lib.rs adds `products_seen` /
`products_deferred` / `by_source` (sorted `{tag: count}`) to all five
native mesh dicts; Python: `MeshList.stats`, `mesh_qto()[0].attrs
["mesh_stats"]`, `to_gltf` / `point_cloud` / `bundle` dicts.
`_mesh_stats()` helper in model.py. Smoke test covers #146 + #166.

### Harness
- `tests/oracle/class_sweep.py --refresh-fast`: recompute ifcfast, keep
  cached ifcopenshell volumes (a new build no longer costs a 15–40 min
  ios pass per model). Used for the second ARK/RIB/RIE pass.

## Evidence
- Clinic Plumbing per-entity tris: FlowController 254 × 6 289,
  FlowFitting 3 318 × 399, FlowSegment 2 893 × 54 (post-#170).
- pipe_analytic worst rows: `1IGP6vnyH9581zYmYZ$M05` 0.5696,
  `2x8mdq2MD4NQ5L80flwWX5` 0.6944, `1QauNCfP199Rqw3$WB2DLK` /
  `2gOHbx$0zFKxUuHs9rbZM6` 1.4581 — all ~1e-5 m³, all `open_shell`.

## Decisions recorded
- Ed: ifcfast.com "drop your IFC" must be **client-side (WASM)**, no
  backend → GH #172, after 0.5.1.

## Next
- Release 0.5.1 (bundle #170 + #146 + #166 + #144 harness fix if done).
- #171 instancing-with-cuts detection, then brep decimation.
- #173 annulus open_shell classification.
- #172 wasm build.
