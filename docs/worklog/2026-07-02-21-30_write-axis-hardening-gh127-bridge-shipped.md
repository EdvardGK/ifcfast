# Write-axis hardening (#128/#129/#130) + GH #127 local-frame bridge shipped

## Agent signature
- **Agent**: `claude-fable-5`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `1f9c12e` → `4a25f18` (5 commits + 1 merge this session, all pushed)
- **Session scope**: build phase after the review session — fix the three deep-review majors, ship the #127 local-frame hotswap bridge, execute the authorized issue closures
- **Touched paths**: crates/core/src/doc/{hotswap,subset,rel_rules}.rs, crates/core/tests/{doc_hotswap,doc_subset,doc_rel_rules,doc_roundtrip}.rs, tests/fixtures/{hotswap_subcontext,subset_owner_history}.ifc, tests/{test_subset,test_hotswap,test_mesh_local_frame}.py, CHANGELOG.md; via merged agent branch (`4079d22`): crates/core/src/{lib.rs,mesh/mod.rs}, python/ifcfast/model.py, AGENTS.md, tests/fixtures/hotswap_roundtrip.ifc, crates/core/tests/mesh_item_local.rs
- **Parallel sessions observed**: one subagent worktree branch `worktree-agent-aaa68fb1d8af53ba0` (spawned by this session, merged in `4079d22`); nothing external
- **Supersedes / superseded by**: continues 2026-07-02-17-45_full-review-consolidation

## Summary

Ed authorized issue closures and said "build". Executed the plan from
the review session.

### Hardening majors — fixed, regression-tested, closed

- **#128** (`b62ee02`): `fmt_real` rejects non-finite coords as
  BadMesh; exponent REALs re-insert the decimal point (`1.0e-5`).
- **#130** (`b62ee02`): `gc_orphans` now counts the swapped rep's
  post-swap edges (all fields except the replaced Items), so a
  uniquely-owned-map swap can no longer cascade-delete the
  subcontext. New fixture `hotswap_subcontext.ifc`.
- **#129** (`d688f09`): `parse_rel`'s pull set is every ref outside
  the anchor field (per-field `scan_ref_tokens`) — OwnerHistory,
  typed pulls, and IFC4's inline `IfcPropertySetDefinitionSet` all
  close automatically; dropped anchor participants stay excluded.
  New fixture `subset_owner_history.ifc`.
- Every fix's regression test was verified to FAIL on pre-fix code
  (git stash trick) before trusting green.
- **#132 item 1** (`d688f09`): corpus gates unified on
  `IFCFAST_CORPUS` (legacy var still read); Rust gates PANIC on
  `--ignored` without corpus instead of green-with-zero-assertions.

### GH #127 local-frame bridge — shipped via subagent worktree

Fable subagent built it statically (no builds on its side); merged
in `4079d22`, compiled and validated here first try.

- `frame="local"` on `m.mesh()` / `m.meshes()` / `iter_meshes()`:
  representation-item frame in NATIVE source units == exactly what
  `doc::hotswap` writes back (world bake with identity placement
  chain, rep_origin rebase retained, f64-anchored).
- Every mesh row carries `placement` (world_from_local 4x4, flat
  row-major; f32 rotation + f64 translation).
- `frame="local"` loudly rejects `cut_openings=True` and
  non-default `unit=` — both would corrupt the hotswap payload.
- AGENTS.md warning replaced with the working
  extract-local → decimate → hotswap pattern.

### Validation (all green, this box, serialized)

- Rust: full core suite (no-default-features), `mesh_item_local`
  (--features mesh), corpus gates over ALL FOUR local G55 files —
  byte-identity on ARK (2.8M records), rel indices, 0-dropped-deps.
- Python (debug wheel via maturin develop): full suite **225
  passed**; ifcopenshell corpus oracle for subset+hotswap (21
  passed); **local-frame corpus round-trip gate 14 passed** —
  local-extract → identity hotswap → reopen → world-frame compare
  on ARK/RIB/RIE/RIV.

### Tracker

Closed this session: #56, #116, #6, #119, #23, #20 (review-session
queue, Ed-authorized) + #128, #129, #130, #127 (fixed/shipped).
Commented status on #132 (item 1 done, 2–8 open).

## Next

1. **#133 attribute mutation** — last write axis; emit path is now
   hardened, safe to build on.
2. **#131** trust-band design call (winding-consistency check vs
   distinct volume_method) — still wants Ed's 10 minutes.
3. #132 items 2–8 (lexer last-`;` trap, AssignsToGroupByFactor,
   styled items, shared-PDS warning, ifczip out_path, error-type
   normalisation).
4. Release: main now carries #121 fix + whole write axis + #127
   bridge + hardening — a strong v0.4.40 (or 0.5.0-rc) bundle.

## Open questions
- None blocking.
