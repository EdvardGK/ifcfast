# Cut-openings + manifold-csg replacement plan

Authored 2026-06-05. Companion artefacts: `docs/2026-06-05_manifold-replacement-audit.json` (raw multi-agent audit output, 45 agents, ~2.5M tokens, 22 minutes wall-clock); oracle harness direction in **GH #59** (ifcopenshell as differential-testing oracle — a third validation voice alongside the proptest harness).

> **Direction update (2026-06-06).** This document has two layers. The **audit findings** (F1–F6), the **sequenced work plan** (W1–W17), and the **risks** below are unchanged and still load-bearing — they are the factual base. What flipped on 2026-06-06 is the **top-level decision** and the **feature-flag endgame**: the target is now **full retirement of `manifold-csg` from default builds in 2–3 months**, not "keep manifold this cycle." The TL;DR and "Feature flag strategy" sections below have been rewritten to reflect this; everything between is the original audit and remains accurate. Rationale for the flip lives in the [[manifold-replacement-direction]] memory and worklog `docs/worklog/2026-06-06-01-08_manifold-strategy-update-full-retirement-target.md`.

## TL;DR

**Target: full retirement of `manifold-csg` from default builds in 2–3 months**, via a specialized IFC-CSG implementation across the IFC opening/host vocabulary, with manifold staying available behind an opt-in feature flag indefinitely as the long-tail escape hatch.

The path to that target is exactly the audit's multi-release programme — **foundation refactors first, then narrow, well-validated, default-on correctness fixes** — but the endgame is reframed: each specialized path that the audit scoped as "replace where the math reduces cleanly" (prism-prism via 2D Boolean + extrude, halfspace via clip) is now a step toward dropping the C++ dep from defaults, not a permanent supplement to it. The audit refuted the most ambitious *single-shot* replacement designs (20 of 32 verdicts refuted, including all four lenses on `brep-minus-prism` and `general-fallback`) — that refutation stands. It argues for **incremental, per-shape specialization validated against the proptest baseline**, not for one big rewrite, and not for keeping manifold in the default forever. The general brep × brep long tail (the audit's irreducible core) is deferred to a thesis-port decision gated on real-corpus telemetry over 4–8 weeks of Phase-1+2 operation; if the long tail doesn't materialize in practice, manifold is never needed in a default build.

**Why the conservative framing flipped:** the "5 years of single-dev Manifold" defence collapsed under inspection — most of those years are engineering execution / bug-reports / perf / bindings that AI-assisted dev compresses, while the irreducibly hard content (Lalish's manifold-preservation invariant) is documented in his thesis and portable under property-based validation in months, not years. The proptest harness shipped 2026-06-05 (commit `3092ae4`, `tests/cut_openings_proptest.rs`) is the load-bearing validation gate: any pure-Rust path that doesn't match its 1024-case baseline + analytic invariants does not ship.

**Phase structure (revised):**

- **Phase 1 (2–3 weeks)** — W3 + W4 + W6 + W9. After Phase 1, ~95 % of real IFC inputs run pure-Rust; manifold falls back only on brep + csg-primitive cutters.
- **Phase 2 (3–6 weeks)** — W11 + W12, *reframed from "validate-then-fall-back-to-manifold" to "replace for the cases we can specialize"*: cylinder → 24-gon prism dispatch; block → rectangle prism; brep with convex connected-components → W9 path; non-convex CC → long-tail specialized handler. After Phase 2, 99 %+ of real IFC inputs run pure-Rust.
- **Phase 3 (defensive, ongoing)** — `manifold-csg` moves behind an opt-in feature flag; default builds drop the C++ dep; linux-aarch64 wheel-build (GH #28) unblocks. General brep × brep is the thesis-port decision, attempted only if Phase-1+2 telemetry shows the long tail is real.

Three independent reasons for the inversion:

1. The chain-tag mental model that several proposed designs rely on **does not exist in the codebase today**. `mesh::boolean::retag` is `role.unwrap_or(new_role)` — innermost-wins, two tokens. Every reader that scans the source chain at depth is reading fiction. This is foundational; fix it first.
2. `unit_scale` is not propagated into any CSG tolerance. US-imperial Revit files silently misbehave across every kernel adapter. Hard-coded `ON_PLANE_EPS = 1e-3` in `halfspace_clip.rs` is mm-shaped; for files in metres or feet it is the wrong magnitude.
3. New parametric sidecars (PrismParams, ExtrusionParams) that the replacement designs proposed were not specified against `BakeFrame` / `mesh_anchor`. UTM-scale Norwegian projects would corrupt silently — the same class of bug that the v0.4.15 anchor work fixed for vertex buffers, re-introduced through new metadata.

The prior-art consensus from the tool comparison (ifcopenshell-OCCT, ifcopenshell-CGAL, web-ifc, Bonsai) is that **3D mesh CSG is the cost everyone pays for the general brep × brep case**. Only halfspace and prism-aligned cases reduce to cheaper algebraic primitives. That matches our existing `halfspace_clip` and aligns with the proposed `prism-prism` work, but does not support replacing manifold wholesale.

## Methodology

A workflow ran 45 agents across five phases:

- **Map** (3 parallel): inventory current `manifold-csg` call sites; map the IFC opening/host shape vocabulary; cross-reference how other open-source IFC tools (ifcopenshell, web-ifc, BlenderBIM, ifc.js, BIMserver) handle each combination.
- **Design** (8 shape categories, pipelined): per-category pure-Rust algorithm sketch, prerequisites, edge cases, LOC estimate, confidence rating.
- **Verify** (32 verdicts, 4 lenses each): adversarial review through `correctness`, `robustness-on-degeneracy`, `performance`, `maintenance-cost`.
- **Critic**: completeness check — what shapes, failure modes, and cross-cutting concerns were missed across the prior phases?
- **Synthesize**: rolled into the work plan below.

20 of 32 verdicts came back `refuted: true`. That is the audit doing its job: ambitious designs that survive adversarial review become work; designs that get refuted on all four lenses (brep-minus-prism, general-fallback) get downgraded to "keep manifold here".

## Headline findings

### F1 — Source-chain encoding is two tokens, readers assume N

`mesh::boolean::retag` (crates/core/src/mesh/boolean.rs:297-308) does:

```rust
role: Some(role.unwrap_or(new_role))
```

That is innermost-wins. Outer roles are dropped. At serialization the role is joined with the source by `format!("{}|{}", role, source)`, yielding at most two tokens.

Readers like `cut_openings::is_cutter` scan `source.split('|').any(|link| link == "boolean_second_operand")`. For a 3-deep nested boolean where the innermost role is `boolean_first_operand`, the chain ends up as `boolean_first_operand|extrusion` — the outer `boolean_second_operand` annotation is gone. `is_cutter` returns false; the cutter is misidentified as host. Multi-level boolean trees are not classified correctly. The fact that Sannergata-class fixtures pass today is statistical — they happen to be configured such that the innermost role is correct.

This is the single most leverage-rich bug in the cut-openings pipeline.

### F2 — `unit_scale` does not reach CSG tolerances

`halfspace_clip.rs` hard-codes `ON_PLANE_EPS = 1e-3`. That's 1 mm in a metre-scale file, 1/1000 mm in a mm-scale file, and 0.001 ft in an imperial file. Three different physical tolerances for the same constant. The indexer surfaces `unit_scale` (mm→0.001, m→1.0, ft→0.3048) and `mesh_qto` consumes it, but **none of the CSG kernel adapters consult it**.

Pure-Rust replacements proposed in the audit (i_overlay, csgrs) would inherit the same blindness if shipped without a unit-aware tolerance policy.

### F3 — Parametric sidecars violate the mesh_anchor invariant

The v0.4.15-v0.4.16 anchor work established that **all coordinates in kernel inputs are mesh_anchor-local** so far-from-origin (UTM-scale) projects don't collapse under f32. Vertex buffers respect this. The proposed `PrismParams { profile_origin, sweep_direction, placement_mat4 }` sidecars were specified without that constraint — `placement_mat4` would carry raw f32 world coords. On Norwegian UTM projects (typical east coord ~600 000 m), the lossy bits in the matrix are the meaningful ones.

If we ship parametric sidecars, the rebase contract has to be in the type signature, not aspirational.

### F4 — IfcBooleanResult UNION/INTERSECTION ignored

`mesh::boolean::boolean_result` parses operands but does not consume `fields[0]` (the operator). Every IfcBooleanResult is treated as if it were IfcBooleanClippingResult (DIFFERENCE). When the file says `.UNION.` or `.INTERSECTION.`, the result is reveal-all of both operands, which produces:

- For UNION of overlapping solids: doubled overlap volume (silently wrong, ~2×).
- For INTERSECTION: complement of intended geometry — visibly broken on the rare-but-legal `wall = roof_volume ∩ wall_extrusion` pattern.

### F5 — IfcMappedItem post-cut cache invariant

When two product instances share an IfcRepresentationMap and one has IfcRelVoidsElement and the other doesn't, the cut output mutates the shared representation cache in-place. The next instance reads the post-cut mesh. `representations.parquet` carries a row that is correct for instance A but wrong for instance B. `instances.parquet` fingerprint columns (centroid/vertex/triangle, GH #19) inherit the error.

### F6 — Polygon-bounded halfspace polygon is currently ignored

`polygonal_bounded_halfspace` reads the polygon boundary but `cut_openings::apply` runs the resulting slab through `halfspace_clip::clip_by_plane`, which is an **infinite-plane** clip. The polygonal boundary on the slab is metadata that the clipper does not consume. We get away with this because Revit emits oversized boundaries (5×5 m around the actual clip region) — the boundary doesn't constrain the cut. Authors that emit tight polygonal bounds (ifcopenshell, hand-authored fixtures) get over-cut. The "97.4% Sannergata match" celebrated in [[halfspace-cutter-bug]] is not a vindication of the algorithm — it is luck that the test corpus uses the oversize convention.

## Sequenced work plan (W1–W17)

The plan is ordered such that any prefix is a coherent, shippable codebase. Each item carries dependency, risk, effort, and rollout.

| ID | Title | Depends | Risk | Effort | Rollout |
|---|---|---|---|---|---|
| W1 | Real source-chain encoding | — | medium | days | default-on |
| W2 | Unified Outcome / UnsupportedReason taxonomy + stats | W1 | low | days | default-on |
| W3 | Unit-aware tolerance policy + cross-cutting validation gate | W2 | medium | days | default-on |
| W4 | Operator-aware IfcBooleanResult (UNION/INTERSECTION semantics) | W1, W2 | low | hours | default-on |
| W5a | Property-based generator harness (`tests/cut_openings_proptest.rs`) — **shipped 2026-06-05** | W2 | low | hours | default-on |
| W5 | Fixed-corpus regression suite | W2 | low | days | CI gate |
| W6 | Polygon-bounded halfspace correctness fix | W3, W5 | medium | days | default-on |
| W7 | Anchor synchronization for cross-product cuts | W3 | medium | days | default-on |
| W8 | Tapered extrusion + boxed halfspace + typed-Unsupported handlers | W2, W3 | low | days | default-on |
| W9 | Prism-prism 2D-Boolean fast path (narrow, gated) | W3, W5, W6 | high | weeks | feature-flag opt-in |
| W10 | csgrs production-readiness probe (benchmark only) | W5 | low | days | benchmark |
| W11 | Brep cutter pre-flight validator + typed Unsupported | W3, W5 | medium | days | default-on |
| W12 | Conditional polygon-bounded prism csg for csg-primitive cutters | W9 | medium | days | feature-flag |
| W13 | Halfspace-clip sequential optimizations + arena | W3 | low | days | default-on |
| W14 | IfcMappedItem post-cut cache invariant | W2, W5 | medium | days | default-on |
| W15 | csgrs vs manifold tier-B decision (gated on W10) | W10, W11 | high | days | feature-flag |
| W16 | AGENTS.md + cache schema bump consolidation | W1–W4, W14 | low | hours | release |
| W17 | Long-tail handler followups (AdvancedBrep, SweptDisk, RevolvedAreaSolid host) | W3, W11 | low | weeks | distributed |

Full per-item scope (files touched, behaviour change, gates) is in `docs/2026-06-05_manifold-replacement-audit.json` under `result.plan.sequenced_work_items`.

## Recommended first PR

**W1 + W2 combined: `feat(csg): unified Outcome::Unsupported taxonomy + real source-chain encoding`**

Scope (~400 LOC plus tests + docs):

1. `crates/core/src/mesh/boolean.rs` — refactor `retag` from `Option<&'static str>` innermost-wins to chain accumulation (`SmallVec<[&'static str; 6]>` or similar). Add `chain_contains(seg, link)` / `chain_count(seg, link)` helpers.
2. `crates/core/src/mesh/mod.rs` — update the `format!("{}|{}", role, source)` site to use chain accumulation.
3. `crates/core/src/mesh/cut_openings.rs` — update `is_cutter`, `is_halfspace_cutter`, and any `source.split('|')` reader to use the new helpers. Add `Outcome::Unsupported(UnsupportedReason)` variant.
4. `crates/core/src/mesh/stats.rs` — extend `CutOpeningsStats` with per-reason counter fields (flat, not HashMap, for FFI cleanliness).
5. `crates/core/src/lib.rs` — update 3 stats accumulator call sites.
6. `python/ifcfast/header.py` — bump `_CACHE_SCHEMA_VERSION`.
7. `AGENTS.md` — document the new taxonomy + chain encoding contract.
8. Tests — add chain-token snapshot tests on existing integration fixtures BEFORE the refactor (documenting OLD behaviour), then update in the same PR (documenting NEW behaviour). One new unit test exercising `chain_count` on a 3-deep synthetic boolean.

Three reasons this earns the front-of-queue slot:

- F1 is foundational. Any downstream work that scans source chains at depth (composite-trees, cross-product-relvoids, prism-cutter dispatch, csg-primitive recognition) is building on fiction until this lands. Coding against the fiction means rewriting once the fiction collapses.
- The `Outcome::Unsupported` taxonomy is the single highest-leverage agent-facing change. Today's opaque silent `Fallback` becomes a typed signal callers can route on, with zero behaviour change for callers that don't inspect the reason.
- Small, low-risk, foundation-laying — exactly what the plan demands at the front. ~400 LOC. Behaviour-preserving on the happy path (validated by the existing integration test suite).

## Feature flag strategy

Current: `default = [..., "csg"]` where `csg` brings in manifold-csg (C++ dep).

Target progression (revised 2026-06-06 — endgame is manifold-out-of-default, not manifold-forever):

- **Phase 1 foundation (W1–W8 + W13 + W14 releases)**: `default = [..., "csg"]` unchanged. Foundation + correctness + typed-Unsupported + anchor sync + halfspace perf are all default-on. No new feature flag. This is critical — the foundation phase must reach every user immediately, not be hidden behind opt-in flags. The pure-Rust prism path (W9) lands here too, behind a `prism-csg-fast` flag while it earns trust (see next bullet).
- **W9 + W12 promotion**: introduce `prism-csg-fast` Cargo feature, default OFF initially. Promoted to default-ON after a benchmark + correctness gate: matches the `tests/cut_openings_proptest.rs` baseline (all 4 properties, ≥1024 cases), ≥2× speedup on door/window fixtures, ≤30 % residual fallback to manifold. Unlike the original plan, this flag is **not a permanent supplement** — once default-ON, it is the *primary* prism engine and manifold is a fallback only for shapes W9 doesn't cover.
- **Phase 2 specialization (W11 + W12 reframed)**: cylinder/block/convex-brep cutters route to the pure-Rust prism path rather than manifold. Each specialized handler is gated on the proptest baseline. After Phase 2 the residual manifold call rate on the real corpus should be measurable and small (target <1 % of cuts); telemetry here is what decides whether Phase 3 is safe.
- **W15 / csgrs probe (W10-gated)**: a *benchmark*, not an adoption decision. csgrs is one candidate for the general brep × brep long tail; the thesis-port of Lalish's manifold-preservation algorithm is the other. Whichever wins the long-tail slot is evaluated against the same proptest baseline. If neither is ready when Phase 3 lands, manifold stays as the opt-in escape for that long tail — which is fine, because by then it is off the default path.
- **Phase 3 endgame target**: `default` drops `csg` (manifold + its C++/CGAL/OCCT transitive deps). Pure-Rust prism + halfspace + specialized handlers serve the default build; `manifold-csg` becomes an **opt-in feature flag**, retained indefinitely as the long-tail escape hatch (`--features csg` for genuinely-degenerate inputs). linux-aarch64 wheel-build (GH #28) unblocks because the default no longer needs a C++ toolchain. **This is a 2–3 month target, gated on real-corpus telemetry** (4–8 weeks of Phase-1+2 operation) showing the pure-Rust paths cover what real files actually do. If telemetry shows a persistent long tail that only manifold survives, Phase 3 still drops manifold from *default* but the escape flag carries more weight — the bar is "pure-Rust is the default," not "manifold is deleted."

CI matrix budget: today is 4 platforms × 1 feature config. The plan adds at most one dimension at a time, capped at 4 × 3 = 12 jobs. Per the release-cross-compile memory, the wheel matrix is already brittle. Phase 3's payoff is partly here: dropping the C++ dep from the default build *simplifies* the matrix and removes the manifold-toolchain constraint that currently blocks linux-aarch64.

Anti-goal: **no dual-engine path at runtime**. Either manifold OR csgrs serves tier-B per build, decided by Cargo features. The brep-prism and maintenance-cost critics both refute dual-engine as double-debt.

## Open questions (gating answers needed before specific items)

1. **GH #56 root cause** — does the tester's 77/84 IfcWallStandardCase fault implicate manifold's CSG kernel, or is it an upstream non-manifold-brep / winding bug that W11's pre-flight validator would catch? Answer reshapes priority between W11 (priority high) and W15 (deferred or accelerated). Currently blocked on tester response to the v0.4.34-upgrade ask filed on the issue.
2. **i_overlay polygon-with-holes API** — does `Overlay::with_subj_and_clip` accept polygons-with-holes as a single shape with NonZero fill, require EvenOdd, or require separate `add_shape` calls? Blocks W6/W9 implementation detail. ~1 hour of doc-reading + a synthetic test.
3. **i_overlay f32 vs scaled-i64 robustness** — is the f32 API safe at building-scale (10 m walls, 1 mm tolerance) after `mesh_anchor` rebase, or do we need to scale by 1000 and use i32/i64? Affects W6/W9.
4. **csgrs production-readiness on real IFC** — the entire point of W10. Pre-condition for W15.
5. **Prism-prism fast path coverage** — should W9 cover the partial-recess and pocket cases (rejected by the verdict in the slab-decomposition form), or stay narrowly through-cut and route partial cases to manifold? Decision: narrow now, evaluate seam reconstruction after the corpus measures partial-recess frequency.
6. **BakeFrame Local vs World** — does cross-product-relvoids' anchor synchronization (W7) interact with World-frame substrate output? Verify no regression in fixture (j) — UTM-rebased Norwegian wall.
7. **IfcExtrudedAreaSolidTapered as cutter** — ship handler in W8 or only typed-Unsupported it? Real-world frequency essentially zero; default to typed-Unsupported, ship handler only on customer demand.
8. **Multi-shell IfcFacetedBrep (ArchiCAD)** — does W11's per-connected-component winding fix actually let manifold accept the input, or does manifold require single-shell? Answer determines whether we ship a connected-components subtract-and-merge wrapper or surface `MultiShellNotSupported`.

## Top risks + mitigations

(Full list with 11 items in the JSON audit under `result.plan.risks_and_mitigations`; abridged here.)

1. **Chain-encoding refactor (W1) breaks subtle tag dispatch.** Mitigation: land snapshot tests of the existing chain output BEFORE the refactor (documenting OLD behaviour as a regression baseline), then update in the same PR. Full integration test suite gate before merge. Revert + Vec-sidecar fallback path if any production fixture regresses.
2. **i_overlay v7 API instability** (single-maintainer, pre-1.0). Mitigation: pin specific 7.x version, wrap behind a thin internal facade (`mesh/polygon_bool.rs`) so swap-or-fork is a one-file change. Consider vendoring if upstream goes stale (~2.5k LOC).
3. **csgrs probe (W10) reveals csgrs unfit for real corpus.** Mitigation: this is exactly why W10 is a probe, not adoption. W15 decision is binary: keep manifold-only and document the negative result. The foundation work (W1–W8, W11) ships regardless.
4. **W9 benchmark gate fails.** Mitigation: define gate explicitly before landing (≥2× speedup, ≤30% fallback rate, zero correctness regression). If gate fails, retire `prism-csg-fast` rather than promote it. Document negative result in worklog.
5. **Unit-aware tolerance (W3) changes behaviour for US-imperial users.** Mitigation: frame as bug fix; debug log per cut; emergency opt-out env var `IFCFAST_LEGACY_HALFSPACE_TOLERANCE=1` for one minor release if a regression surfaces.
6. **W14 cache-invariant fix reveals downstream consumers were relying on the wrong-but-consistent behaviour.** Mitigation: pair with W16's `_CACHE_SCHEMA_VERSION` bump and AGENTS.md note. Add an integration test asserting two instances of one mapped rep with different opening sets get different fingerprints but share the representation row.
7. **Bus-factor compounding** — i_overlay, csgrs, manifold each fragile. Mitigation: vendor-or-fork policy, each pre-1.0 dep behind a one-file facade, quarterly health audit.

## Baseline against manifold (W5a, shipped 2026-06-05)

`tests/cut_openings_proptest.rs` is a proptest-driven property harness that generates random `(host_box, cutter_box)` configurations and asserts closed-form analytic invariants on `cut_openings::apply`'s output. Four properties, 256 cases each, 1024 total configurations:

- **Volume invariant** — `volume(host − cutter) == volume(host) − volume(host ∩ cutter)` within `max(0.5%, 1e-5)` m³ tolerance.
- **Closed-manifold output** — every undirected edge in exactly two triangles post-cut.
- **Disjoint cutter** — `cutter ∩ host = ∅` → output volume unchanged.
- **Contained cutter** — `cutter ⊆ host` → output volume reduced by exactly `cutter.volume()`.

**Result on current main (manifold-csg as the cut engine):** all 4 properties pass on 256 cases each. The audit's "manifold is fragile on axis-aligned input" warning does **not** generalise to random axis-aligned prism × prism inputs at building scale — at least not at this sampling density. Pathological cases (exact-coplanar faces, sub-mm slivers, far-origin coords) are not exercised by this harness yet; those are W7 + a planned coplanar-stress harness.

Implications for the plan:

- The case for replacing manifold on prism-minus-prism rests primarily on **performance + dep-stack hygiene + cross-compile** (#28), not correctness. W9's benchmark gate should weight those signals accordingly.
- GH #56's "wall sliver" symptom is **not reproducible by random axis-aligned prism × prism cuts on current main**. Either the failing G55_RIB walls aren't in this shape class (so the fix is in extraction, not in CSG — possibly Phase 1 of GH #52 fixed it in v0.4.34), or the pathology is narrower than axis-aligned prism-prism.
- W6 (polygon-bounded halfspace) and W11 (brep cutter pre-flight) become the highest-leverage items: the failure modes there are NOT covered by this harness's prism-only generator, and prior-art tools (ifcopenshell) had specialised paths for them.

The harness extends naturally to halfspace cutters, brep cutters, far-origin coords, and rotated prisms; each extension adds a generator + a property and reuses the same assertion shape.

## What would change this decision

- If GH #56 turns out to be a non-manifold-brep upstream bug rather than a manifold-csg failure, the case for keeping manifold strengthens further; priority shifts fully to W11 (upstream validation) over W15 (kernel replacement).
- If W10 shows csgrs achieves ≥95% volume agreement vs manifold on the real corpus and ≤3× wall-clock, the calculus flips toward csgrs-as-default. Until the probe exists, this is speculation.
- If linux-aarch64 wheel-build (GH #28) becomes a hard customer requirement, a narrow `pure-rust-csg` wheel becomes mandatory; W7/W8/W11 move up.
- If ifcopenshell 0.9+ ships a stable Rust-bindings IFC geometry kernel, the entire question reframes as "wrap or write".

---

# W6 implementation blueprint (appended 2026-06-06)

> Status: **designed, not yet implemented.** W9 cross-product flush-wiring
> landed first (`8c15b94`). W6 is the last Phase-1 piece before the
> bundled W3+W4+W6+W9 release. Blueprint from a code-architect pass;
> the table-at-`apply` challenge (below) is the load-bearing decision a
> implementer must settle before writing code.

## The bug (F6) — precise location

`IfcPolygonalBoundedHalfSpace` cutters are over-cut when the boundary is
**tight** (smaller than the host cross-section). Flow today:

```
boolean.rs::polygonal_bounded_halfspace
  → thin slab in BaseSurface.Position frame, tagged halfspace_bounded:{agree|disagree}
cut_openings.rs::apply  (the IN-REP boolean path)
  → is_halfspace_cutter → true
  → derive_plane_from_slab → infinite (plane_point, normal)   ← boundary DISCARDED here
  → halfspace_clip::clip_by_plane
```

The existing test `polygonal_bounded_uses_base_surface_position_normal`
passes only because its boundary (X∈[-1000,1000], Y∈[-500,500]) **exceeds**
the wall cross-section (X∈[-500,500], Y∈[-100,100]) — discarding a
larger-than-host boundary is harmless. F6 needs a tight-boundary fixture.

The unbounded `IfcHalfSpaceSolid` plane-clip path stays untouched (GH #39
oversized-box fragility rationale intact). Only the **bounded** case with
a **tight** boundary gets the new path.

## Chosen approach: B (pure-Rust 2D reduction) fast-path, plane-clip fallback

Rejected A (build the bounded solid, subtract via Manifold) — adds a NEW
hot-path Manifold dependency against the retirement thesis, when the
dominant real case (sloped wall top, corner notch) is 2D-reducible.
Rejected C (boundary side-plane decomposition) — breaks on non-convex
boundaries. Approach B reuses the just-landed `polygon_bool::difference`
+ `SweepFrame` + re-extrude machinery, exactly like `try_prism_cut`.

Fast-path (behind `prism-csg-fast`), engaged only when: host is a single
direct Z-perpendicular extrusion AND arg[2].Z ∥ host sweep axis AND the
boundary is tight. Reduce to: project boundary polygon into the host
sweep-frame 2D basis, `difference(host_footprint, boundary)`, re-extrude
over the **plane-clipped sweep band** `[s_lo, s_hi]` (s_cut from
projecting the BaseSurface plane point onto the host axis; band side from
`plane_normal·axis` sign + agreement). Non-reducible (diverging axes,
non-prism host) → `NotParametric` → existing plane-clip fallback.

Default build: byte-identical to today (plane-clip over-cut). Feature on:
correct bounded cut, plane-clip only as fallback.

## The load-bearing implementation challenge (the architect glossed this)

Unlike `flush` — a clean POST-stream hook where a second `EntityTable`
was cheaply built and passed in — `apply` runs **inside the streaming
sink's `on_product`**, where there is no table in scope. The bounded
fast-path needs the boundary polygon + arg[2]/BaseSurface frames, which
must be re-derived from the IFC entity (the slab mesh alone doesn't carry
them). Two ways to get that data to `apply`, pick ONE up front:

1. **Table-into-sink (borrow threading).** Build the table once behind
   the feature (`prism_table_for_flush`), store `Option<&EntityTable>` on
   the sink, pass to `apply`. Cost: the sink now holds a borrow that must
   outlive the `py.allow_threads(|| mesh_ifc_streaming_framed(&mmap, &mut
   sink, …))` call, which itself borrows `&mmap`. Lifetime-fiddly but the
   table and mmap both outlive the streaming call, so it resolves; needs
   the sink struct to carry a lifetime param OR an `Arc`-wrapped table.
   Also `apply` gains a `table: Option<&EntityTable>` param → ripples to
   the proptest + integration `apply(…)` call sites (pass `None`).

2. **Payload-on-ProductMesh (carry from tessellation).** Resolve the
   `BoundedHalfspacePayload` in `boolean.rs::polygonal_bounded_halfspace`
   (table IS in scope there) and attach it to the `ProductMesh` (new
   optional field, feature-gated or always-`Option`). `apply` reads it off
   the mesh — no table param. Cost: pollutes the central `ProductMesh`
   type with a feature-specific sidecar; every ProductMesh constructor
   (tessellate_one, emit_geometryless, test fixtures) touches the field.

Recommendation: **(2)** keeps `apply`'s signature stable for the hot
in-rep path and avoids borrow-threading a table through the streaming
sink; the ProductMesh field is the same "carry params, not the kernel"
pattern `InstancePart.rep_step_id` already follows. Make it
`Option<BoundedHalfspacePayload>` (not cfg-gated) so constructors don't
need cfg blocks; it's `None` in default builds and on every non-bounded
product. Decide before writing — it sets the whole change shape.

## Tightness detector

`is_tight_boundary`: project host bbox AND boundary polygon onto the
BaseSurface plane's (e1,e2) in-plane basis (derivable from the slab
normal). If the boundary AABB strictly contains the host AABB in both
dims (≥10% margin) → not tight (cheap plane-clip). Else tight. The
existing large-boundary test stays on the cheap path by construction.

## Diagnostic counter (deferred within W6)

`UnsupportedReason::TightPolygonalBoundaryIgnored` already exists but is
unemitted. Wiring it cleanly needs a `stats`/warning channel orthogonal
to the single per-product `Outcome` (a tight boundary that still
plane-clips is `Outcome::Cut` AND a warning). To avoid an `apply`
signature ripple for `stats`, DEFER emitting this counter — the primary
W6 value is the correct bounded cut. Counter-wiring is a clean follow-up
once the outcome/warning split is designed.

## Differential test strategy (mirror the W9 prism tests)

Fixture `WALL_WITH_TIGHT_BOUNDED_HALFSPACE`: wall 1000×200×3000 (0.6 m³);
BaseSurface plane at Z=2000, +Z normal, `.T.` (subtracts Z<2000);
boundary 300×200 at origin in arg[2]=identity (tight — inside the host
footprint). Analytic: infinite-plane (current) leaves only Z∈[2000,3000]
= **0.20 m³**; bounded (new) removes only 300×200×2000 = **0.48 m³**.
The 0.28 m³ gap is unambiguous.

- non-gated: `apply(table=None or no payload)` → ~0.20 m³ (documents the
  over-cut as the default-build behaviour).
- `prism-csg-fast`: bounded path → ~0.48 m³ (analytic).
- oracle (`prism-csg-fast` + `csg`): build the exact bounded solid
  (extrude boundary, clip by plane, cap) and subtract via
  `csg::subtract_many`; assert the 2D fast-path matches within 1%.

## Build sequence

1. `boolean.rs`: resolve `BoundedHalfspacePayload {boundary: Polygon2D,
   boundary_xform: Mat4, plane_normal: Vec3, plane_point: Vec3}` in
   `polygonal_bounded_halfspace` and surface it (approach-2: attach to
   ProductMesh).
2. `prism_csg.rs`: expose `SweepFrame` as `pub(crate)` (or factor the
   projection helper) so `cut_openings` can reuse it.
3. `cut_openings.rs`: `is_tight_boundary`; bounded fast-path in `apply`
   reading the payload; plane-clip fallback unchanged.
4. Tests: the three above. Default + `prism-csg-fast` suites green under
   `-D warnings`.
5. Then bundle W3+W4+W6+W9 → single `v*` tag (cache already at 14).
