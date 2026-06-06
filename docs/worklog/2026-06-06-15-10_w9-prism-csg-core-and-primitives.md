# Session: W9 prism-CSG — i_overlay foundation, algebra core, cross-product primitives, architecture correction

## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `e338671` → `d45b1a9` (4 commits this slice)
- **Session scope**: after W3+W4, unblock + build the W9 pure-Rust prism-minus-prism CSG — i_overlay research, the 2D-boolean facade, the prism algebra core, the cross-product primitives, and the integration architecture
- **Touched paths**: `crates/core/src/mesh/{polygon_bool,prism_csg}.rs` (new), `crates/core/src/mesh/{extrusion,mod}.rs`, `crates/core/Cargo.toml`, `Cargo.lock`, `.github/workflows/csg-smoke.yml`
- **Parallel sessions observed**: none on `origin/main` during the session window
- **Supersedes / superseded by**: continues `2026-06-06-13-44_w3-w4-unit-tolerance-operator-aware-booleans.md` (same session, W9 arc)

## Summary

Continued GH #58 Phase 1 into W9 (the load-bearing manifold replacement for the prism class). Resolved the i_overlay blocker, built the 2D-boolean facade + the prism algebra core + the cross-product primitives, all behind the off-by-default `prism-csg-fast` feature (pure-Rust, not in the default dep graph — zero wheel-matrix impact until promoted). Used a `code-architect` sub-agent as an architecture sounding board, then corrected its key flaw from first principles and re-sequenced. 4 commits, all tested + `-D warnings` clean.

Mid-session the user gave a standing directive (saved as [[feedback-architecture-advisor-agent]]): take control of reversible architecture/scope calls, be ambitious / first-principles (no easy-80%), and consult a sub-agent architect rather than asking the user to arbitrate.

## Changes

- **`2099bc5` + `c54e4d8`** — `mesh/polygon_bool.rs`: thin facade over i_overlay 7.0 (`SingleFloatOverlay::overlay_as::<i64>`, `Difference`, `NonZero`, f64 near-origin). Quarantines the pre-1.0-churn dep behind one file. 6 tests. csg-smoke CI lane added (cross-compile proof on 5 platforms).
- **`65a1f21`** — `mesh/prism_csg.rs`: the prism algebra. `subtract`/`subtract_many` on `PrismParams` → `Cut | Empty | Unchanged | NotParametric`. Through-cut + perpendicular extrusion + parallel sweep axes → project footprints into a host sweep frame → 2D Difference → re-extrude. 10 tests, volume-validated.
- **`d45b1a9`** — cross-product primitives: `extrusion::extrude_params` (parse without tessellating; `extrude` delegates), `PrismParams: From<ExtrudeParams>`, `prism_csg::rebase_params` (F3-safe anchor rebase). 3 more tests incl. the F3 far-origin gate.

## Technical Details

- **i_overlay v7 research** (recorded in [[i_overlay-api]], GH #58). The audit's `Overlay::with_subj_and_clip` snippet was the *outdated integer API*; v7's float path is the `SingleFloatOverlay` extension trait. Decisions: `NonZero` fill (unions overlapping cutters; `EvenOdd` would XOR), f64 coords rebased near origin (i_overlay's `FloatPointAdapter` does adaptive float→i64 internally — manual scaling would fight it), `overlay_as::<i64>` for the finer grid. Empirically validated: `subject_with_hole_round_trips` (shape-with-holes input accepted) + `overlapping_cutters_union_via_nonzero`.
- **F3 frame contract** is the core W9 risk and is baked into `PrismParams` docs: all operands share ONE near-origin rebased frame; `local_xform` is the solid-local Position only, never the world ObjectPlacement. `rebase_params` moves a prism between mesh-anchor frames by the anchor *difference* — `cross_product_rebase_is_far_origin_safe` proves a cut between two products at ~600 km UTM stays correct (volume 45, finite) because only the small (<10 m) difference touches the f32 matrix.
- **Architecture correction.** Spawned a `feature-dev:code-architect` agent for a first-principles review of scope + plumbing. It proposed doing the in-rep cut inside `boolean_result` — which I rejected: `boolean_result` feeds the reveal-all ParquetSink too, so cutting there nets the substrate and violates reveal-all. The cut must stay sink-gated. Re-sequenced: **cross-product `IfcRelVoidsElement` leads** (both the ~90% Revit/ArchiCAD case AND the only cleanly sink-gated path — `CrossProductCut` exists only in cut mode); in-rep prism deferred (needs param-carrying to the post-bake layer; manifold covers it meanwhile).
- **Scope honesty:** through-cut covers the architecture/MEP majority; partial recess / pocket (Tekla notches, window sills) stay `NotParametric` → manifold for now, with slab decomposition explicitly scoped into Phase 2 — not silently dropped.

## Next

1. **Cross-product flush wiring** (mechanical now; primitives ready). `CrossProductCut::flush` tries `prism_csg::subtract_many` before `csg::subtract_many`. Crux: thread `&EntityTable` to the cut layer (or stash params at construction in lib.rs); recover params via `rep_step_id` on buffered `ProductMesh.parts`; compose bake-frame from `world_transform` + Position + anchor; rebase cutters via `rebase_params`. Validate with `cut_openings_proptest --features prism-csg-fast` + a benchmark for the promotion gate.
2. **W9 Phase 2 — slab decomposition** for partial recess / pocket (required before claiming the full prism class).
3. In-rep prism (deferred).
4. GH #56 / #57 tester response still open (reshapes W11 vs W15).

## Notes

- `prism-csg-fast` is pure Rust; `cargo tree -i i_overlay` confirms it's absent from the default dep graph. Its eventual promotion to default + dropping `csg` from default is the GH #28 (linux-aarch64) unblock — gated on Phase-2 telemetry showing the manifold fallback rate is small.
- The seam between disconnected result pieces from a splitting cut is coincident-but-unwelded (`append_mesh`); fine for volume/render, but if a downstream closed-manifold check trips on the prism path, add a weld pass. Flagged for the wiring slice.
