## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `fb9b07d` → `fb9b07d` (no commits made this session — changes uncommitted in working tree)
- **Session scope**: Implement the GH #121 open-shell QTO routing fix (trust mesh volume over inflated prism for doors/windows/railings) + retune the W4 collapse backstop.
- **Touched paths**: crates/core/src/mesh/qto.rs, crates/core/tests/cut_openings_integration.rs, python/ifcfast/header.py, python/ifcfast/model.py, AGENTS.md, CHANGELOG.md
- **Parallel sessions observed**: another local session running concurrently (user-reported, shares the 16 GB box; no origin/main commits landed during window — branch still at fb9b07d). `.claude/worktrees/agent-*` present in tree.
- **Supersedes / superseded by**: follows 2026-06-22-16-38_qto-corpus-sweep-g55-ark-114-fixed-121-filed (which filed #121); none supersede this.

## Summary
Implemented the GH #121 fix: `mesh_qto` was over-counting open-shell doors/windows/railings 40–66× because the lower-bound collapse tripwire (`mesh_vol < prism × 0.1`) mistook a legitimately-small open-shell volume for a W4 CSG collapse and substituted the inflated prism — even though the mesh signed-tetra volume already matched the ifcopenshell kernel. Retuned the routing to trust an open shell's mesh volume within `min(prism, AABB)` and pinned the collapse backstop at a near-zero `< bound × 1e-3`, added a new `volume_method = "mesh_open"` to distinguish trusted open shells from closed manifolds. Full core suite green (243 tests); **not yet validated against the G55_ARK oracle** (deferred — requires a maturin `.so` build that risks OOMing the parallel session).

## Changes
- **`crates/core/src/mesh/qto.rs`** — the routing rewrite in the non-closed branch of `compute()`:
  - New `upper = min(prism, aabb)` two-sided upper bound; over-report tripwire and trust gate both key on it (catches inverted open shells where `vol > box` too).
  - Collapse guard: `LOWER_FRAC 0.1` → `COLLAPSE_FRAC 1e-3`, compared against `upper` not `prism`. Only a genuine mesh→~0 vs a real multi-litre solid escalates now.
  - Trusted open shell returns `(volume_mesh_m3, "mesh_open", true, prism)` instead of `"mesh"`.
  - Struct-field doc for `volume_method` documents the `mesh`/`mesh_open`/`prism_fallback` tri-state.
  - Tests: updated `open_shell_within_prism_bound_keeps_mesh_value` to expect `"mesh_open"`; added `open_shell_low_fill_keeps_mesh_not_inflated_prism` (synthetic at fill 0.033 — in the old mis-fire band — proving the threshold move).
- **`crates/core/tests/cut_openings_integration.rs`** — drive-by: fixed the *pre-existing* stale test `tight_bounded_halfspace_default_over_cuts` (was already red on HEAD before my edit). Commit `add0465` (GH #114) made the default cut path honor the bounding polygon → 0.48 m³, contradicting the test's obsolete "over-cuts to 0.20" premise. Renamed to `..._default_honors_polygon`, asserts ~0.48.
- **`python/ifcfast/header.py`** — `_CACHE_SCHEMA_VERSION` 23 → 24 with a v24 changelog comment (open-shell `volume_m3`/`volume_best_m3`/`volume_method` change; closed rows byte-identical).
- **`python/ifcfast/model.py`** — `meshes()`/QTO docstring: documented `mesh_open` and the new trust semantics.
- **`AGENTS.md`** — volume-reliability section: documented the open-shell trust gate, the #121 door/window/railing rationale, and the `mesh_open` value.
- **`CHANGELOG.md`** — Unreleased entries for #121 and the #114 test fix.

## Technical Details
- The bug signature: a thin glazed door's true volume is ~1.5–2.5 % of its min-prism bound (footprint × depth treats the full face as filled, but only the frame + thin glass is material). That low fill ratio is geometrically identical to a W4 over-subtraction collapse — they cannot be distinguished by fill ratio alone. The principled resolution: for open shells the signed-tetra mesh volume IS our best estimate (it matches the reference kernel, which does the same divergence integral), so trust it; only an *absolute* near-zero (`< bound × 1e-3`) — the genuine W4 "→0" signature — escalates. Accepted blind spot: a *partial* collapse (vol drops to e.g. 30 % but not ~0) is undetectable without the oracle; this matches the documented W4 threat model (near-total collapse) and the project's speed-first-with-kernel-escalation philosophy.
- No Rust/Python code branches on the `volume_method` string (verified by grep) — it is pure pass-through to the parquet column, so adding `mesh_open` is safe; only docstrings enumerated the old values.
- The `collapse_to_sliver` backstop test still passes: its volume <1e-3 is well under `upper × 1e-3 ≈ 9e-3`, so the near-zero collapse still escalates.
- Per `feedback-corpus-differential-over-synthetic`: synthetic tests only lock the contract; the *real* gate is the G55_ARK oracle re-run, deferred this session for OOM safety.

## Next
1. **Validate against G55_ARK oracle** (the real gate, next-steps item 1 carried forward): build the `.so` via maturin (HEAVY — only when the parallel session frees RAM; never `--release` on the 16 GB box), re-run the geometry-QTO differential. Expect doors/railings to collapse from 40–66× over-count to kernel parity. Confirm 0 regressions on walls/slabs (closed rows are byte-identical by construction, so risk is only on open shells).
2. **Commit** the change once the oracle confirms (single commit: `fix(qto): trust open-shell mesh volume within bound, retune collapse tripwire (GH #121)`), then **close #114** (user's call — RIB + ARK both confirmed).
3. Windows / real glazing still need shell-closing (#62) — mesh volume can still mis-estimate genuinely-open glazing; #121 only stops the *prism substitution*.
4. ~1300 missing_in_ours on ARK — scope skipped reps (mapped-item coverage).
5. Widen sweep to G55_RIV / Sannergata for the full v0.5.0 damage report before tagging.

## Notes
- **Nothing is committed.** All changes are uncommitted in the working tree; branch is still `fb9b07d`. A `/clear`'d session must `git diff` to see this work.
- **OOM constraint was live this session** — user warned a second uncontrolled session shares the box. Avoided the maturin build entirely; only the cached debug `cargo test` ran. Honor this before any heavy build.
- The `.so` in `target/` is stale relative to this change (built from pre-#121 source). Any Python-level check must rebuild first.
