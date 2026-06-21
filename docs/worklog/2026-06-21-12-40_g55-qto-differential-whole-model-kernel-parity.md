# Session: G55_RIB QTO differential → whole-model parity with the ifcopenshell kernel (coordinator + dual swarms)

## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `24eec18` → `add0465` (4 commits this segment; continues the same long session whose first worklog was `24eec18`)
- **Session scope**: Drive a ground-up ifcfast-vs-ifcopenshell QTO differential on G55_RIB to chase the "180 vs 130" discrepancy — found & fixed a self-inflicted slab regression and the #114 bounded-halfspace over-clip, reaching whole-model kernel parity, and built the geometry oracle that gates it.
- **Touched paths**: crates/core/src/mesh/{qto,boolean,cut_openings,halfspace_clip,mod}.rs, tests/oracle/{_geom_adapter,test_geometry_oracle}.py, tests/fixtures/geom_box.ifc, docs/worklog/
- **Parallel sessions observed**: none (all commits on origin/main are this session's)
- **Supersedes / superseded by**: continues `24eec18` (same session)

## Summary
A by-hand, full-model differential of ifcfast `mesh_qto` vs the ifcopenshell geometry kernel on `scratch/g55/G55_RIB.ifc` took the model from −0.5%/+9.8% noise to **+0.0% whole-model parity** — every element type now matches the kernel to rounding. Along the way it caught a **regression we shipped earlier this same session** (the lower-bound tripwire) that the synthetic adversarial test had passed, and pinned #114 to its true cause (the default cut path ignoring `IfcPolygonalBoundedHalfSpace` bounds). Two parallel swarms then landed the durable gate (geometry oracle) and the fix (W6 bounded clip), with the coordinator owning the one privileged rebuild so the swarms never collided.

## Changes
- **`73fcf28` tripwire regression fix** (`qto.rs`): `LOWER_FRAC` 0.5→0.1. The lower-bound collapse tripwire (shipped `bb979b4` earlier this session) assumed the prism bound is trustworthy; on an `open_shell IfcFacetedBrep` slab whose prism over-counts 2.6× (342 vs true 131.74), a *correct* mesh read as fill 0.385, tripped "collapsed", and emitted the inflated prism — the whole-model error was +9.8%, all from one slab. Tightening to 0.1 (above zero-collapse, below any legitimate fill) restored 131.74 = ios.
- **`24661b2` geometry oracle adapter** (`tests/oracle/_geom_adapter.py`, `test_geometry_oracle.py`, `tests/fixtures/geom_box.ifc`): `diff_geometry_volumes(path)` — per-element `mesh_qto.volume_m3` vs ifcopenshell kernel (signed-tetra f64), keyed by guid, typed DisagreementRecords. Reusable on any file (used by hand on G55). New 24 m³ committed fixture (no existing one had meshable geometry). Non-vacuous + clean-skip proven.
- **`add0465` W6 bounded-halfspace fix** (`boolean.rs`, `mod.rs`, `cut_openings.rs`, `halfspace_clip.rs`): the default (`csg`, non-`prism-csg-fast`) cut path derived an *infinite* plane from the slab centroid and never read the bounding polygon (boundary read + payload were gated behind `prism-csg-fast`), so edge-trim clips sheared full wall thickness. Fix: always carry the `BoundedHalfspacePayload`, build a *finite* boundary-column cutter (polygon → cutting-plane basis → extrude through removed-side extent → csg subtract). prism-csg-fast untouched.
- **Tracker**: #114 corrected (precision → void → cross-product → **bounded-halfspace**, each disproved in turn) then fixed; **#117** (mm-UTM f32 storage ceiling), **#118** (mesh_qto+sql MCP), **#119** (prism footprint over-count) filed.

## Technical Details
- **Method = coordinator + adversarially-honest differential.** The "180 vs 130" was slab `0KQH…Msew` (ios 131.74). The diagnosis zig-zagged (I was wrong twice — cross-product, then "prism-csg-fast caused the slab") and the user's "from the top" full-model view (all types, not walls only) is what exposed both the slab regression and that prism-csg-fast made the *total* worse (+9.8%) even while fixing walls.
- **Build discipline**: `maturin develop` (DEBUG, never `--release` — the documented OOM trigger) is ~8 s incremental and RAM-flat; used it to A/B `prism-csg-fast` and verify the W6 fix. Volumes identical debug vs release. The two final swarms ran in parallel on disjoint trees (`tests/oracle/**` vs `crates/core/src/**`), neither ran `maturin`, and the coordinator did the single rebuild — so the shared venv `_core` was never clobbered mid-pytest.
- **The two "collapse" walls (0.08 m³) were never a separate bug** — same unbounded-clip over-removal; the W6 fix corrected all 46 walls to exact kernel match at once.

## Next
- **Wire the G55/Sannergata corpus sweep through the geometry oracle** — the CI oracle runs on tiny fixtures; the real gate is the corpus differential (needs corpus-path decision: Sannergata external, G55 on ACC). This is the durable fix to the process gap that bit us.
- **#119 prism footprint over-count** — the slab's prism still reads 2.6× (760 m² vs ~293). Latent fragility under any prism fallback; root of the tripwire's brittleness.
- **prism-csg-fast +0.69 over-report** on ~5 walls (its 2D region-decomposition path) — moot for default now, but blocks promoting prism-csg-fast.
- Continue 0.5.0: **#118** mesh_qto+sql MCP; then tag v0.5.0 on oracle-clean.

## Notes
- **The defining lesson:** our synthetic adversarial refuter PASSED the tripwire that regressed the biggest element in the model — because it wrote its own fixtures with *sane* prism bounds. Only the real-corpus differential caught it. Synthetic self-fixtures structurally can't catch "the reference itself is wrong" bugs. Corpus differential (the oracle) must gate, not synthetic tests. This is now baked into the geometry oracle.
- Build state: venv on default debug build with the W6 fix. `__version__` prints 0.4.38 (cosmetic; `_core` is current).
- `mesh_qto` is computed live (not in the schema cache); the substrate uses raw operand-split volumes (never `cut_openings`), so this fix needed no `_CACHE_SCHEMA_VERSION` bump or AGENTS.md change.
