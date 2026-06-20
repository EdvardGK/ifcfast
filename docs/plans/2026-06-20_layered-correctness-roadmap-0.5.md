# ifcfast roadmap — the correctness leap (→ 0.5.0)

**Date:** 2026-06-20 · **Author:** `claude:omarchy` with edkjo (product) ·
**Basis:** product review #86, QTO-correctness cluster (#114/#56/#62), oracle #59.

## The bet

Competitors (ifc-lite et al.) race for **100% feature/schema coverage** — a
funded-team game a one-person+agent project loses. ifcfast wins a different,
narrower, winnable claim:

> **100% trustworthy numbers** — correct where it says it's correct, and *loud*
> (flag + escalate to the ifcopenshell kernel) where it isn't.

"Fast" decays — everyone tokenizes at GB/s now. "Correct, and honest about it"
compounds. The leap is to stop selling speed and ship **0.5.0 = the IFC tool
whose QTO and mesh you don't re-check**, certified by a differential oracle.

Key reconciliation: **100% correct ≠ 100% capable.** We don't have to win the
full CSG/kernel war to be trustworthy. The hybrid-QTO pattern (already shipped)
is the pressure-relief valve: certify where we match the kernel, flag/escalate
the ~0.3% where we don't. The oracle measures both halves.

## The layering

Everything spatial and clever is **Layer 2** and sits *on top of* a certified
**Layer 1**. Layer 2 is important and not abandoned — it is *sequenced*, because
a clash on the wrong floor or a reroute through a mis-measured void is worthless
if the mesh underneath isn't trusted. Parking Layer 2 now is what lets it stand
later.

### Layer 0 — Substrate (have; keep; don't touch)
file → typed DataFrames / parquet / DuckDB; the self-describing trio
(`summary`/`schemas`/`preview`); long-format tables; honesty disclaimers
(`system_prompt()`). The review's verdict: this is right. Keep the shape.

### Layer 1 — The certified core ("100% QTO + mesh"; THIS is 0.5.0)
The must. Definition of done for every item = **oracle-clean over the corpus**.

1. **Oracle harness (#59)** — differential test vs ifcopenshell geom kernel + QTO
   over the whole client corpus, in CI. This is the moat and the literal
   meaning of "100%". Build it FIRST; every existing bug fixture becomes a
   regression test for free. Truth source = ifcopenshell (automatable), per
   product decision 2026-06-20.
2. **Far-origin precision sweep** — the f32 signed-tetrahedra accumulator suffers
   catastrophic cancellation on far-origin geometry (G55 lives 100 m+ out).
   Root-caused in `crates/core/src/mesh/qto.rs:191/260` and
   `crates/core/src/mesh/stats.rs:99/118`: **rebase to a local origin + accumulate
   in f64**. trimesh (f64) matches Solibri; ifcfast (f32) reads 9–18% low → #114.
   Audit *every* world-coord reduction, not just QTO.
3. **QTO correctness cluster** — #114 (precision), #56 (~0-vol Revit walls),
   #62 (prism bound), shell-closing so `open_shell` elements yield real mesh
   volume instead of falling back. Keep the confidence flags; keep
   escalate-to-kernel as the honest escape.
4. **Mesh extraction incl. cut-openings** — openings are *base logic*: a wall
   with a door hole is the truth. Pull the cut-openings CSG (#21/#64, W-programme
   #58) into the core, **reframed strictly as "match the kernel," not "expand
   capability."** Oracle decides how far it must go. manifold-csg stays as the
   opt-in escape hatch / escalation target, never a default crutch.
5. **Loud failure (`strict=True` default)** — #70/#71/#72/#73 (truncation,
   framing comments, unknown table/entity, imperial-as-metres). A silent wrong
   number is the *opposite* of 100%; this is inseparable from the core.

**Gate:** cut 0.5.0 when Layer 1 passes the oracle clean. Say why in the
release notes — version numbers are documentation.

### Layer 1.5 — Own the lane (near-term, after the gate)
6. **Finish the MCP data surface** — `psets / quantities / materials /
   product_card(guid) / sql(query)`. Today the flagship agent integration
   exposes the skeleton and keeps the data in Python (#86 §1.2).
7. **geometric_storey + location_reliable (#94)** — "mesh is truth." The
   differentiator and load-bearing under any storey-bounded QTO. Depends on the
   certified mesh from Layer 1.
8. **Minimal pset write-back** — X-ray → surgical tool; the #1 agent unlock
   (#86 want-list 4). Byte-offset preservation already in design notes.

### Layer 2 — Spatial intelligence (parked this session; the real roadmap)
Important. Not abandoned. Each consumes certified Layer-1 mesh + geometric_storey.
Sequence:
- **MEP system topology (#91)** — ports + geometric fallback; prerequisite for
  both reroute and clash triage.
- **Clash triage / grouping (#92, #93) + federated multi-model (#50)** — group on
  `geometric_storey`, not the relationship storey, or it reports on the wrong
  floor.
- **Free-space voxel + EDT (#115)** — rooms / portals / clearance from the void
  complement; generalizes the reroute C-space.
- **Constraint-aware MEP rerouting (#63)** — primitives partly shipped on
  `feat/reroute-primitives`; a *product on top of* the parser.

**Why Layer 2 waits:** it is only as trustworthy as the mesh and storey
assignment beneath it. Certify Layer 1, then these become defensible features
instead of plausible-looking demos.

## What we stop (to afford the above)
- **Data-path parquet cache** — measured wash, caused two integrity bugs
  (#68/#80). Scope it to geometry layers only.
- **App-shaped code in the library** (`federated_floors.py`, triage trajectory)
  → `examples/` or downstream (sprucelab, #2).
- **Capability-expansion in the kernel** — W-programme depth beyond
  oracle-required cut-openings, exact-union plans. Correctness, not capability.

## Sequenced worklist (Layer 1, in order)
1. #59 oracle scaffold (corpus runner + ifcopenshell adapter + CI hook).
2. #114 far-origin f32→f64+rebase fix in `qto.rs` + `stats.rs` + regression test.
3. Oracle sweep → triage QTO divergences (#56, #62, shell-closing) by magnitude.
4. Cut-openings oracle parity pass (#21/#64) — match-the-kernel scope.
5. `strict=True` loud-failure pass (#70–#73).
6. Tag **v0.5.0** on oracle-clean; release notes explain the correctness pivot.

## Keep AGENTS.md in lockstep
Any agent-facing surface change (MCP tools 6, write-back 8, strict default 5,
new QTO columns/flags) updates `AGENTS.md` in the same change — public contract.
