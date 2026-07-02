# v0.4.40 released + GH #131 winding gate: implemented, corpus-refuted, codified

## Agent signature
- **Agent**: `claude-fable-5`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `4a25f18` → `0697432` (2 commits + tag `v0.4.40`, all pushed)
- **Session scope**: same session as the hardening worklog, continued — Ed said "own it": released v0.4.40, then resolved the #131 design question empirically
- **Touched paths**: Cargo.toml, pyproject.toml, Cargo.lock, CHANGELOG.md (release stamp), crates/core/src/mesh/qto.rs, python/ifcfast/header.py (bumped 25 then reverted to 24)
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: continues 2026-07-02-21-30_write-axis-hardening

## Summary

### v0.4.40 released
Ed's standing conventions (autonomous `v*` tag-push + bundle-releases)
covered it. Tag pushed, CI green on all 4 platforms + sdist,
**published to PyPI**. Contents: write axis (subset + hotswap), #127
local-frame bridge, #121 open-shell QTO fix (cache schema 24),
#128/#129/#130 hardening.

### GH #131: the empirical answer

Ed declined the design question ("think like an owner"). So instead of
choosing between options a priori, I implemented the winding gate and
let the corpus judge:

1. Snapshotted per-GUID `volume_method` on ARK+RIB (v0.4.40 wheel).
2. Implemented the gate (2 % bad-edge tolerance on welded adjacency),
   3 unit tests green, schema bumped.
3. Diffed: 110 rows flipped mesh_open → prism_fallback.
4. **Oracle-checked all 110** by carving them into `m.subset()` files
   and running ifcopenshell on just those (dogfooding the write axis
   as oracle-input carving — 380k-record subset from 2.8M-record ARK).

**Verdict: refuted.** 104/110 flipped rows were at kernel parity
(median rel err 0.000); the prism substitute was off by median +112 %.
Entire genuine catch: one proxy (0.71 vs 1.48 m³) and a 2-litre plate.

**Root insight (now in memory + code comments):** the correctness
contract is *oracle parity*, and ifcopenshell computes the same
signed-tetra divergence sum over the same authored geometry — authored
winding incoherence (double-sided panels, flipped faces) cancels
identically on both sides. A validity predicate must predict ORACLE
divergence, not physical divergence.

**Shipped state (`0697432`):** zero behavior change (verified 0 flips
/ 0 volume deltas vs v0.4.40). `winding_coherent()` kept as a
documented building block; `winding_gate_tests` includes a tripwire
test asserting incoherent-but-in-band shells stay trusted BY DESIGN.
Schema back at 24. #131 rescoped to the measured 2-row residue
(0.016 % of reliable products), low priority.

## Next
1. **#133 attribute mutation** — the last write axis. Emit path
   hardened; API mirrors subset/hotswap.
2. #132 items 2–8; #122 mesh-coverage gap; #123 degenerate collapse.
3. QTO frontier for 0.5.0: #62 (windows shell-closing) is the big one.

## Open questions
- None. The #131 question Ed was asked is answered empirically and
  documented in-thread.
