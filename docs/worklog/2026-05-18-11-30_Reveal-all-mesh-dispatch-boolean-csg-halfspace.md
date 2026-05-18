# Session: Reveal-all mesh dispatch — boolean / CSG / halfspace handlers

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `787afe8` → (pending) (1 commit prepared, not yet pushed)
- **Session scope**: Close the silent `Vec::new()` dispatch fall-through in `mesh/mod.rs::mesh_item` by adding real handlers for the composite / halfspace representation types and bucketing anything still unhandled with its IFC type name. No subtraction, no curation — reveal-all stance.
- **Touched paths**: `crates/core/src/mesh/mod.rs`, `crates/core/src/mesh/boolean.rs` (new), `crates/core/src/mesh/brep.rs`, `crates/core/src/mesh/extrusion.rs`, `crates/core/src/mesh/mapped.rs`, `crates/core/src/mesh/gltf.rs`, `crates/core/tests/mesh_reveal.rs` (new), `AGENTS.md`, `README.md`
- **Parallel sessions observed**: `none` (`git log origin/main --since='2026-05-18'` returned empty during the window)
- **Supersedes / superseded by**: `none`

## Summary

Session opened intending to fix opening-vs-host visual overlap via boolean subtraction (issue #4). User reframed the philosophy mid-session: **"We are trying to reveal IFC. Not be opinionated. Reveal ALL so that we can understand and make decisions and move towards being able to surgically model using code. Model and edit."** That flipped the goal from "make the demo look clean" to "make the parser an X-ray that loses nothing on the way in." Both operands of every boolean now emit as their own visible mesh segments; the door-overlapping-wall stays visible because the file actually says that.

## Changes

**`crates/core/src/mesh/mod.rs`** — core rewrite:
- New `MeshFragment` enum: `Mesh { mesh, source, role }` or `Unhandled { ifc_type }`. The dispatcher returns one or more of these per representation item; never a silent drop.
- New `MeshSegment { index_start, index_count, source: String }` on `ProductMesh`. A wall built from `IfcBooleanClippingResult(extrusion, halfspace)` now carries TWO segments tagged `"boolean_first_operand|extrusion"` and `"boolean_second_operand|halfspace_bounded"` — both the structural role and the leaf representation type survive into the output. Compound `role|leaf` format keeps both facts visible without losing back-compat for consumers that read just `.source`.
- Removed the door / window scale-up z-fight workaround (was at lines 130-141). Under reveal-all, intersecting volumes are honest output, not a bug to mask.
- Removed the `IfcOpeningElement` silent skip (was at lines 88-96). Opening meshes now appear in `m.products` and contribute their own ProductMesh; the visible overlap with the host wall IS the file's truth — consumers that want a "clean" scene can toggle by entity downstream.
- `is_composite` cache logic now also includes `IFCBOOLEANRESULT` / `IFCBOOLEANCLIPPINGRESULT` / `IFCCSGSOLID` so the outer composite node isn't cached (operand-level caches still hit).
- Unhandled types bucket as `stats.by_source["unhandled:IFCXXX"]` with the explicit IFC type name so the consumer sees exactly what we couldn't tessellate.

**`crates/core/src/mesh/boolean.rs`** — new module with four handlers:
- `boolean_result` — `IfcBooleanResult` / `IfcBooleanClippingResult`. Recurses into both operands via a `mesh_item` function pointer, tags each fragment with its operand role using a `role`-preserving retag (outer composites don't overwrite inner roles, so the innermost / most-specific role wins).
- `csg_solid` — `IfcCsgSolid`. Recurses into the `TreeRootExpression`, tags fragments as `"csg_branch"`.
- `polygonal_bounded_halfspace` — `IfcPolygonalBoundedHalfSpace`. Extracts the polygonal boundary (`IfcPolyline` or `IfcIndexedPolyCurve`), extrudes it through the base plane as a finite slab tagged `"halfspace_bounded"`. This IS a real bounded volume — the polygon clip Revit emits is now visible geometry.
- `halfspace_solid` — `IfcHalfSpaceSolid`. Unbounded by definition; we emit a large square cap (20m × 20m) on the base plane, tagged `"halfspace_plane"`, so the consumer can SEE the cutting orientation. The finite extent is a conceptual stand-in — the tag declares that honestly.

**`crates/core/src/mesh/brep.rs`** — wired `IFCADVANCEDBREP` to the existing `faceted_brep` path (which already accepts `IFCADVANCEDFACE` in the inner face walker). Curved surfaces are approximated by triangulating the face's outer poly-loop; the dispatcher tags this as `"advanced_brep_approx"` so the curvature loss is declared. Real BSpline tessellation is a future pass.

**`crates/core/src/mesh/mapped.rs`** — `expand()` now returns `Vec<MeshFragment>` instead of `Vec<(LocalMesh, &'static str)>`. Unhandled fragments inside a mapped expansion pass through unchanged so they reach `stats.by_source`.

**`crates/core/src/mesh/extrusion.rs`** — derived `Debug, Clone, Default` on `LocalMesh` (the new `MeshFragment::Mesh` variant uses `#[derive(Debug)]`).

**`crates/core/src/mesh/gltf.rs`** — per-node `extras` now carries a `segments: [{start, count, source}, ...]` array. Lets a glTF viewer split / colour / filter each product's triangles by representation role without needing a Python-side change.

**`crates/core/tests/mesh_reveal.rs`** — new integration tests, all 4 passing:
- `boolean_clipping_emits_both_operands_as_segments` — synthetic wall + door clip, verifies two segments tagged with the operand prefixes, both with positive triangle counts, no gaps in the index buffer.
- `boolean_over_halfspace_preserves_both_facts` — synthetic wall clipped by `IfcPolygonalBoundedHalfSpace`, verifies the compound tag is exactly `"boolean_second_operand|halfspace_bounded"`. The whole point of the compound tag: losing either fact (just role, just leaf) would mean reveal-all leaked.
- `unhandled_representation_appears_as_labeled_bucket` — synthetic `IfcRevolvedAreaSolid` product, verifies `stats.by_source["unhandled:IFCREVOLVEDAREASOLID"] >= 1`. Names the missing handler explicitly.
- `source_tags_set_is_documented` — `MeshFragment::source_tags()` keeps the list of known tags honest; future handler authors must register their tag here.

**`AGENTS.md` / `README.md`** — bundled my philosophy-stance section ("Reveal-all geometry stance" + "What ifcfast does NOT do" / north-star) with the WIP edits that were left for me from the prior session.

## Verification

- `cargo test --features mesh --no-default-features -p ifcfast-core` → 4 passed (new mesh_reveal tests) + 0 lib tests.
- `cargo build --no-default-features --features "python mesh"` → clean.
- `.venv/bin/maturin develop --release` + `pytest tests/ --ignore=tests/test_mcp_server.py` → 58 passed, 0 failed. Full Python suite still green.
- Real-file smoke test on Duplex (7 `IfcBooleanClippingResult` × 7 `IfcPolygonalBoundedHalfSpace`): `cargo run --release --bin ifcfast-mesh -- duplex.ifc` reported `boolean_second_operand|halfspace_bounded × 7` and `boolean_first_operand|extrusion × 4` in `by_source`. (4 vs 7 first operands is because 3 outer-boolean first-operands are nested booleans that get the innermost role instead — by design, not a bug.)
- glTF extras verified by reading back `/tmp/duplex.glb`: `IfcWallstandardcase` nodes now expose `extras.segments = [{source: "boolean_first_operand|extrusion", ...}, {source: "boolean_second_operand|halfspace_bounded", ...}, ...]`. Viewer can colour by segment without Python changes.

## Next

- **Push the commit.** Pending user OK (per CLAUDE.md "always confirm push").
- **Close issues #4 and #5** on `EdvardGK/ifcfast` once pushed, with the reveal-all stance explained — `#4` (boolean subtract) becomes "WONTFIX, by design: we reveal both operands instead"; `#5` (expand representation coverage) is partially closed (`IfcAdvancedBrep`, `IfcPolygonalBoundedHalfSpace`, `IfcHalfSpaceSolid`, `IfcBooleanResult`, `IfcCsgSolid` now handled) but the primitive leaves (`IfcBlock`, `IfcSphere`, `IfcRightCircularCylinder`, `IfcRectangularPyramid`, `IfcRightCircularCone`) and swept-area variants (`IfcRevolvedAreaSolid`, `IfcSurfaceCurveSweptAreaSolid`) remain → file a follow-up tracking issue listing each from the live `unhandled:` buckets.
- **Site update**: the demo's "openings overlap walls" warning row should now self-disable once the new parser version is published and the site regenerates sidecars / re-renders meshes. Site-side polish (issues #11-#14) from the prior session is still uncommitted in `~/workspace/inbox/ifcfast-site/`.
- **Expose `segments` through the Python `analyse_drift` surface** so Python consumers (not just glTF viewers) can read the per-segment role. Small follow-up.
- **North-star next milestone**: round-trip editing. Read → modify → write. Requires per-entity byte-offset preservation and a deterministic STEP serialiser. Tracked in user direction "model and edit"; out of scope for this session.

## Notes

- The `mesh_item` recursion for `boolean_result` and `csg_solid` is wired via a `&dyn Fn` function pointer to avoid the borrow-checker churn of passing `&mut shape_cache` through a closure. The cost is one indirect call per composite node — negligible relative to the tessellation work.
- `MeshFragment::source` stays `&'static str` (no String alloc on the hot path). Compound tags happen at fragment→segment conversion via one `format!("{}|{}", role, source)` per segment — Duplex has only 14 boolean operands total, so cost is irrelevant.
- The retag policy is "innermost role wins". Alternative would be "outermost wins" — pick this if downstream needs the outer narrative. The choice is in `boolean::retag()` at one line; flip if needed.
- `IfcHalfSpaceSolid` (unbounded) emits a 20m × 20m × 0.01mm quad cap. Any value here is arbitrary; the tag declares it's a finite stand-in. If a project has an unusually large extent, the cap might not reach far enough — at that point upgrade the handler to compute the cap from the project's world bbox.
