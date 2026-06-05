# Cut-openings + manifold-csg replacement plan

Authored 2026-06-05. Companion artefact: `docs/2026-06-05_manifold-replacement-audit.json` (raw multi-agent audit output, 45 agents, ~2.5M tokens, 22 minutes wall-clock).

## TL;DR

We are **not** doing a wholesale `manifold-csg` → pure-Rust rewrite this cycle.

The audit converged on a different shape: a multi-release programme of **foundation refactors + narrow, well-validated, default-on correctness fixes**, with manifold staying as the production engine for the irreducible 3D-CSG long tail. The audit refuted the most ambitious replacement designs across multiple adversarial lenses (20 of 32 verdicts refuted, including all four lenses on `brep-minus-prism` and `general-fallback`).

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
| W5 | Fixture corpus for cut_openings regression | W2 | low | days | CI gate |
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

Target progression:

- **W1–W8 + W11 + W13 + W14 releases**: `default = [..., "csg"]` unchanged. Foundation + correctness + typed-Unsupported + anchor sync + halfspace perf are all default-on. No new feature flag. This is critical — the foundation phase must reach every user immediately, not be hidden behind opt-in flags.
- **W9 + W12 release**: introduce `prism-csg-fast` Cargo feature, default OFF. Mutually compatible with `csg`. Promoted to default-on only after a benchmark gate: ≥2× speedup on door/window fixtures, ≤30% fallback rate to manifold, zero correctness regression. If the gate fails, retire the flag rather than ship slow.
- **W15 release**: conditional on W10 probe. If csgrs passes (≥95% volume agreement vs manifold, 0 panics, ≤3× wall-clock on real corpus): introduce `pure-rust-csg` feature, default OFF, **mutually exclusive** with `csg`. Gives linux-aarch64 (GH #28) and pure-Rust-only users a path. Manifold remains primary default. If csgrs fails: no new feature flag; manifold stays sole tier-B engine and the W15 plan is filed as "evaluated, deferred".
- **Long-term target**: `default = [..., "csg", "prism-csg-fast"]`. `csg` (manifold) remains optional but on-by-default for at least 2 minor releases past the `prism-csg-fast` promotion. `pure-rust-csg` stays opt-in. **Manifold is not retired in this cycle.** Its eventual removal is a separate decision gated on a future audit showing pure-Rust coverage is empirically complete.

CI matrix budget: today is 4 platforms × 1 feature config. The plan adds at most one dimension at a time, capped at 4 × 3 = 12 jobs. Per the release-cross-compile memory, the wheel matrix is already brittle.

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

## What would change this decision

- If GH #56 turns out to be a non-manifold-brep upstream bug rather than a manifold-csg failure, the case for keeping manifold strengthens further; priority shifts fully to W11 (upstream validation) over W15 (kernel replacement).
- If W10 shows csgrs achieves ≥95% volume agreement vs manifold on the real corpus and ≤3× wall-clock, the calculus flips toward csgrs-as-default. Until the probe exists, this is speculation.
- If linux-aarch64 wheel-build (GH #28) becomes a hard customer requirement, a narrow `pure-rust-csg` wheel becomes mandatory; W7/W8/W11 move up.
- If ifcopenshell 0.9+ ships a stable Rust-bindings IFC geometry kernel, the entire question reframes as "wrap or write".
