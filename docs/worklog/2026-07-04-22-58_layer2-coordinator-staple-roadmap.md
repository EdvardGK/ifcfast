## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `4ce1260` → `b4cc38b` (1 commit: Layer 2 roadmap doc)
- **Session scope**: Strategic planning tail of the #139/v0.4.42 session — a Layer 2 roadmap ("from certified core to BIM-coordinator staple") + epic issue #140.
- **Touched paths**: docs/plans/2026-07-04_coordinator-staple-roadmap.md
- **Parallel sessions observed**: none on ifcfast origin/main.
- **Supersedes / superseded by**: continuation of [[2026-07-04-03-20_gh139-arc-profiles-v0.4.42]] (same session; that entry covers the #139 fix + v0.4.42 release, this one the planning tail).

# Session: Layer 2 roadmap — from certified core to BIM-coordinator staple

## Summary
After shipping v0.4.42 (which cleared the last named QTO/mesh residues), the
user asked a 30k-foot question: what remains to make ifcfast a staple in any
BIM coordinator's workflow, and to build a plan around it. Produced a phased
Layer 2 roadmap as a repo doc + epic issue #140. The strategic thesis: the
correctness engine is now trustworthy (the hard, differentiating part is done);
the gap to "staple" is **workflow completeness + interop**, gated by the same
oracle discipline — every new surface reconciles against Solibri/Navisworks the
way QTO reconciles against ifcopenshell.

## Changes
- **`docs/plans/2026-07-04_coordinator-staple-roadmap.md` (`b4cc38b`)** — the
  Layer 2 plan, written as the explicit **successor** to the 2026-06-20
  correctness roadmap (which built Layer 1 and deliberately parked Layer 2
  behind a trust gate that is now open). Frames the work around the coordinator
  daily loop (ingest → federate → validate → clash → triage → BCF → diff →
  visualize) with a reconciliation GATE per phase.
- **Epic issue #140** — the phased plan in the tracker (canonical backlog per
  convention), folding in existing issues (#50, #67, #93, #92, #136, #16, #2,
  #59) and naming the four new pieces to spin out (clash-oracle, IDS,
  rule-surface, revision-diff).

## Technical Details
The plan's phases and keystone reasoning:
- **Phase 1 (keystone)** — trust clash at federation scale: federated substrate
  + cross-discipline `clash()` (#50), streaming (#67), and a **clash oracle**
  (new) differential vs Solibri/Navisworks on the ACC G55 benchmark. Nothing
  downstream is trustworthy until clash reconciles federated.
- **Phase 2** — BCF round-trip (#93): the interop key that puts ifcfast *inside*
  the coordinator's toolchain (Solibri/Navisworks/BIMcollab) rather than beside
  it.
- **Phase 3** — validation / IDS (buildingSMART) + rule surface.
- **Phase 4** — revision diff (on dedup #136) + issue lifecycle (#92).
- **Phase 5** — viewer workflow + dashboards + adoption unblockers (#16, #2).
- Build first: the clash oracle harness (gate before fixes, mirroring how #59
  preceded the QTO work — every clash bug then becomes a free regression test).

Deliberately filed ONE epic rather than spraying six issues, so the user can
steer the shape before the sub-issues are spun out. Offered to spin out the four
new issues (with oracle-gate acceptance criteria) or start Phase 1 on their nod.

## Next
- **User decision**: greenlight spinning out the four new sub-issues
  (clash-oracle / IDS / rule-surface / revision-diff), or start Phase 1 directly
  with the clash oracle harness.
- **Phase 1 first move** (if greenlit): extend `tests/oracle` to clash — wire the
  ACC G55 + Solibri clash export as the differential truth, gate-first.
- Confirm nothing regressed post-release (v0.4.42 is live on PyPI, verified).

## Notes
- The named QTO/mesh correctness residues are now all shipped or parked — the
  project is at a genuine phase boundary (Layer 1 → Layer 2). Future sessions
  should treat #140 as the north-star, and keep the non-negotiable: no
  coordination surface ships un-oracle-gated against the incumbent.
