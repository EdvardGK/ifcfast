# Full review + consolidation — write-axis audit, backlog sweep, docs sync

## Agent signature
- **Agent**: `claude-fable-5`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `df678fa` → `1df2bd9` (4 commits this session, all pushed)
- **Session scope**: full review + backlog consolidation — deep static review of the shipped write-axis arc, 30-issue backlog audit, docs-contract drift sweep, repo hygiene
- **Touched paths**: .gitignore, README.md, AGENTS.md, CHANGELOG.md, python/ifcfast/__init__.py, python/ifcfast/model.py, docs/worklog/ (backfill commit)
- **Parallel sessions observed**: none (only this session's commits on origin/main during the window)
- **Supersedes / superseded by**: none

## Summary

Ed asked for a full review + consolidation before continuing the write
axis. Ran as coordinator with three parallel agents (Fable review
sub-coordinator, Opus backlog auditor, Sonnet docs auditor) while
running tests + hygiene inline. Everything verified/actioned below is
pushed; GitHub mutations were comments + new issues only (closes are
permission-gated → queued for Ed).

### Deep review of the write-axis arc (`4b17c4c^..df678fa`)

Round-trip emit core is **solid** (byte-identity by construction; all
13 pinned rel indices verified correct for IFC2x3 + IFC4). Four majors
found, adversarially verified, filed:

- **#128** `fmt_real` is `format!("{x:?}")` → emits `NaN`/`inf` and
  exponent-without-decimal (`5e-5`) — invalid ISO-10303-21 REALs; no
  finiteness check anywhere in the chain (hotswap.rs:448).
- **#129** subset retains rels without closing their non-anchor refs →
  dangling OwnerHistory on multi-OwnerHistory files; IFC4
  `IfcPropertySetDefinitionSet` pulls nothing (`_ => Vec::new()` in
  rel_rules field_refs) yet the rel names the set verbatim. Violates
  the stated zero-dangling guarantee (subset.rs:109-135).
- **#130** hotswap orphan-GC skips shape_rep in the refcount → can
  delete the geometric (sub)context the swapped rep still references;
  a subset→hotswap pipeline on a single-element file hits it.
- **#131** the #121 trust band has no lower-bound protection — an
  inconsistently-wound open shell cancels to an in-band under-report
  AND lost its GH #60 oracle routing.
- **#132** minors: Rust/Python corpus gates read different env vars
  (`IFCFAST_CORPUS` vs `IFCFAST_SUBSET_CORPUS`), `--ignored`+unset-env
  reports green with zero assertions, lexer last-`;` comment trap,
  `IfcRelAssignsToGroupByFactor` dropped, styled items dropped,
  shared-PDS sibling mutation, ifczip out_path writes plain STEP.

Verdict: **#128/#129/#130 land before attribute mutation (#133) and
before quoting the zero-dangling guarantee.**

### Backlog audit (30 open issues, evidence-checked)

- **Ready to close (queued for Ed, evidence in-thread):** #56 (fixed
  `f8b4cf2`, tester recommended twice), #116 (fixed `0224de1`), #6
  (obsolete note), #119 (→ merged into #62), #23 (→ merged into #67),
  #20 (umbrella complete; remnant spun out as #134).
- **Recluster:** QTO-accuracy = {#62 (+#119), #123}; #120 moved to the
  #58/W6 cut-path cluster with #64 (telemetry tail). #51 trimmed to
  cargo-audit CI + zip/arrow bumps.
- **#124 rescoped:** headline shipped; remaining = #127 bridge,
  #133 attribute mutation (new), CLI subset surface, #128–#130 hardening.
- **New issues filed:** #128–#132 (review), #133 (attribute mutation),
  #134 (meshopt+zstd).
- **Data-loss flag resolved:** `feat/reroute-primitives` had unpushed
  commits → pushed to origin.

### Docs-contract drift (fixed + committed `16555c9`, `1df2bd9`)

- `ifcfast.system_prompt()` was stale since ~v0.4.20 despite the
  "additions only" promise — now covers by_type, mesh(guid), iter_*,
  to_gltf, subset/hotswap, bundle()/clash(), strict, unit_scale, full CLI.
- README said "read-only by construction" (false since #124) and
  "property variants skipped" (false since v0.4.29) — both fixed.
- AGENTS.md: added `ifcfast types`, the 3 undocumented rel rules
  (Nests/AssignsToGroup/Declares), MCP `list_open()`/`close()`,
  clash `include_classes` example.
- model.py hotswap docstring example was itself the #127 trap
  (`m.meshes()[0]  # local-frame` — wrong); fixed + stats `product`
  key + real exception types documented.
- CHANGELOG `[Unreleased]` was missing the entire write-axis arc — added.

### Repo hygiene (`104bad3`, `7cf7b6b`)

- An auto-checkpoint (`951dfbe`) had committed **85 MB node_modules
  (2561 files)** + a dead root package.json (@google/model-viewer,
  referenced by nothing) — untracked; `-o/view.sql` + empty
  `--help.bundle` dir (misfired CLI, May 21) trashed; `scratch/`,
  `.claude/`, `node_modules/` now gitignored.
- Six worklogs (Jun 15 – Jul 2) were sitting untracked — committed.

### Verification state

- `cargo test -p ifcfast-core --no-default-features`: green (exit 0).
- `cargo check -p ifcfast-core --features python,mesh,csg`: green —
  no pyo3 stats-field drift at type level.
- Review was static-only (no maturin on this box mid-session); the
  #128–#130 findings are code-grounded, not runtime-repro'd.

## Next

1. **Harden the writer:** #128 (fmt_real — small), #129 (rel-closure —
   the real one), #130 (GC seed fix — small). Add the fixtures from the
   review's test-gap list (multi-OwnerHistory, PropertySetDefinitionSet,
   uniquely-owned-map + subcontext, NaN/1e-5 coords).
2. **Ed:** run the queued closes — #56, #116, #6, #119, #23, #20
   (one-liners in each thread's last comment).
3. Then #127 local-frame bridge → #133 attribute mutation.
4. Release note: main carries unreleased #121 fix + whole write axis;
   next tag bundles them (per bundle-releases convention).

## Open questions
- None blocking. #131 (trust-band lower bound) is a design question —
  winding-consistency check vs a distinct volume_method for the
  low-fill band — worth 10 minutes of Ed's opinion before implementing.
