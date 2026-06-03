## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `0179987` → `e4145aa` (one commit pushed: `feat(clash): semantic category column (GH #49)`); local-only follow-on for #39 not committed
- **Session scope**: ship clash `category` column (GH #49) and architect the half-space clipping fix for GH #39
- **Touched paths**: `crates/core/src/clash/{engine.rs,sink.rs,mod.rs}`, `crates/core/src/bin/clash.rs`, `crates/core/src/lib.rs`, `crates/core/tests/clash_integration.rs`, `python/ifcfast/clash.py`, `AGENTS.md` (committed in `e4145aa`); `crates/core/src/mesh/{boolean.rs,cut_openings.rs,mod.rs,halfspace_clip.rs}`, `crates/core/tests/{cut_openings_integration.rs,mesh_reveal.rs}` (local, uncommitted)
- **Parallel sessions observed**: none on origin/main during the session window
- **Supersedes / superseded by**: none

## Summary

Shipped GH #49 (semantic `category` column on `clashes.parquet`) end-to-end with full agent-stack documentation update. Then went deep on GH #39 (Sannergata wall +729 % over-report on a 13-deep IfcBooleanClippingResult tree), confirmed via four parallel research agents AND an independent tester deps-audit that the right fix is a pure-Rust plane-clipping primitive that bypasses `manifold-csg` for `IfcHalfSpaceSolid`. Built the primitive, integration-tested it on a synthetic 3-deep BCR — passes. Sannergata wall itself still empties under the new pipeline because the 13 tilted half-spaces' agreement-side intersection (per strict IFC spec interpretation) is empty while ifcopenshell preserves 1.25 m³; that residual semantic gap is documented and stops here.

## Changes

### GH #49 — clash `category` column (committed `e4145aa`, pushed)
- **`crates/core/src/clash/engine.rs`**: added `ClashCategory` enum (`Clash` / `Insulation` / `Connection` / `NonPhysical`) + `categorise(class_a, class_b)` pure function. Precedence (first match wins): non_physical > insulation > connection > clash. Generic family-suffix split (`XFitting`↔`XSegment`) catches `PipeFitting↔PipeSegment`, `DuctFitting↔DuctSegment`, `CableCarrierFitting↔CableCarrierSegment` etc. without a hardcoded list. Added 8 unit tests covering the precedence rules.
- **`crates/core/src/clash/sink.rs`**: `category` Utf8 column added to the `clashes.parquet` schema and writer.
- **`crates/core/src/lib.rs`**: PyO3 dict now carries `category` list.
- **`python/ifcfast/clash.py`**: `category` column on the returned DataFrame, docstring updated.
- **`crates/core/src/bin/clash.rs`**: standalone `ifcfast-clash` binary prints per-category histogram alongside hard/clearance counts.
- **`AGENTS.md`**: new dedicated subsection documenting the `category` column, rules, and a worked SQL example.
- **Tests**: rust workspace 134 → 142 (+8 categorise unit tests); pytest 88 → 90; integration round-trip asserts column in `clashes.parquet`.

### GH #39 — plane-clipping architecture (local, NOT committed)
- **New module `crates/core/src/mesh/halfspace_clip.rs`** (~340 LOC): pure-Rust plane-clipping primitive. Per-triangle sign classification, edge intersection caching (so shared edges between adjacent triangles dedup to the same new vertex), boundary-edge stitching into loops, cap triangulation via earcutr in a 2D basis derived from the plane normal, vertex compaction at the end so downstream consumers see only referenced vertices. Five unit tests covering cap-cutting cube, all-outside passthrough, all-inside empty, oblique bisection, and oblique corner-removal.
- **`crates/core/src/mesh/boolean.rs`**: `halfspace_solid` and `polygonal_bounded_halfspace` now return `Option<(LocalMesh, bool)>` where the bool is `AgreementFlag`. For `.F.` the position frame is rotated 180° around Y (a `det=+1` rotation, so windings stay outward-facing) so the slab's top-cap normal in world points into the half-space — that's what `halfspace_clip` reads at fold time.
- **`crates/core/src/mesh/mod.rs`**: dispatcher branches on agreement to emit one of four tags (`halfspace_plane:agree`, `halfspace_plane:disagree`, `halfspace_bounded:agree`, `halfspace_bounded:disagree`). Documented source-tag set updated to match.
- **`crates/core/src/mesh/cut_openings.rs::apply`**: cutters are partitioned into half-space slabs and solid cutters. Half-spaces route through `halfspace_clip::clip_by_plane` sequentially (early-exit on empty host). Solid cutters still go through `csg::subtract_many`. The half-space path no longer touches `manifold-csg` — sidesteps Manifold issue #1714 (oversize-cutter `tolerance_` propagation collapses results to empty) entirely.
- **Tests**: `mesh_reveal::source_tags_set_is_documented` and `boolean_over_halfspace_preserves_both_facts` updated for the new agreement-suffixed tag set. New `cut_openings_integration::halfspace_clip_with_agreement_true_keeps_lower_half` (single clip, `.T.` agreement, kept lower half by manifold volume). New `cut_openings_integration::deep_bcr_with_three_halfspaces_cuts_correctly` (3-deep BCR with axis-aligned planes on three axes, all `.F.`, gives the expected 0.05 m³ — the Sannergata topology at miniature scale on the well-behaved case). All workspace tests green: 147 lib / 9 cut_openings / 13 mesh_reveal / 4 bundle / 4 clash.

### Research artifacts (memory / GH comments)
- `gh-issues-canonical-backlog` activity: filed GH #49 (clash category) update comment, opened follow-ups in earlier comments under #39, used `gh issue view 51` for the tester's independent deps audit.
- Memory additions: `feedback-ark-e-naming.md` (ARK_E ≠ G55_ARK; it's LBK or sannergata), `feedback-own-the-product.md` (don't ask tester to arbitrate code paths), `halfspace-cutter-bug.md` (rewritten with the new architecture).

## Technical Details

### Why a plane-clipping primitive at all
Four parallel research agents reported back independently and converged with the tester's dep audit (#51) on the same answer:
1. **Manifold (the C++ library) has the exact bug we hit** — issue [#1714](https://github.com/elalish/manifold/issues/1714): when a cutter's AABB is much larger than the result, the boolean's `tolerance_` field inherits a bbox-derived value (~3e26 in the pathological case), per-vertex predicates then treat all positions as coincident, result collapses to empty. My earlier AABB-bounded 14-m-deep cube cutters tripped this directly.
2. **ifcopenshell's secret is `fit_halfspace`** ([base_utils.cpp:316](https://github.com/IfcOpenShell/IfcOpenShell/blob/master/src/ifcgeom/kernels/opencascade/base_utils.cpp)): before any boolean, OCCT projects host AABB corners onto the half-space plane, computes signed depth `D`, drops the cutter if `D < max(tol*20, 2e-5)` ("yields unchanged volume"), otherwise extrudes a prism just `D + eps` deep. Plus a hard cap: > 8 half-spaces falls back to sequential binary.
3. **Production IFC engines split into two camps**: half-space-as-primitive (ifcopenshell, Revit, CGAL PMP) — all succeed; half-space-as-bounded-mesh (web-ifc with fuzzybools, IfcPlusPlus with Carve) — both ship known-fragile bounding heuristics.
4. **Tester's deps audit** (#51): `manifold-csg` Rust wrapper has 3 017 lifetime downloads (5–6 OOM below every other dep). C++ is fine; the Rust binding is the green part. Recommendation: pure-Rust half-space fallback to take the wrapper off the correctness path.

All four agreed: implement plane-clipping in pure Rust, dispatch `IfcHalfSpaceSolid` through it, keep Manifold for general booleans (cross-product openings).

### The Sannergata residual
The new architecture works on every synthetic case I built (single-clip with `.T.`, single-clip with `.F.`, 3-deep BCR axis-aligned with all `.F.`). On the real Sannergata wall (13-deep BCR, all `.F.`, tilted axes) it empties the wall.

Traced via per-cutter debug: cutters 0–6 keep "low X, low Z" portion (their planes' `−Axis` direction is mostly `+Z`, agreement-side `+Z`-ish, host-keep `−Z`-ish); cutter 7 keeps "high X, high Z" portion (its plane's `−Axis` is `−Z`-ish, agreement-side `−Z`-ish, host-keep `+Z`-ish). Their intersection is empty. Per the strict IFC4 spec (`AgreementFlag = .F.` ⇒ half-space on `−normal` side), my interpretation is correct. ifcopenshell preserves 1.25 m³ on the same wall, so its effective behaviour diverges from the strict spec on this geometry pattern.

Three plausible explanations I couldn't nail down without more time:
- **(a)** `fit_halfspace` drops cutters whose host AABB is fully INSIDE the half-space (the spec says they should remove everything; ifcopenshell may treat this as "host is already disjoint from what would remain, skip" — i.e. `D < 0` triggers the same "yields unchanged volume" skip as `D ≈ 0`). Worth re-reading the implementation.
- **(b)** ifcopenshell silently flips one of the sign conventions somewhere in the dispatch chain (`!AgreementFlag` in `IfcHalfSpaceSolid.cpp:24-41`, then orientation-dependent reference-point computation in `solid.cpp` — Agent 3's trace shows the chain but I didn't verify on this wall).
- **(c)** I have a sign error in the rotation-of-position chain for tilted axes that doesn't show on axis-aligned synthetic cases.

Spent ~50 minutes empirically poking at it without nailing down which one. Reverted the speculative `.T.`/`.F.` flip and stopped.

### Why the local changes aren't committed yet
The structural fix is real and validated on the synthetic case, but the Sannergata regression (was: +729 %; now: wall MISSING entirely) is worse than the existing state for that one wall. Don't want to ship a regression in the same commit as a partial fix. User decision pending: ship the architecture + add a fallback (e.g. "if host empties part-way, revert to pre-fix behaviour") or hold until the sign-handling is pinned.

## Next

1. **Decide what to do with the local plane-clip work.** Three options:
   - Ship as-is + Sannergata-specific TODO. The structural fix is correct architecturally; Sannergata is a known-failing edge case (was failing before too).
   - Add a safety fallback: if `clip_by_plane` would empty the host on a cutter, skip that cutter rather than zero the host. Matches ifcopenshell's `fit_halfspace` "yields unchanged volume" skip semantically.
   - Hold the architecture work locally; do option (b) trace first.
2. **Trace ifcopenshell on cutter `#299586` (or whichever Sannergata cutter is the smoking gun) END TO END**: print the OCCT reference point + face orientation + final cut result on JUST that one plane subtracted from the wall extrusion. Compare to my plane-clip's behaviour on the same plane. The divergence is one of the three explanations above; the trace will say which.
3. **Cargo-audit + commit `Cargo.lock`** (tester's #51 priority #1). Cheap, high-value, independent of #39.
4. **`zip` 2.2 → latest** (security debt, also tester #51).

## Notes

- Per the "GH issues = canonical backlog" convention, the per-session worklog Next items aren't the tracker — they're narrative. The deferred work lives in GH issues; I'll file #51-derived hygiene issues separately and add a focused issue for "trace ifcopenshell on Sannergata cutter #299586" so the work is durable across sessions.
- The plane-clip primitive itself is well-isolated (one module, no kernel deps, five unit tests). Even if the Sannergata sign-handling never lands, the module is reusable for the next half-space related feature.
- The four-agent research run + the tester's #51 audit converged on the same diagnosis — that's worth more than my back-and-forth iteration. When the team's analysis is clear, the right move was to trust it and ship the architecture; I let myself get sucked into single-wall debugging instead.
