# Packaging an upstream PR (ifcfast)

Personal fork: **[jonatanjacobsson/ifcfast](https://github.com/jonatanjacobsson/ifcfast)** (`origin`).
Upstream: **[EdvardGK/ifcfast](https://github.com/EdvardGK/ifcfast)** (`upstream`).

This tree mixes **upstream-worthy** Rust/Python IDS work with **local bench** tooling under
`scripts/bench/`. Use branch `ids-engine` on your fork; open PRs from there to `upstream/main`.

## Branch layout

| Branch | Purpose |
|--------|---------|
| `feature/ids-rust-engine` (or similar) | PR to upstream — product code + tests + docs |
| `nobel/bench` (optional) | `scripts/bench/`, local reports, prolonged runs |
| `main` (fork) | Nobel superset: merge PR branch + graph ingest + bench |

## Include in the upstream PR

**Rust (`crates/core`)**

- `src/ids/*` — compiled IR, facets, validation, schemas, `tier1_validate`, `validation_plan`
- `src/ids_session.rs`, `src/scan.rs`, `src/object_guid.rs`
- `src/indexer.rs` — `IndexProfile`, single-pass / tier-1 scan
- `src/entity_table.rs` — `TableBuilder` shared scan
- `src/lib.rs` — PyO3 `IdsSession`, `open_ifc_index`, `validate_ids_native`
- `src/bin/ids_bench.rs` — align with `scan_ifc` API if changed
- `tests/ids_parity.rs`

**Python**

- `python/ifcfast/ids/*` — engine, compile, facets, support
- `python/ifcfast/model.py` — `_ifc_native` / `prepare_ids_session` reuse

**Tests**

- `tests/ids/*` — conformance, goldens, partof, rust engine
- `tests/__init__.py`, `tests/ids/__init__.py` if needed for discovery

**Scripts (upstream)**

- `scripts/run_buildingsmart_ids_conformance.py`
- `scripts/export_ids_goldens.py`
- `scripts/gen_entity_schema.py`, `scripts/gen_attribute_schema.py`
- `scripts/fixtures/bench_large_models.ids` — small bench IDS for docs/CI

**Docs**

- `docs/IDS_VALIDATION.md`, `docs/IDS_SUPPORT_MATRIX.md`, roadmap/compliance as updated

## Exclude from upstream PR (stay on fork / `nobel/bench`)

- `scripts/bench/*` — IfcTester comparisons, prolonged runs, Windows install helper
- `reports/` — JSON bench output
- `debug-*.log` — local debug sessions
- Hard-coded machine paths (use env vars in bench scripts)
- Nobel-only: `ifcfast.graph_layers`, StreamBIM paths in other repos

## Pre-PR checklist

1. **Release build passes CI-shaped tests**
   ```powershell
   maturin develop --release
   pytest -q
   cargo test --release -p ifcfast-core
   ```
2. **Conformance** (optional locally; needs `IDS_TESTCASES_ROOT`)
   ```powershell
   python scripts/run_buildingsmart_ids_conformance.py --engine rust
   ```
3. **Squash / reorder commits** — prefer logical commits: scan unification → validation plan → tier-1 → PyO3 session → Python engine → tests/docs.
4. **No secrets, no large IFCs** — `.gitignore` already excludes `*.ifc` outside fixtures.
5. **Rebase onto upstream `main`** before opening PR.

## Creating the PR branch (example)

From a clean working tree with all product changes staged:

```powershell
git fetch origin
git checkout -b feature/ids-rust-engine origin/main
# cherry-pick or stage only upstream paths (see lists above)
git add crates/core python/ifcfast tests/ids scripts/run_buildingsmart_ids_conformance.py `
        scripts/fixtures docs/IDS_VALIDATION.md docs/IDS_SUPPORT_MATRIX.md
git status   # confirm scripts/bench/ and reports/ are NOT staged
git commit -m "Add native Rust IDS validation pipeline with tier-1 fast path"
git push -u origin feature/ids-rust-engine
```

Bench-only work:

```powershell
git checkout -b nobel/bench main
git add scripts/bench/
git commit -m "Add local IDS bench scripts (fork only)"
```

## PR description template

**Title:** Native Rust IDS validation (compiled IR, tier-1 fast path, session API)

**Summary**

- Single STEP scan for index + optional entity table (`scan_ifc`)
- `ValidationPlan` skips entity table / full object pool when IDS facets allow
- Tier-1 validate on product columns; full pipeline fallback for property/material/partof
- PyO3: `prepare_ids_session`, `validate_ids_native`, `open_ifc_index` for reuse after `open()`
- buildingSMART conformance harness unchanged; parity tracked in `tests/ids`

**Test plan**

- [ ] `cargo test --release`
- [ ] `pytest -q` on Linux/macOS/Windows
- [ ] `python scripts/run_buildingsmart_ids_conformance.py --engine rust` (maintainer with TestCases checkout)
