# Session: ifcopenshell fork question evaluated — oracle direction filed as GH #59

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `1780ac6` (no code commits this slice — strategic conversation + GH issue + memory update only)
- **Session scope**: evaluate "fork ifcopenshell" via multi-agent workflow; file the resulting recommendation as a tracked issue
- **Touched paths**: `docs/worklog/2026-06-06-12-58_ifcopenshell-fork-evaluated-oracle-direction.md` (this file). GH #59 filed (external).
- **Parallel sessions observed**: none on `origin/main` during the session window
- **Supersedes / superseded by**: extends `2026-06-06-01-08_manifold-strategy-update-full-retirement-target.md` (same session lineage; previous slice updated the manifold-replacement direction, this slice addresses the parallel question of ifcopenshell's role)

## Summary

User asked "what about forking ifcopenshell and improving on that?" Ran a 25-agent multi-perspective workflow to evaluate four interpretations of "fork" (full fork / vendor modules / clean-room port / differential-testing oracle) across positioning, engineering-cost, user-migration, and long-term-direction lenses. Verdict converged with user's own intuition that contributing > competing: **don't fork, use ifcopenshell as a differential-testing oracle alongside our property-based test harness**. Filed as **GH #59** — companion to the manifold-replacement programme tracked in GH #58, sequenced as M1 enumerable-surfaces only at first, geometry oracle deferred to after #58 W11+.

## Changes

No code commits this slice. The strategic deliverables are:

- **GH #59 filed** — [Oracle harness for ifcopenshell differential testing (GH #58 companion)](https://github.com/EdvardGK/ifcfast/issues/59). Full rationale + sequencing + alternatives-rejected + license notes captured in the issue body so a future session can pick up cold.
- **`next-steps.md` updated** — three queueable items for next session: GH #59 M1 (oracle scaffold, 1–2 weeks), GH #58 W3 + W4 (unit-aware tolerance + operator-aware IfcBooleanResult, days), design-doc reframe (5 min). #59 and #58 don't conflict — can run in parallel PRs.

The strategic update has four concrete shifts that should propagate into the project surface (next session work):

1. **Ifcopenshell is NOT ground truth on geometry.** They ship THREE kernels (OpenCASCADE / CGAL / HybridKernel meta-fallback) precisely because no single one survives real IFC. `DISABLE_OPENING_SUBTRACTIONS` ships as an explicit give-up flag. CGAL drops original edges (their issue #6173). Treating them as oracle on geometry requires `ifcopenshell_quirk is the default classification` discipline — otherwise the oracle silently anchors ifcfast to their quirks and forecloses the case where pure-Rust is genuinely better. Properties / quantities / materials / spatial graph: ifcopenshell IS authoritative there; oracle defaults `ifcfast_bug` on disagreement.
2. **License correction**: ifcopenshell is **LGPL-3.0-or-later** (not BSD as I implied early in the conversation). Both COPYING (GPLv3) and COPYING.LESSER (LGPLv3) ship at root — some files may be GPL. Dynamic linking from non-LGPL code is permitted; static linking treated like GPL. CGAL transitive is GPLv3-or-commercial; OCCT is LGPL-2.1-with-exception. Forking + shipping requires LGPL source release.
3. **"Contributing > competing" insight holds.** ifcopenshell has 274 contributors, top three Moult (6 026 commits) + Andrej730 (5 582) + Krijnen/aothms (3 327). Project is HOT — daily Bonsai alpha releases. The C++ contribution path is constrained for us (we're Rust-first, no C++ dev), BUT our property-based test harness output IS the most-valued contribution form: every counterexample our generator surfaces against their kernel becomes a minimal reproducer we can file upstream. That builds ecosystem reputation without writing C++.
4. **Oracle composes with #58, doesn't compete.** W5a (proptest, shipped) validates pure-Rust CSG against analytic invariants + manifold's output. Oracle adds ifcopenshell as a third independent voice on the same generated cases (M4 deferred). Without oracle: "we replaced manifold with pure Rust" is self-attestation. With oracle: "we replaced manifold with pure Rust, volumes agree with ifcopenshell on the production corpus to within 1 mm³" is defensible to viewer integrators + Skiplum colleagues + prospective adopters.

## Technical Details

The workflow shape that paid off (worth re-using for similar strategic questions):

- **Phase 1 — Research (4 parallel)**: ifcopenshell facts (license / deps / kernel architecture / maintenance signals) + ifcfast current positioning (identity pillars, differentiators, bets-reversed-by-forking) + performance comparison + CSG quality landscape. Established the factual base everyone else reasons from; no opinions yet.
- **Phase 2 — Evaluate (pipeline 4 interpretations)**: A (full fork) / B (vendor modules) / C (clean-room port) / D (oracle). Each gets a design with scope / first-six-months / permanent-costs / value-unlocked / user-migration / blockers.
- **Phase 3 — Verify (4 lenses × 4 designs = 16 verdicts)**: positioning / engineering-cost / user-migration / long-term-direction. Each lens evaluated hard-nosed; counted strong-yes / yes / mixed / no / strong-no.
- **Phase 4 — Synthesize**: ranking + headline + concrete first action + decision-change triggers + integration with the manifold-replacement programme.

Total: 25 agents, ~11 min wall-clock, 1.3M tokens. The earlier manifold-replacement audit was 45 agents / 22 min / 2.5M tokens — proportional to the question's scope. For "should we do X strategic thing?" with ~4 interpretations × ~4 lenses, 20–25 agents is the right size.

Why I trust the verdict: D and the manifold-replacement programme reinforce each other rather than competing. The proptest harness shipped 2026-06-05 (commit `3092ae4`) was correctness-by-construction against analytic invariants. The oracle harness adds ifcopenshell as a third independent voice that doesn't share ifcfast's f64 epsilon or manifold's coplanar-face quirks. That is exactly the validation infrastructure the manifold-retirement decision needs to be defensible to skeptics — without it, the replacement is plausible but unverifiable to outsiders.

## Next

1. **GH #58 W3 + W4 bundle PR** OR **GH #59 M1 oracle scaffold** — pick one, both shippable independently. W3 + W4 unblocks the manifold-replacement Phase 1; #59 M1 unblocks the oracle harness for downstream geometry-QTO comparison.
2. **Design-doc reframe** — `docs/plans/2026-06-05_cut-openings-manifold-replacement.md` still reflects the audit-conservative framing from the previous worklog. 5-min update: change top-level Decision paragraph to "full retirement target in 2–3 months"; add a "Companion: oracle harness (GH #59)" pointer.
3. **GH #56 / #57 tester response** still unanswered (blocked external). Their answer reshapes priority between W11 (brep pre-flight, accelerated if it's an input bug) and W15 (csgrs probe).

## Notes

- The workflow output identified one concrete operational risk worth carrying forward: on geometry-QTO disagreements between ifcfast and ifcopenshell, **classify `ifcopenshell_quirk` as the default** until proven otherwise. Their hybrid-kernel-with-fallback IS an admission that no single kernel survives real IFC. Without this discipline the oracle anchors ifcfast to their quirks.
- The "contributing > competing" insight (user's framing) is the strongest single argument for D over the other interpretations. The IFC community has ~274 active contributors total to ifcopenshell; the gravitational centre is small. Being a contributor compounds reputation; being a competitor splits the community and makes every ifcfast user explain to their team why they're not using the standard.
- The escalation lever: if GH #58 W11–W15 stalls for >8 weeks on a specific primitive (IfcAdvancedBrep tessellation is the most likely culprit), promote C's **policy-layer ports** (`fuzzy_tolerance.rs` + `halfspace_policy.rs` + `hybrid_dispatch.rs`, ~1.5 kLOC, codifies 10+ years of their kernel-triage wisdom under MIT). Do NOT port their algorithm-heavy stuff (BSpline evaluator, advanced-brep trimming) — that's the engineering-cost trap that makes C strong-no overall.
- Workflow output JSON cached at `/tmp/claude-1000/-home-edkjo-workspace-inbox-ifcfast/8abd9c25-d33a-47f0-90a2-f3faa62b4af4/tasks/wzibwwiqx.output`. Will be cleaned up next reboot; if the detail matters for a future session, re-run the workflow (script is at `…/workflows/scripts/ifcopenshell-fork-evaluation-wf_71f234e6-50b.js`) — identical inputs would produce nearly-identical results.
