## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `e4145aa` → `<post-commit-sha>` (one commit this session)
- **Session scope**: pin GH #39 Sannergata divergence to ifcopenshell's `!AgreementFlag` flip and ship the fix
- **Touched paths**: `crates/core/src/mesh/boolean.rs`, `crates/core/src/mesh/cut_openings.rs`, `crates/core/src/mesh/halfspace_clip.rs`, `crates/core/src/mesh/mod.rs`, `crates/core/tests/cut_openings_integration.rs`, `crates/core/tests/mesh_reveal.rs`
- **Parallel sessions observed**: none on origin/main during the session window
- **Supersedes / superseded by**: continues `2026-06-03-20-19_clash-category-column-plus-halfspace-architecture.md` (which left the half-space architecture local-uncommitted with Sannergata still empty)

## Summary

Resumed the previous session's GH #39 cliffhanger — pure-Rust
plane-clipping primitive worked on synthetic deep BCRs but emptied the
Sannergata wall (`2Nf9lR2yz4n8M7t01TygaQ`, 13-deep tilted half-space
clipping tree). The three suspects logged in the prior worklog were
(a) `fit_halfspace` `D<0` skip, (b) ifcopenshell silently flipping the
agreement convention, (c) a rotation-of-position bug on tilted axes.

This session: traced ifcopenshell's `IfcHalfSpaceSolid` end-to-end and
confirmed **theory (b)**. Two-line fix flips the convention in
`boolean.rs`; two integration-test fixtures had `AgreementFlag` values
flipped so the same geometric outcome holds under the corrected
interpretation. Sannergata now reports 1.2497 m³ vs ifcopenshell's
1.2496 m³ — **+0.01 %** match. Wider sweep (388 walls): **97.2 %
within 5 %** of ifcopenshell, residual outliers belong to two
unrelated pre-existing bugs documented as follow-ups (see Next).

## Changes

### Layer 1 — convention flip in `mesh::boolean`

The smoking gun, found by reading ifcopenshell's source after the
ifcfast-vs-ifcopenshell volume diff:

`src/ifcgeom/mapping/IfcHalfSpaceSolid.cpp:33`
```cpp
f->orientation.reset(!inst->AgreementFlag());
```

`src/ifcgeom/kernels/opencascade/solid.cpp:47`
```cpp
const gp_Pnt pnt = pln.Location().Translated(
    face->orientation.get_value_or(false)
        ? pln.Axis().Direction()
        : -pln.Axis().Direction());
TopoDS_Shape halfspace = BRepPrimAPI_MakeHalfSpace(face, pnt).Solid();
```

`BRepPrimAPI_MakeHalfSpace(face, refPnt)` returns the half-space
CONTAINING `refPnt`. With `orientation = !AgreementFlag`:

- `.T.` → `orientation=false` → `pnt` on −normal side → half-space on
  −normal side → subtract → **keep +normal side**.
- `.F.` → `orientation=true` → `pnt` on +normal side → half-space on
  +normal side → subtract → **keep −normal side**.

This is the **opposite** of a strict reading of the IFC4 documentation
text ("if `.T.`, the half-space is in the direction of the surface
normal" → keep −normal side after subtraction). Every production engine
I have access to (ifcopenshell, web-ifc, the Revit round-trip behaviour
implied by the file) treats the spec text the way ifcopenshell does.

My code had the spec-strict interpretation. Two-line fix:

- `boolean::halfspace_solid` — swap the `if agreement` branches so the
  Y-180° rotation is applied for `.T.` (not `.F.`). Slab now sits on
  the SUBTRACTED side; `halfspace_clip::clip_by_plane` keeps the
  negative side of the slab-top normal → keep +normal side for `.T.`,
  keep −normal side for `.F.`.
- `boolean::polygonal_bounded_halfspace` — same swap, same reason.

Comment block in both functions cites the ifcopenshell line numbers as
authority. The Y-180° rotation has `det=+1` so windings are preserved
either way; only the side of the plane that the slab is built on
changes.

### Layer 2 — synthetic-test fixture flips

The two `mesh::cut_openings`-level integration tests asserted geometry
that lined up with the OLD (spec-strict) convention. Flipping the
fixture's `AgreementFlag` value preserves the geometric outcome under
the corrected convention:

- `halfspace_clip_with_agreement_true_keeps_lower_half`
  → renamed to `..._false_keeps_lower_half`. Fixture now
  `IFCHALFSPACESOLID(#52,.F.)`; doc comment now cites the de-facto IFC
  convention and references the ifcopenshell trace chain.
- `deep_bcr_with_three_halfspaces_cuts_correctly` — all three cutters
  in the synthetic 3-deep BCR fixture flipped from `.F.` to `.T.`,
  doc comment rewritten to describe what the de-facto convention does
  on each cutter (keep +normal side rather than my earlier strict
  reading).

### Validation

- `cargo test -p ifcfast-core --features mesh,csg,clash,bundle` —
  147 lib + 9 cut_openings + 13 mesh_reveal + 4 bundle + 4 clash, all green.
- `pytest` — 90/90 green.
- `ifcfast.open(Sannergata_ARK_E.ifc).meshes(cut_openings=True)`
  on wall `2Nf9lR2yz4n8M7t01TygaQ`: signed volume = 1.249694 m³.
  ifcopenshell on same wall (use-world-coords settings): 1.249649 m³.
  Diff +0.0001 m³ (+0.01 %). Pre-fix this wall was completely empty.
- 388-wall sweep on Sannergata: 377 within 5 % / 0.05 m³, 2
  under-reports (polygonal-bounded `Position`-frame bug), 9
  over-reports (cross-product cut path, large walls 6-130 m³).

## Technical Details

### Why the synthetic deep_bcr_with_three_halfspaces test passed under the wrong convention

The fixture used `.F.` flags with axis-aligned `+X`/`+Y`/`+Z` normals
and an explicit expectation `kept X∈[500,1000]` (i.e. keep +normal
side). My old code, treating `.F.` as "keep +normal side", happened to
match that. The fixture was constructed to validate the implementation
rather than the de-facto IFC interpretation — confirmation bias I
walked right into. The smoking-gun trigger was Sannergata, where two
clusters of cutters have opposing-direction normals, and the
spec-strict interpretation produces mutually-disjoint keep-regions.

### Why polygonal-bounded half-space walls still empty

Two outliers in the 388-wall sweep:
- `3_6AbaPP55CwroXTQjRwPB` — single `IfcPolygonalBoundedHalfSpace` cutter.
- `2G2LbZEmr1wwfAb0tg_3Ej` — similar pattern.

`IfcPolygonalBoundedHalfSpace` carries TWO independent
`IfcAxis2Placement3D`s:
1. `BaseSurface.Position` — defines the cutting plane's normal.
2. `IfcPolygonalBoundedHalfSpace.Position` — defines the local XY frame
   in which `PolygonalBoundary` (a 2D polyline) lives.

In `3_6AbaPP55CwroXTQjRwPB` these are radically different:
- BaseSurface.Position.axis = `(-0.02, 0, -0.9998)` (nearly horizontal,
  tilted slightly).
- IfcPolygonalBoundedHalfSpace.Position.axis = `(0, 0, 1)` (the default).

My code uses IfcPolygonalBoundedHalfSpace.Position as the slab's frame.
Result: the slab is extruded vertically (`+Z`) instead of along the
plane's actual normal. The first triangle's CCW world normal is `+Z`,
not the plane's `(-0.02, 0, -0.9998)`. `cut_openings` then clips the
wall by a horizontal plane near the wall's top, removing the entire
body.

This is **independent of the convention flip** — pre-fix, the same
wrong frame was being used; the wall just emptied in the opposite
direction. Filed as a follow-up issue rather than tangled into the
convention fix.

### The 9 over-reports

Large walls (6-131 m³ post-cut) where ifcfast reports +47 % to +121 %
above ifcopenshell. Pattern: most of them have BOTH an in-rep
`IfcBooleanClippingResult` AND cross-product `IfcRelVoidsElement`
openings. The cross-product cut path (`CrossProductCut::flush`) runs
`subtract_many` on the openings; if Manifold fails on any opening, the
host falls through with whatever volume it had after in-rep cut, which
may be much larger than the truly net-cut volume. These may correspond
to the original GH #39 `+75/+64/+37 %` over-report cases. Investigating
the cross-product path is separate work.

## Next

1. **Commit the half-space convention flip + the layer-1 architecture**
   (the prior session's local plane-clip module is still in this tree).
   Push to main. Bump version + tag for a v0.4.32 release that closes
   the worst-case face of GH #39.
2. **File GH issue for the polygonal-bounded `Position`-frame bug**.
   Title: "IfcPolygonalBoundedHalfSpace: use BaseSurface.Position for
   plane normal, not the polygon's local frame." Sannergata reproducers:
   `3_6AbaPP55CwroXTQjRwPB`, `2G2LbZEmr1wwfAb0tg_3Ej`.
3. **File GH issue for the cross-product over-report cluster** with
   the 9 sweep examples. Likely the same root cause as the original
   GH #39 +75/+64/+37 % cases.
4. Add a `gh issue comment` on #39 summarising what's now closed
   (deep-BCR half-space tree empty result) and what's deferred (the
   two follow-ups above).
5. **`#51` cargo-audit + Cargo.lock** remains the tester's top ask —
   cheap, high-value, fully independent.

## Notes

- The big lesson: don't trust the IFC4 documentation text on
  AgreementFlag. The text is ambiguous, the production implementations
  are unanimous, and they disagree with my literal reading of the text.
  Same shape as the indexed-poly-curve orientation work
  (Revit emits CW, the math expects CCW): the spec describes a
  convention; production tools have settled on a different one;
  match the tools, not the spec text.
- The 388-wall sweep is now the canonical "does the half-space path
  still work" check for Sannergata-shaped files. Worth keeping the
  script (`/tmp/sannergata_sweep_full.py` in this session) in
  `scripts/` for future regression catches — TODO.
- Two confirmation-bias artefacts caught: (1) the strict-spec synthetic
  test gave me false confidence the architecture was right, (2) the
  spec-strict reading of the IFC4 docs gave me confidence the
  interpretation was right. Real-file diff against ifcopenshell at the
  end was what surfaced the actual answer. Add a "do a real-file diff
  against ifcopenshell before declaring victory" step earlier on any
  future geometry-semantics change.
