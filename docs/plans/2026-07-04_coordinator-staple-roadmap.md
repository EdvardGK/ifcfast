# ifcfast roadmap — from certified core to coordinator staple (Layer 2)

**Date:** 2026-07-04 · **Author:** `claude:omarchy` with edkjo (product) ·
**Basis:** the correctness leap ([[2026-06-20_layered-correctness-roadmap-0.5]]),
product review #86, clash/coordination cluster (#50/#92/#93/#63/#94/#115).
**Supersedes:** nothing — it is the **successor** to the correctness roadmap.
That plan built Layer 1; this one activates Layer 2 on top of it.

## The inflection

The 2026-06-20 roadmap made one bet: **win on trustworthy numbers, not feature
breadth** — certify QTO + mesh against an ifcopenshell oracle, flag/escalate the
rest. As of **v0.4.42 (2026-07-04)** that bet is substantially paid off: the
named QTO/mesh residues (#114, #62, #121, #123, #138, #139, far-origin, winding)
are shipped or explicitly parked, each oracle-gated over the G55 corpus. The
write axis (subset / hotswap / mutate) is complete.

**The engine is now trustworthy. That was the hard, differentiating part.**

The correctness roadmap said Layer 2 (everything spatial and coordination) "is
important and not abandoned — it is *sequenced*, because a clash on the wrong
floor or a reroute through a mis-measured void is worthless if the mesh
underneath isn't trusted." That sequencing gate is now open. **This plan is
Layer 2.**

## The thesis (unchanged, extended)

Certified correctness is the moat. Don't compete with Solibri's UI; be the
**certified, scriptable / agent-drivable layer** a BIM coordinator (and their
automations) lean on for the mechanical 90%, interoperating through **BCF / IFC
/ IDS** with the tools they already run.

The one rule that turns "a fast engine" into "a staple": **extend the exact
oracle-gate discipline that made QTO trustworthy onto every new surface.** Clash
counts get gated against Solibri/Navisworks the way QTO is gated against
ifcopenshell. A coordinator switches a habit for trust they can't get elsewhere
— "ifcfast's clash set reconciles element-for-element with Solibri, in seconds,
scriptable" — not for another feature.

## The product frame: the coordinator's daily loop

```
ingest → federate → validate → clash → triage → communicate(BCF) → track(diff) → visualize
```

Every phase below is one stage of that loop, and each has a **definition of done
= reconciles against the incumbent tool over the ACC G55 + Solibri ITO benchmark**
([[acc-g55-ito-benchmark]]), the same way Layer 1's DoD was "oracle-clean over the
corpus."

## Phases (sequenced; each with a GATE)

### Phase 0 — Certified core — **DONE**
Oracle-gated QTO + mesh, write axis, clash engine, cut-openings, surface styles,
substrate. (Layer 1.) Keep the oracle gate as the ship discipline for anything
that moves geometry/QTO.

### Phase 1 — Trust the clash numbers at federation scale · **the keystone**
A coordinator always works federated (ARK/RIB/RIV/RIE at once), and won't trust a
clash count they can't reconcile with Navisworks/Solibri.
- **Federated substrate + multi-model clash** (#50) — one queryable model-of-models
  with discipline provenance; `clash()` across disciplines without hand-merging.
- **Streaming / region-bounded extraction** (#67) — federated sets are huge;
  `meshes()` is eager and OOMs.
- **Clash oracle** *(new)* — extend `tests/oracle` to clash: differential vs
  Solibri/Navisworks clash results on the ACC G55 federated benchmark. This is the
  #59 discipline applied to clash.
- **GATE:** clash count + pairs reconcile element-level vs Solibri on the G55
  federated set (within a stated tolerance, flagged where not); a full federated
  set extracts under the memory ceiling.

### Phase 2 — Get inside the toolchain · **BCF round-trip**
BCF is how coordinators talk to Solibri / Navisworks / BIMcollab. Without it,
ifcfast is *beside* the workflow, not *in* it. Highest interop leverage.
- **BCF export** (viewpoints, camera, components, snippets) + **BCF import** —
  builds on the #93 export feedstock (overlap centroid, grids, `locate()`).
- **GATE:** a clash issue set exports to BCF 2.1/3.0, opens correctly in
  BIMcollab/Solibri, and re-imports lossless (round-trip conformance vs
  incumbent-authored BCF).

### Phase 3 — The daily acceptance gate · **validation / IDS**
Coordinators QA every incoming model before they trust it (Solibri rulesets).
- **IDS checking** *(new)* — buildingSMART Information Delivery Specification, the
  emerging BEP-compliance standard.
- **Rule surface** *(new)* — naming, classification coverage (NS 3451 already
  parsed), property/pset completeness. The substrate makes this fast; the surface
  doesn't exist yet.
- **GATE:** buildingSMART IDS conformance suite green; rule results differential
  vs an equivalent Solibri ruleset on G55.

### Phase 4 — Make it a habit · **revision diff + issue lifecycle**
Every coordination cycle is a *diff*; recurring value is what makes a tool a
habit, not a one-off.
- **Element-level revision diff** *(new)* — added / removed / moved / re-QTO'd /
  re-clashed between model versions, built on the dedup primitive (#136).
- **Issue lifecycle across runs** — two-layer triage (#92) hardened into a stable
  issue model (new / active / resolved / reappeared) so BCF issues survive
  re-runs.
- **GATE:** correct diff on known before/after model pairs; issue-set stability
  run-over-run on G55_RIV (measured 1832 clashes → ~306 issues).

### Phase 5 — The human surface · **viewer workflow + reporting**
For the coordinators who don't script. Trails the engine work; the ifcfast-site
viewer ([[ifcfast-site-frontend-repo]]) is the vehicle.
- **Viewer**: clash/issue navigation, sectioning, measure, BCF sync.
- **Dashboards**: per-discipline scorecards, per-storey heatmap density (#93),
  QTO/ITO reports.
- **Adoption unblockers**: "drop your own IFC" demo (#16); sprucelab as canonical
  production user (#2).

## Cross-cutting (applies to every phase)

- **Trust extension** — oracle-gate each new surface (clash, IDS, BCF) against the
  incumbent, in CI. Non-negotiable; it's the moat.
- **Surface parity** — every agent-visible capability lands on Python + CLI + MCP
  together, `AGENTS.md` updated in lockstep (project contract).
- **Benchmark asset** — ACC G55 + Solibri ITO is the differential source of truth
  ([[acc-g55-ito-benchmark]]); guard it, keep it current.

## Dependencies / critical path

- **Phase 1 is the keystone** — nothing downstream is trustworthy until the clash
  numbers reconcile at federation scale. Do it first.
- **Phase 2 (BCF)** can parallel late Phase 1 once the clash issue model is stable.
- **Phases 3 & 4** depend on the federated substrate from Phase 1.
- **Phase 5** trails; needs a stable issue model (Phase 4) to be worth the viewer
  investment.

## What to build first (next 2–3 sessions)

1. **Clash oracle harness** — extend `tests/oracle` to clash; wire the ACC G55 +
   Solibri clash export as the differential truth. (Mirrors how #59 was built
   before the QTO fixes — the gate comes first, then every clash bug becomes a
   free regression test.)
2. **Federated substrate + cross-discipline `clash()`** (#50).
3. **BCF export MVP** (#93) — even before import, exporting a coordinator-grade
   issue set to BCF is the fastest path to "inside the toolchain."

## Non-goals (explicit, to protect the sequencing)

- Not building a Solibri/Navisworks UI replacement — be the certified layer under
  and beside them.
- Not chasing 100% schema/geometry coverage (the funded-team game we don't play).
- Not shipping any coordination surface (clash/validation/BCF) that isn't
  oracle-gated against the incumbent — an un-reconciled number is worse than no
  number.
