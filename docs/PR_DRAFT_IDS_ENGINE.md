# Native Rust IDS 1.0 validation pipeline

## Summary

Adds a native **IDS 1.0** validation path on top of the existing tier-1 IFC index: compile IDS XML to a JSON IR, scan the model once (or reuse an open index), and validate without routing the hot path through pandas or IfcOpenShell. The design goal is **parity with IfcTester** on supported facets while improving cold and warm validation latency on large production models.

## Motivation

IDS checking on multi‑hundred‑megabyte architectural and MEP models is dominated by repeated IFC opens and full schema walks when using reference tooling. Agents and batch pipelines need:

1. **First validated result** as fast as possible after opening a model.
2. **Repeat validation** against other rulesets without re-reading the STEP file.
3. **Deterministic alignment** with the buildingSMART IDS reference behaviour (IfcTester), not a parallel interpretation.

## Approach

- **Compiled IDS IR** — shared between Python and Rust; restrictions and cardinality match IfcTester compilation.
- **Single STEP scan** — tier-1 index and optional entity byte-table built in one lexer pass when both are required.
- **Validation plan** — derived from the compiled ruleset: skip entity table, property extractors, and full object pool when facets allow (entity + attribute on products only).
- **Tier-1 fast path** — validate on indexed product columns (`Name`, entity type, etc.) with automatic fallback to the full pipeline when property, material, classification, or PartOf facets are present.
- **Session API** — prepare once per model; validate many rulesets; integrate with `open()` so the index built for exploration is reused for IDS.

Supported facet families (with documented limits): Entity, Attribute, Property, Classification, Material, PartOf (supported spatial relations). Non-product applicability and exotic PartOf relations still require IfcTester fallback via `engine=auto`.

## buildingSMART conformance (parity target)

Validation is measured against **IfcTester** on the official **IDS 1.0 Implementers TestCases** corpus (291 rulesets paired with their reference IFC models).

| Metric | Result |
|--------|--------|
| TestCases executed | 291 |
| **Status parity** (pass/fail vs IfcTester per case) | **0 mismatches** |
| Outcome vs filename expectation | 19 mismatches (pre-existing; same class of drift as reference engine tuning) |

Parity means: for each official testcase, the native engine reports the same specification status as IfcTester, not merely similar pass rates on custom models.

Optional applicability rules: specs that target non-product entities disable the tier-1 fast path so predefined-type and inheritance semantics stay aligned with IfcTester.

## Throughput benchmarks (release build)

Compared head-to-head with **IfcTester** (IfcOpenShell open + validate per run). Ruleset: production-style pack (walls, doors, windows — required `Name` on each type). Native extension built with **release** optimisations; debug builds are ~10× slower on scan and are not representative.

### Large models — cold validate (open + scan + check)

| Model size | Indexed products (approx.) | IfcTester | Native cold | Speedup |
|------------|---------------------------|-----------|---------------|---------|
| ~66 MB | low thousands | ~2.2 s | **~0.08 s** | **~28×** |
| ~144 MB | **~7,100** | ~4.7 s | **~0.17 s** | **~28×** |
| ~241 MB | tens of thousands | ~8.7 s | **~0.26 s** | **~34×** |

Scan (index only) on the ~144 MB model: **~130–150 ms**; validation step **~3–4 ms** for the three-spec ruleset.

### Agent workflow (open model + first validation)

After a single tier-1 open, the first native validation reuses the in-memory index (no second full scan):

| Model size | Cold native | Open + 1st validate |
|------------|-------------|---------------------|
| ~66 MB | ~0.07 s | ~0.21 s |
| ~144 MB | ~0.17 s | ~0.25 s |
| ~241 MB | ~0.26 s | ~0.34 s |

Second validation on the same prepared session: **~3–30 ms** (ruleset-dependent).

### Prolonged run (stress)

5 full matrix repetitions × 3 large models × 16 rulesets (1 production pack + 15 official attribute pass cases from the TestCases tree):

| Engine | Total time (240 trials) | Median per trial |
|--------|----------------------|------------------|
| IfcTester | ~1,682 s | ~5.3 s |
| Native cold | ~329 s | ~0.71 s |
| Native warm (prepare + validate) | ~317 s | ~0.67 s |
| Native repeat validate | ~159 s | ~0.37 s |

**Blended speedup ~5×** across all rulesets. Production entity+attribute pack on large models remains **~27–39×**; attribute cases that force full attribute materialisation on large files are **~3×** (expected — tier-1 fast path intentionally disabled).

## What we are not claiming

- Full IDS 1.0 facet coverage in native code — Property `dataType`, nested PartOf, and non-product specs may still need IfcTester.
- Outcome agreement with testcase filename hints in all 291 cases (19 outcome drifts remain; status parity with IfcTester is the bar).
- Sub-second cold scan in **debug** builds; release is required for production timings above.

## Test plan

- [ ] `cargo test --release -p ifcfast-core`
- [ ] `pytest -q` (all platforms in CI)
- [ ] `python scripts/run_buildingsmart_ids_conformance.py --engine rust` with `IDS_TESTCASES_ROOT` pointing at a clone of the official TestCases repository
- [ ] Spot-check large-model bench pack (3 entity specs) on a ~144 MB architectural model — expect ~150 ms scan, ~30 ms total cold native validate (release)

## Follow-up

- Rebase onto current upstream `main` (IDS module may have moved; this branch carries the full pipeline).
- Narrow outcome mismatches on the 291 TestCases where IfcTester and filename disagree.
- Extend tier-1 fast path guards where parity tests allow without reintroducing predefined-type drift.
