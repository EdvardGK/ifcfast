# Session: Close-out backlog sweep — core waves 4–5 + ifcfast-site frontend + OOM recovery

## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast` (parser) + `/home/edkjo/workspace/inbox/ifcfast-site` (Next.js demo/marketing site); work via ephemeral agent worktrees
- **Branch**: ifcfast `main` `806491f` (→ `dc6aa03`/#113 pending CI); ifcfast-site `master` → `716d1d5` (PRs #1/#2/#3 merged)
- **Session scope**: "close out [=solve] all open issues and PRs" — fan out fix-agents across the remaining tractable parser backlog AND the 9 frontend issues (which live in ifcfast-site, a separate repo)
- **Touched paths**: ifcfast: `crates/core/src/{lexer,indexer,source,lib,bundle/mod,extractors/*,mesh/boolean}.rs`, `python/ifcfast/{model,cache,header}.py`, `AGENTS.md`, `CHANGELOG.md`, tests; ifcfast-site: `app/{page,layout,opengraph-image}.tsx`, `components/{findings-view,qto-panel,dash-tile,layer-sets,copy-button,mcp-install,terminal}.{ts,tsx}`, `app/dev/workbench/findings.ts`
- **Parallel sessions observed**: none (sole writer)
- **Supersedes / superseded by**: continues the same megasession as `2026-06-14-11-38` (v0.4.38 release) — third checkpoint

## Summary
Continuation of the v0.4.38 session. User directive: "close out all open issues and PRs" — clarified mid-session to mean **solve**, not close-without-fix. Triaged 30 open issues into tractable-core / already-shipped / frontend-in-another-repo / multi-session-epics / meta. Ran **wave 4** (#71/#76/#87/#89) and **wave 5** (#27/#52/#88) of parser fix-agents, and a **3-agent frontend wave** against the **ifcfast-site** repo for the 8 findings/QTO/landing issues. Net this phase: **~14 more issues solved** (running session total ≈24). Hit a hard **OOM wedge** (shell couldn't fork) from overlapping `maturin develop --release` + `next build` + an agent's 666 MB `node_modules` copy into `/tmp` (tmpfs); recovered via user-run `! rm -rf` of the tmpfs copy.

## Changes
**Parser (ifcfast) merged:** #21 (verified cross-product cut already shipped, closed), #71 (Python minor batch), #76 (Rust lexer/extractor escape+set-value batch, cache→20), #87 (header decode + no-drift cache gate), #89 (truncation guard into Rust `source::open`), #27 (catch_panic on all 8 unwrapped pyfunctions), #52 (regression test only — P1 already shipped, P2 left open), #112 (FILED: python-without-mesh build breaks). #113 (#88 storey-via-aggregate, cache→21) pushed, CI pending.
**Frontend (ifcfast-site) merged** — PRs #1/#2/#3 closing ifcfast #7/#8/#9/#10/#11/#12/#13/#14:
- #1 findings-view: de-stack same-GUID findings (severity strip counts distinct conditions), suppress no-storey on spatial orphans (most of #11/#13/#14 were already shipped).
- #2 QTO: partition aggregate-rollup geometry once (Roof no longer double-counts Slab; 1725→1637.5 m³), scope-aware KPI tiles, layer-sets from bundle.json.
- #3 landing: copy honesty + UX/a11y/Next-16 metadata (OG image) polish.

## Technical Details
- **Design call made (own-the-product):** #88 — `storey_guid` now inherits transitively through `IfcRelAggregates` so `filter(storey_guid=)` == `products_in()`; both the Python `_index_native` and Rust `bundle::resolve_storey_sid` paths fixed in lockstep, direct-containment precedence + cycle guard. On G55_ARK: 1,754 aggregate parts gained a storey. Also corrected the stale `test_storey_products_cardinality` test that encoded the old disagreement.
- **3.13 dtype brittleness (#108):** CI failed only on py3.13 because newer pandas prints empty string columns as `'str'` not `'object'`; fixed by normalising string-storage dtype variants → `'object'` in `_df_schema` (version-stable contract).
- **Frontend cross-repo:** issues filed on EdvardGK/ifcfast, code in EdvardGK/ifcfast-site → PRs target ifcfast-site, ifcfast issues closed manually (no reliable cross-repo auto-close). Agents set up their own ifcfast-site worktree; build-verified from the main checkout (Turbopack rejects symlinked node_modules out of the worktree root).
- **OOM incident:** overlapping heavy builds on the 16 GB box exhausted RAM+swap → `fork()` failures → every Bash call exit-1-no-output while Read still worked. Cause: an agent `cp -rL`'d 666 MB node_modules into `/tmp` (tmpfs=RAM). Recovery required the user to `! rm -rf /tmp/fe-qto/node_modules` (I couldn't run a shell). All work was safe on branches.

## Next
- **Merge #113** (#88) once CI green (`gh pr merge 113 --squash`; clean up worktree `agent-ac21f32d20afc19ec` + branch) — then close #88. Rust 226 tests pass locally; awaiting Python matrix.
- **PR #90** (ToghrolTP, external) — still needs user OK to post the drafted credited close (the permission classifier correctly blocks me acting on an external PR).
- **Remaining tractable core (a wave 6):** #47 (`m.mesh(guid=)`), #56 (mesh_qto ~0 vol on Revit walls — likely deep), #62 (prism bound), #112 (the build-config bug filed this session).
- **#16** (drop-your-own-IFC) — a real client-side-parsing feature for ifcfast-site, not a quick fix.
- **Epics (hand-off):** #63, #91, #92/#93/#94, #67, #23, #50, #58, #59, #64. **Meta:** #2, #51 (partly resolved by pyo3 bump), #86, #52-P2.

## Notes
- **Operational lesson (critical):** never let agents copy node_modules into `/tmp` (tmpfs) and never overlap `maturin --release` + `next build` + Rust agent builds on this 16 GB box. Stage heavy builds; frontend agents must put worktrees on the same filesystem root as node_modules (btrfs `/home`, not tmpfs `/tmp`).
- ifcfast-site is the demo/marketing site (`ifcfast.com`): Next.js 16 + React 19 + d3; its AGENTS.md warns "Next.js 16 APIs differ — read node_modules/next/dist/docs first."
- `feat/reroute-primitives` (GH #63) still untouched; local checkout remains on it.
