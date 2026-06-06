# Session: Manifold replacement strategy update — full retirement target

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `fdb4c72` (no commits this slice — strategic conversation only)
- **Session scope**: revise the manifold-replacement direction from "narrow specialized + manifold as long-tail safety net" to "full IFC-specialized CSG replacement targeting manifold retirement from defaults in 2–3 months"
- **Touched paths**: `docs/worklog/2026-06-06-01-08_manifold-strategy-update-full-retirement-target.md` (this file)
- **Parallel sessions observed**: none on `origin/main` during the session window
- **Supersedes / superseded by**: extends `2026-06-05-14-44_w1-w2-w5a-and-manifold-strategy.md` (same session lineage; this slice updates the direction recorded there)

## Summary

Pure conversational slice — no code changes, no commits. User challenged the audit-conservative "manifold-stays-for-the-long-tail" framing on the grounds that AI-assisted development genuinely compresses what "5 years of single-dev Manifold" means. The challenge held up under scrutiny: most of those 5 years is engineering execution + bug-reports that the proptest harness (shipped earlier in the day, commit `3092ae4`) substitutes for. Direction revised to: target full retirement of `manifold-csg` from default builds within 2–3 months of focused work, with manifold staying available behind an opt-in feature flag indefinitely as the long-tail escape hatch.

## Changes

No file edits this slice. The design doc `docs/plans/2026-06-05_cut-openings-manifold-replacement.md` still reflects the conservative framing; updating it is the first action item for the next session.

The strategic update has three concrete shifts:

1. **W9 (pure-Rust prism-minus-prism) moves up.** Was sequenced after W3 + W6 in the original plan for a 30 % LOC saving; now scheduled to land in the same coding session as W3 + W4 (foundation work in parallel rather than as prerequisites).
2. **W11 + W12 are reframed.** Original plan: "validate then fall back to manifold." New framing: classify-then-dispatch where we *replace* manifold for the cases we can specialize. Cylinder cutters become 24-gon prism approximations; block cutters become rectangle prisms; both route through W9's path. Brep cutters get the connected-component winding fix + dispatch to W9 if convex, otherwise to a specialized long-tail handler.
3. **Phase 3 (full brep × brep replacement) becomes a real target rather than a deferred-indefinitely item.** Approach: port Lalish's manifold-invariant algorithm from the open-source C++ source, validated faithfully by reading the thesis (the irreducibly hard intellectual content is the manifold-preservation proof, which Claude can read and re-implement carefully). Property-based testing catches regressions; manifold stays available as opt-in escape until the corpus confirms parity.

Three honest risks the user accepted (not as veto, as known costs):

- Numerical-robustness debugging is the long pole. Even with property-based testing at 10k cases per property, real-world degeneracies hit production that the generator doesn't cover. We own that bug surface permanently. Mitigation: keep manifold as `csg` feature, default-off after Phase 1 lands but available for users who hit a degenerate input.
- Lalish's manifold-invariant work needs faithful porting, not paraphrasing. Skipping symbolic-perturbation predicates is the textbook way to ship subtle bugs. Reading the thesis is not optional.
- Opportunity cost. 2–3 months on CSG means 2–3 months not on viewer feedback / substrate / MEP geometry. User has the runway and accepts the trade.

## Technical Details

The key argument the user made, restated for the record:

> "5 years of single-dev development" decomposes into roughly:
> - ~1 year figuring out which algorithm to use (Claude reads the thesis; substitute for this).
> - ~1 year finding numerical-robustness pitfalls (the proptest harness substitutes for this when its generator covers the relevant parameter space).
> - ~3 years fixing reported bugs, supporting users, optimizing perf, language bindings (engineering execution; AI-assisted dev compresses this dramatically).
>
> The irreducible part — the manifold-invariant proof — is documented in Lalish's thesis. Re-implementing it carefully under property-based validation is plausible in 1–3 months of focused work for a *specialized* IFC CSG engine (narrower scope than general 3D mesh CSG).

This collapses the "we'd be wholesale-replacing 5 years of work" objection that had been carrying weight in the audit-conservative framing. It does *not* collapse the harder objections — numerical robustness debugging is intrinsically hard, the thesis content matters, opportunity cost is real — but it reshapes them from veto-class risks to managed-cost risks.

The audit (45-agent multi-perspective workflow earlier today) is still the right map of the algorithmic terrain. What changed is the timeline + the destination: not "specialize where the math is simple, keep manifold for the rest," but "specialize systematically across all the IFC vocabulary, retire manifold from defaults, keep it opt-in as a fallback."

## Next

1. **Update `docs/plans/2026-06-05_cut-openings-manifold-replacement.md`** to reflect the revised direction. Specifically: (a) reframe the "Decision" paragraph at the top from "keep manifold as production engine" to "full retirement target with 2–3 month timeline," (b) update Phase tables — Phase 1 covers W3+W4+W6+W9 in 2–3 weeks; Phase 2 covers W11+W12+long-tail brep handler in 3–6 weeks; Phase 3 retires manifold from defaults, (c) document the user's AI-dev-velocity argument as the framing-update rationale.
2. **GH #58 meta-issue comment** noting the direction update — keeps the canonical tracker in sync with the design doc.
3. **W3 + W4 + W9 as the next coding bundle.** No sequencing prerequisite between W3 and W9 — bundle them. About a week of focused work to land all three behind feature flags, default-off until the benchmark gate clears.
4. **Read Lalish's thesis before writing Phase 3 code.** That's the long-pole prep work.

## Notes

- The proptest harness (W5a, commit `3092ae4`) is now the load-bearing piece of validation infrastructure for this entire programme. Every PR in Phases 1–3 runs against the same 1024-case baseline (extending to 10k for stress) + the analytic invariants. A pure-Rust path that doesn't match the proptest budget doesn't ship. The harness IS the "correctness budget" — keep it sharp.
- Tester response on GH #56 / #57 still unanswered. If the answer comes back as "GH #52 Phase 1 in v0.4.34 already fixed it" (probable per the proptest baseline finding that prism-prism works correctly on current main), the manifold-vindication-on-prism-prism story strengthens further and Phase 1 priority shifts toward W6 (polygon-bounded halfspace) over W9 (which we've already shown isn't broken).
- The conversational slice today re-anchored on a useful tension: audit-driven plans are conservative-by-design (the critic phase intentionally surfaces refutations and adds friction). User pushback that compresses scope is healthy correction. The proptest baseline gives us the empirical grip we need to take that correction safely — we can verify rather than argue.
