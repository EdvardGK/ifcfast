# Session: cut-openings W3 + W4 — unit-aware tolerance + operator-aware IfcBooleanResult

## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `7a55c3b` → `f8b4cf2` (2 commits this session)
- **Session scope**: ship GH #58 Phase-1 items W3 (unit-aware cut tolerance) + W4 (operator-aware IfcBooleanResult), and reframe the manifold-replacement design doc to the full-retirement direction
- **Touched paths**: `crates/core/src/mesh/cut_validate.rs` (new), `crates/core/src/mesh/{halfspace_clip,boolean,cut_openings,mod}.rs`, `crates/core/src/lib.rs`, `crates/core/tests/{cut_openings_integration,cut_openings_proptest}.rs`, `python/ifcfast/header.py`, `AGENTS.md`, `docs/plans/2026-06-05_cut-openings-manifold-replacement.md`
- **Parallel sessions observed**: none on `origin/main` during the session window
- **Supersedes / superseded by**: continues the manifold-replacement programme; previous slice was `2026-06-06-12-58_ifcopenshell-fork-evaluated-oracle-direction.md`

## Summary

Picked up the next-steps queue: (3) reframe the design doc (done first, 1 commit) then W3+W4 (one feat commit). W1+W2+W5a had already landed (`1924497`, `3092ae4`), so the `Outcome::Unsupported` taxonomy + chain helpers + proptest baseline were in place to build on. Both W3 and W4 ship default-on, no new feature flag — exactly the foundation-phase posture the plan demands. Cache schema 13→14. Pushed to `main` (`f8b4cf2`); GH #58 updated with the full landing note. No release tagged — bundling W3+W4 with W6+W9 for one Phase-1 release.

## Changes

**Commit 1 — `e009278` docs(plans): reframe to full-retirement direction.** TL;DR and feature-flag-strategy sections rewritten to the phased retirement (Phase 1 W3+W4+W6+W9 → ~95% pure-Rust; Phase 2 W11+W12 reframed replace-not-fallback → 99%+; Phase 3 manifold behind opt-in flag, default drops C++ dep, GH #28 unblocks). Banner clarifies the audit findings F1–F6 / work plan W1–W17 / risks below are unchanged and still load-bearing — only the decision + endgame flipped. Oracle harness (#59) noted as companion.

**Commit 2 — `f8b4cf2` feat(csg): W3 + W4.**

*W4 — operator-aware `IfcBooleanResult` (fixes F4).* `boolean::second_operand_role(operator)` maps `.UNION.` → `boolean_union_operand`, `.INTERSECTION.` → `boolean_intersection_operand`, `.DIFFERENCE.` / missing / unreadable → `boolean_second_operand`. Only `boolean_second_operand` is a cutter (`is_cutter` unchanged), so union/intersection operands are no longer subtracted in cut mode — pre-W4 they were, silently producing `first − second`. `cut_openings::detect_unsupported_boolean_op` surfaces `UnionWithOverlap` / `IntersectionNotImplemented` when such an operand is present and there's no DIFFERENCE cut to perform. New tags added to `MeshFragment::source_tags()`.

*W3 — unit-aware tolerance + manifold classifier (fixes F2).* New `mesh/cut_validate.rs` (csg-gated) owns `on_plane_eps(unit_scale) = ON_PLANE_EPS_BASE_M / unit_scale` (physical 1 mm in source units) and `is_manifold_mesh` (thin wrapper over `qto::is_closed_manifold`). `halfspace_clip::clip_by_plane` gained an `on_plane_eps: f32` param (the old hard-coded `ON_PLANE_EPS` const is now `pub ON_PLANE_EPS_BASE_M`, the metre-base). `cut_openings::apply` and `CrossProductCut::flush` gained a `unit_scale: f32` param, threaded from `self.unit_scale` at the three cut-path sinks (QtoSink / MeshSink / GltfSink) in `lib.rs`. `Outcome::Unsupported(NonManifoldInput)` wired via `classify_subtract_failure` as a **post-failure classifier** on the manifold subtract `Err` path (in-rep + cross-product) — never a pre-gate.

## Technical Details

- **Why the metre-base tolerance choice.** Setting `ON_PLANE_EPS_BASE_M = 1e-3` (the historical constant) makes metre files (`unit_scale=1.0`) byte-identical to pre-W3 — preserving the Sannergata baseline and the proptest's all-green state — while making the tolerance a consistent physical 1 mm everywhere. mm files go from a meaningless 0.001 mm snap to a correct 1 mm; foot files from 0.0003 m to 1 mm. Proved consistency with a test asserting `eps * unit_scale == 1e-3 m` across mm/m/ft.
- **Why NonManifoldInput is post-failure, not a pre-gate.** `is_closed_manifold` merges edges by *index*, so it false-negatives on a visually-closed mesh whose vertices aren't index-welded (manifold-csg welds by *position*). Using it to *skip* a cut would suppress real openings on un-welded inputs. Running it only after the kernel already `Err`d makes the false-negative harmless: the kernel decides whether the cut happens, the classifier only labels the failure.
- **HostConsumed deferred.** It's listed under W3 in the plan but has semantic friction with `Outcome::Unsupported` ("cut could not be performed, host left as-is") — a consumed host is the opposite (cut succeeded, mesh emptied). Kept as `Cut`-with-empty-output; revisit when QTO-gone tracking is designed. `CoplanarFaceDegeneracy` / `MalformedHost` / `BspDepthExceeded` need representation-type / recursion plumbing not in `apply()` → fold into W11.
- **Validation.** `cargo check --features csg` clean; `cut_validate` (3) + `boolean` (1) + `cut_openings` (4 new) lib tests; 141 lib + 12 integration + 4 proptest all green under `--no-default-features --features csg`; full `cargo test --release` exit 0; `RUSTFLAGS=-D warnings cargo check --all-targets --features csg` clean.

## Next

1. **W6 — tight polygonal-bounded halfspace** (fixes F6). Blocked on ~1h research first: i_overlay v7 polygon-with-holes API (NonZero vs EvenOdd, single shape vs separate `add_shape`) + f32-vs-scaled-i64 robustness at building scale. Both gate W9 too.
2. **W9 — pure-Rust prism-minus-prism** (the load-bearing replacement; feature-flag `prism-csg-fast`, gated on the proptest baseline). The depth item of Phase 1.
3. **Phase-1 release** — bundle W3+W4 with W6+W9, tag once. Cache is already at 14.
4. **GH #59 M1 oracle scaffold** — runs in parallel, no code conflict.
5. Still open external: GH #56 / #57 tester response (reshapes W11 vs W15 priority).

## Notes

- The W3+W4 cache bump (13→14) means substrate `source` column gains `boolean_union_operand` / `boolean_intersection_operand` tokens and net meshes shift for non-metre + boolean-union products. No release picks this up until the Phase-1 bundle — that's intentional per the bundle-releases convention, but worth flagging to the viewer integrator (GH #20) when the release lands, since their `source`-tag parser should learn the two new tokens.
