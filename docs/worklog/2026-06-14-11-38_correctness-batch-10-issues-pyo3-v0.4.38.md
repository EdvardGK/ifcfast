# Session: Correctness + security batch — 10 issues fixed, pyo3 0.29, v0.4.38 cut

## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast` (local checkout on `feat/reroute-primitives`; all work landed on `main` via PRs + ephemeral agent worktrees under `.claude/worktrees/`)
- **Branch**: `main` @ `8af0820` (v0.4.37) → `5454a91` (v0.4.38) — 12 commits this session (11 squash-merged PRs + 1 release commit)
- **Session scope**: Coordinate parallel fix-agents to clear the correctness backlog, independently review+verify each PR, then cut release v0.4.38
- **Touched paths**: `crates/core/src/{lexer.rs,indexer.rs,extractors/{psets,quantities,classifications}.rs,mesh/mod.rs,bundle/parquet_sink.rs,lib.rs}`, `python/ifcfast/{model.py,classify.py,cache.py,header.py,mcp_server.py}`, `AGENTS.md`, `CHANGELOG.md`, `Cargo.toml`, `pyproject.toml`, `Cargo.lock`, `tests/*`
- **Parallel sessions observed**: none (sole writer to origin/main this window; external PR #90 from ToghrolTP remained open/untouched)
- **Supersedes / superseded by**: none

## Summary
Started from a request to check GH activity and independently review the open PR #95. That expanded into a coordinator session: I reviewed+merged #95, then ran **three waves of parallel fix-agents** (in isolated git worktrees) against the correctness backlog, independently reviewing and verifying every PR before merge, and finished by cutting release **v0.4.38**. Net: **10 issues closed + a pyo3 security bump**, all on `main`, cargo-audit clean, cache schema v16→v19. The discipline throughout was *verify, don't trust the self-report* — real-model checks, local build-tests on merges where two branches edited the same Rust file, and a trace of the cache mtime plumbing.

## Changes
**Merged PRs (11):**
- #95 (#66) — strip synthetic half-space cutter slabs from no-cut surfaces (verified on real G55_ARK: 54 m → true extents, AABB == mesh_qto.aabb_volume_m3)
- #97 (#81) — `by_type` subtype expansion via static supertype map (counts == ifcopenshell)
- #98 (#73) — imperial `IfcConversionBasedUnit` → unit_scale (FOOT 0.3048 / INCH 0.0254)
- #96 (#77) — UTF-8-first string decode (per-byte Latin-1 fallback), escapes preserved
- #99 (#74) — IFC4 IfcDoor/IfcWindow predefined_type positional fix, USERDEFINED preserved
- #100 (#82) — IFC4X3 built elements classify (IfcBuiltElement rename + schema-suffix resolve)
- #101 (#80) — parquet cache: source-freshness validation (mtime/size) + atomic writes
- #102 — pyo3 0.24→0.29 (RUSTSEC-2026-0176/0177); `allow_threads`→`detach` ×21
- #103 (#75) — classification chain walk to terminal IfcClassification (cycle-guarded)
- #104 (#72) — STEP framing comment/string-aware (memchr3 fast path kept)
- #105 (#69) — capture bare IfcTypeProduct/IfcTypeObject + type_guid + type psets/quantities
- Release commit `5454a91`: version 0.4.37→0.4.38 (Cargo.toml/pyproject/Cargo.lock), CHANGELOG finalized, tag `v0.4.38` pushed → release.yml publishing.

## Technical Details
- **Orchestration**: 3 waves of `general-purpose` agents in `isolation: worktree`, run in background. Wave 1 = #73/#74/#77 (Rust) + #81 (Python); wave 2 = pyo3 + #80 + #82; wave 3 = #69/#72/#75. Each agent: read the issue, implement, add tests, update AGENTS.md+CHANGELOG+cache-version in lockstep, push branch, open PR with CI.
- **OOM discipline** (Omarchy 16 GB): wave 1's shared `/tmp` target dir is **tmpfs (RAM)** — caused a linker bus error + disk-quota error under concurrent builds. Fix: later waves used disk-backed `$HOME/.cache/...` shared target dirs; cargo's build lock serializes the heavy compiles so only ~1 runs at a time. Python agents reused the prebuilt `_core.abi3.so` (copied into their worktree) — no Rust compile.
- **Merge conflict pattern**: every PR touched AGENTS.md + CHANGELOG + `_CACHE_SCHEMA_VERSION`, so all but the first in each wave conflicted. Resolved each with a `git merge origin/main` **merge commit** (no force-push, per the force-push gate) — keep both doc/changelog entries, keep the single shared cache version. Build-tested the two merges where both sides edited the same Rust file (#99: indexer; #105: extractors vs #104's lexer API churn) before merging.
- **cargo-audit**: was red on every branch from two pyo3 advisories *published 2026-06-14*. No branch protection on `main`, so it didn't block merges; #102 cleared it. Confirmed green on #102's pyo3-0.29 state before declaring release-ready.
- **Release**: pattern is bump version in 3 files + finalize `## [Unreleased]`→`## [x] - date` + `chore(release):` commit pushed to main, then `git push origin v<x>` triggers release.yml (wheel matrix → PyPI). v0.4.37 shipped cache v16, so v0.4.38 is v16→v19.

## Next
- ~~Verify v0.4.38 published~~ — **DONE: release.yml run 27502094288 completed `success`, full wheel matrix built + PyPI publish job green. v0.4.38 is live on PyPI.**
- **Coordinator features** (the big open thread, user wants to tackle next, architect-level): #92 clash triage (systematic vs one-off, 1832→306 measured), #93 clash reporting feedstock (overlap centroid + grids + BCF + heatmaps), #94 "mesh is truth" (geometric_storey + location_reliable).
- **Remaining correctness** (smaller, deferred): #88 (storey_guid aggregates — needs a filter-vs-products_in design call), #71/#76/#87/#89 batch.
- **#90 (ToghrolTP, external)** — likely partly superseded by merged #77/#80; needs triage (don't close/comment without user).

## Notes
- All "Next" backlog items already exist as GitHub issues — no new issues filed (avoided duplicates per the gh-issues-canonical-backlog convention).
- Pre-existing repo state to know: `cargo clippy` shows ~67 lint errors on a bleeding-edge local toolchain (rust 1.95) and `cargo fmt` is not clean on main — **CI runs neither**, so these are out of scope and shouldn't be "fixed" reactively.
- Local checkout stayed on `feat/reroute-primitives` (reroute work, GH #63) the whole session — untouched; that branch is still 7 ahead/unpushed per prior memory.
