# Session: The correctness leap — oracle keystone + 3 QTO/loud-failure fixes (coordinator + agent swarms)

## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `a9330c6` → `395ab7c` (6 commits this session)
- **Session scope**: Set ifcfast's next-leap strategy (certified correctness, not feature breadth) and ship the first Layer-1 pillars — oracle harness, far-origin precision, mesh-welding/tripwire, strict-mode loud failure — orchestrated as adversarially-verified agent swarms.
- **Touched paths**: docs/plans/, docs/worklog/, tests/oracle/, tests/fixtures/{materials,no_lengthunit,broken_conversion_unit,latin1_encoded}.ifc, tests/test_strict_mode.py, crates/core/src/mesh/{qto,stats}.rs, crates/core/src/lib.rs, crates/core/src/bin/mesh.rs, python/ifcfast/{model,header,mcp_server,cli}.py, AGENTS.md, .github/workflows/ci.yml, pyproject.toml
- **Parallel sessions observed**: none (all 6 commits on origin/main are this session's)
- **Supersedes / superseded by**: none

## Summary
Defined the strategic next leap for ifcfast: stop competing on breadth (ifc-lite et al. win that) and win on **certified correctness in a narrow lane** — "the IFC tool whose QTO and mesh you don't re-check," gated by an ifcopenshell differential oracle, shipped as 0.5.0. Then executed the first four of five Layer-1 pillars as a coordinator driving agent swarms, each with a built-in adversarial verify/refute stage. That stage repeatedly earned its place: it **refuted my own #114 root-cause** (saving a wrong fix) and **caught a latent size-relative-eps bug** and a **Norwegian-encoding default-flip regression** before they shipped.

## Changes
- **Roadmap** (`docs/plans/2026-06-20_layered-correctness-roadmap-0.5.md`, commits `7ce2101`/`78a4dd6`): layered plan. L1 certified core (oracle, precision, QTO, cut-openings-as-base-logic, loud failure) gates 0.5.0; L2 (voxel #115, clash, reroute #63) sequenced on top — parked but not abandoned. Folded in recon corrections.
- **Oracle harness #59 M1** (`72f8419`): `tests/oracle/` with reusable `normalize.py` (tolerance/ordering DSL) + `report.py` (typed DisagreementRecord, collect-all not first-failure) + conftest (single importorskip, dual-open corpus). Diff adapters: quantities (lifted), psets, materials, classifications vs ifcopenshell 0.8.5 (pinned, dev-only). New oracle CI job. Hardened: clean-skip without the dev extra, value_type-aware pset coercion, anti-vacuous-green asserts.
- **#116 far-origin precision** (`0224de1`): f32 signed-tetra accumulator cancelled catastrophically on the `BakeFrame::World`/`bundle()` path at georef scale (1 m³ → ~6672 m³). Fix = AABB-min rebase + f64 accumulate (volume + surface area) in `qto.rs`/`stats.rs`; centroid/bbox stay absolute. UTM regression test. `_CACHE_SCHEMA_VERSION` 22→23.
- **Welding + tripwire** (`bb979b4`): coordinate-welding pre-pass (0.1 mm physical grid, gated to non-closed branch) so step_id-dedup/CSG-fragmented watertight meshes classify `closed` instead of demoting to prism. Lower-bound tripwire closes the one-sided #60 gap (collapse-to-~0 now flags `volume_reliable=false`). 11 adversarial tests.
- **Strict-mode loud failure #73/#72** (`395ab7c`): `strict=True` default raises on silently-wrong NUMBERS (unresolved LENGTHUNIT was read as metres); `length_unit→'unknown'` + `unit_resolved` flag. Single `_strict_signal` channel. Encoding lossiness (latin-1 Norwegian) WARNS not raises; UNKNOWN schema raises. Threaded through open/header/summary/diff/MCP/CLI. AGENTS.md strict section.

## Technical Details
- **Method = coordinator + Workflow swarms.** Five workflows this session: (1) read-only recon fan-out (6 pillars + a skeptic), (2) oracle M1 build (foundation → parallel adapters → verify-runs-pytest → review), (3) #116, (4) welding/tripwire, (5) strict mode — each ending in adversarial verification + a code-reviewer agent. Findings folded back via targeted fix agents.
- **The skeptic disproved #114 with a numeric profile**: f32 cancellation gives ~0% error <1 km offset, sign-flipped garbage at UTM — never a clean 9–18%. So #114 is a void-subtraction discrepancy (cut-openings, L1), and the *real* f32 bug is the World-path one → split to #116; the storage-quantization residual → #117.
- **RAM discipline (Omarchy 16 GB)**: recon read-only; oracle/strict on tiny fixtures against the editable wheel (no rebuild); Rust fixes a single scoped `cargo test` each, phases sequential so builds never overlap; all heavy CI builds remote. Zero OOM.
- **ifcfast is editable-installed** (`ifcfast.pth` → repo tree), so pure-Python edits are live without maturin — key enabler for the strict-mode swarm.

## Next
- **mesh_qto + sql MCP tools** (Layer 1.5, RAM-light, unblocked). mesh_qto is mechanical (mirror the psets tool) but geometry-heavy → needs a guid filter + surface-row cap. `sql()` needs a design call first (read-only enforcement, which substrate dir to bind) — not purely mechanical.
- **Oracle sweep** over G55/Sannergata (blocked on corpus/edkjo decision) — stands up the geometry-QTO adapter (#58 W11) and measures the W6 over-report + confirms #114's void cause. Runs on edkjo (32 GB), not Omarchy.
- **W6 polygonal-bounded-halfspace over-report** (`halfspace_clip.rs`, #58) — the real remaining QTO defect; sequence after the sweep prioritizes it.
- Tag **v0.5.0** on oracle-clean; release notes explain the correctness pivot.

## Notes
- **The oracle works on day one** — its psets adapter immediately surfaced a real divergence (IfcBoolean → 'True' string), which on inspection was ifcfast's documented `value`/`value_type` contract, so the adapter gained value_type coercion (no spurious bug filed).
- Pre-existing local test failure `test_mcp_server::test_mcp_server_open_ifc_roundtrip` — the `ifcfast-mcp` entry point resolves to `~/miniconda3/bin` (conda PATH shadowing) where ifcfast isn't installed. Env-specific, not a repo bug; CI unaffected.
- Tracker: #114 corrected, #116 + #117 filed, #56 (recommend close — W4 fixed it) + #86 (MCP tools already shipped in PR #85) commented.
