## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `26c79ec` → `97d58d9` (2 commits this session)
- **Session scope**: verify + commit prior session's GH #21 work, then start GH #22 (csg-smoke workflow)
- **Touched paths**: `.github/workflows/csg-smoke.yml`, `docs/worklog/2026-05-31-14-40_gh21-commit-and-gh22-csg-smoke.md` (this file). The GH #21 files were modified by the prior session, only staged + committed here.
- **Parallel sessions observed**: none (origin/main last touched at `26c79ec` before session start; no new origin commits during the window)
- **Supersedes / superseded by**: builds on `2026-05-31-14-13_gh21-cross-product-opening-cuts.md` (which described uncommitted work); does not supersede it

## Summary
Ed opened the session unable to test interactively, asked me to either self-test or move on. Verified the prior session's uncommitted GH #21 work was sound — all 7 `cut_openings_integration` tests pass under `--features csg`, full `_core` test suite green (mesh_reveal 13, bundle 4, clash 4, etc.), default-feature build clean, all-features `cargo check` clean — and committed it as `a43ad4f`. Then started GH #22 by adding `.github/workflows/csg-smoke.yml`: informational matrix job (linux x86_64, linux aarch64, windows x64, macos x86_64, macos aarch64) running `cargo check --features csg` plus the `cut_openings_integration` release-mode test on every push to main / PR / manual dispatch. Both invocations verified locally before commit. Committed as `97d58d9`.

## Changes
- **`a43ad4f` (GH #21)** — committed the prior session's work as-described in `2026-05-31-14-13_gh21-cross-product-opening-cuts.md` (`crates/core/src/mesh/cut_openings.rs` CrossProductCut + Routed, `crates/core/src/lib.rs` MeshSink wrapper + flush, 4 new integration tests, `AGENTS.md` + `python/ifcfast/model.py` doc updates). No code edits from this session — only the commit.
- **`.github/workflows/csg-smoke.yml`** (new, `97d58d9`) — separate workflow, not a `ci.yml` job. 5-platform matrix mirroring `release.yml`'s wheel targets (linux x86_64 via `ubuntu-latest`, linux aarch64 via `ubuntu-24.04-arm` for native ARM, windows x64, `macos-15-intel` for Intel macOS, `macos-latest` for Apple Silicon). Each runner does `cargo check -p ifcfast-core --features csg` then `cargo test -p ifcfast-core --features csg --test cut_openings_integration --release`. `fail-fast: false` so we see per-platform breakage. Informational only — not wired into ci.yml's required gates.

## Technical Details
**Why a separate file and not a ci.yml job.** The `manifold-csg` cmake build adds non-trivial wall time per platform and the issue explicitly wants it informational until proven stable. Keeping it in its own workflow file means it can be silenced by editing one file or by removing one matrix row, without churning the gating ci.yml history. Promotion-to-required is a separate, deliberate edit later — see GH #22 acceptance.

**Why `-p ifcfast-core --features csg` and not `--manifest-path`.** `cargo` resolves features at the workspace level when invoked from the root, but only as long as you've named the package with `-p`. The shorter `--features csg` from root fails with "cannot specify features for packages outside of workspace" (which I hit and corrected during local verification — kept the workflow's flags aligned with what actually runs). Same problem the prior session likely would have hit if they'd tried to run the smoke locally.

**Why release-mode for the test.** `cut_openings_integration` exercises the C++ Manifold side end-to-end. Debug-mode mesh ops are very slow on macOS runners specifically; release is what `release.yml` actually publishes anyway. Compile-time penalty (3 min on my box, probably 5–8 on hosted runners) is the right trade for an honest smoke.

## Next
1. **Push these two commits + watch the first csg-smoke run.** Both commits are LOCAL on my machine. The push isn't autonomous-authorized — Ed wanted to make sure the work was sound before publishing.
2. **After 2 clean csg-smoke runs across all 5 platforms**: flip `crates/core/Cargo.toml` `default = [..., "csg"]`, update CHANGELOG, drop the "requires `csg` feature" disclaimer from AGENTS.md + the `cut_openings` docstring, tag `v0.4.20`. That's the GH #22 acceptance.
3. **GH #20 P0 #2 — rayon** parallelization of `mesh_ifc_streaming`. Independent of (1). Sizable change (workspace dep + sink-thread-safety audit + reordering or batched-flush of the streaming loop) — own session.
4. After v0.4.20 ships, close GH #20 or split it.

## Notes
- Two commits unpushed at session end: `a43ad4f` and `97d58d9`. Push gate is intentional per "external writes" rule.
- Prior worklog's "Branch: ... → 26c79ec (no commits made this session)" is now stale (the commits described in it are in `a43ad4f`); forward-only convention means I don't backfill it. This worklog is the authoritative record of the commit.
- AGENTS.md upkeep: the prior session already updated the cut-openings paragraph to drop the cross-product disclaimer. No additional AGENTS.md work needed for `csg-smoke` — workflow plumbing isn't an agent-visible primitive.
