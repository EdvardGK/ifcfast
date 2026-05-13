# Session: ifcfast standalone-repo extraction from ifc-workbench

## Summary

Pulled the fast IFC parser out of the `ifc-workbench` scratch repo and
gave it its own home at `/home/edkjo/dev/projects/ifcfast/`. MIT
licensed, version 0.1.0, fresh git root. The build is verified and the
test suite passes; end-to-end smoke against a 22 MB Archicad IFC
reproduces the audited drift numbers from the ifc-workbench history.

Repo state is committable but not yet pushed — `git init` is the next
step, then the user decides on `EdvardGK/ifcfast` push timing.

## What moved

| `ifc-workbench`                                  | `ifcfast`                            |
|---|---|
| `crates/ifcfast/` (Rust)                         | `crates/core/`                       |
| Rust lib `_ifcfast`                              | `_core` (imported as `ifcfast._core`) |
| Python pkg `ifc_workbench.fastparse`             | `ifcfast`                            |
| `quantities/qto.py`                              | `ifcfast.classify`                   |
| `fastparse/cache.py` + `fastparse/cache_data.py` | unified `ifcfast.cache`              |
| `fastparse/index.py` (`IndexedModel`, `open_fast`) | `ifcfast.model.Model`, `ifcfast.open()` |
| `IFC_WORKBENCH_CACHE` env var                    | `IFCFAST_CACHE`                      |
| `~/.cache/ifc-workbench/`                        | `~/.cache/ifcfast/`                  |
| `data/projects/lbk-building-c.yaml`              | `examples/projects/lbk-building-c.yaml` |

Everything inside the Rust crate (lexer, indexer, entity_table, the four
extractors, the mesh pipeline, both bins) is verbatim — only path
prefixes (`crate::pset::` → `crate::extractors::psets::`) and the lib
name changed. The audit-confirmed parity numbers carry over unchanged.

## What was left behind

- `extents.py` / `extents_variants.py` (bbox-variant runner) —
  experiments that didn't ship to the public API.
- `scripts/validate_against_ito.py` — Solibri-ITO-specific glue, 523
  lines. Could be reintroduced as a separate tool once the use case is
  clearer.
- `apps/glb-viewer/` — deprioritised (sprucelab uses ThatOpen Fragments).
- `dashboard/`, `fastparse/server.py`, `fastparse/catalog.py` — internal
  to ifc-workbench, not parser concerns.
- Issue #6 (Solibri ITO validation pipeline) stayed in ifc-workbench as
  an open ticket — it's about a validation harness, not the parser.

## What was added

- `docs/history/origin.md` — the trail back to ifc-workbench and the
  rename table.
- `docs/history/audit/` — Issues #1, #2, #4, #5, #7, #8, #9 from
  `EdvardGK/ifc-workbench` ported as markdown, with full bodies and
  comments preserved. Index at `docs/history/audit/README.md`.
- `python/ifcfast/__init__.py` — public API (`ifcfast.open`,
  `Model`, `header`, `cache`, `classify`).
- `python/ifcfast/cli.py` — `ifcfast` entry point with `index`,
  `extract`, `drift`, `cache` subcommands.
- `README.md`, `LICENSE` (MIT), `.gitignore`.

## Build verification

- `cargo build --release` (lib + both bins, both feature sets) — clean.
- `maturin develop --release` — builds and installs the editable wheel.
- `pytest tests/` — 19/19 pass.
- `ifcfast.open('Sannergata_bygg_ARK_E.ifc')`:
  - cold parse: 113 ms (8,873 products, 13 storeys, IFC2X3 Revit)
  - hot reload: 24 ms
  - data layers: 24,601 psets, 0 quantities, 13,635 materials,
    1,145 classifications, 8,653 drift rows
  - drift severity: 8,042 ok / 347 warn / 264 error — reproduces the
    audited 264-error count from the ifc-workbench drift analyser run.

## Older worklogs

The four ifc-workbench worklogs that documented the parser's evolution
were moved into this repo:

- `2026-04-20-03-11_QTO-engine-migration-from-ifc-toolkit.md` — origin
  of the QTO classifier that became `ifcfast.classify`.
- `2026-05-10-14-30_Fast-parser-v2-ownership.md` — initial fastparse
  ownership + tier-based design.
- `2026-05-11-08-50_Bbox-investigation-and-handover.md` — bbox accuracy
  vs Solibri ITO, multi-semantic framing decision.
- `2026-05-11-16-30_Fastparse-v3-native-Rust-tier1-bbox-v7-federated-floors-validation-arc.md` —
  the 20-27× Rust tier-1, full ITO validation arc.

The `2026-03-03-17-41_Clash-zone-integration-and-spruceforge-push.md`
worklog stayed in `ifc-workbench` — it's about the clash-zone tooling,
not the parser.

## Next

- `git init` + first commit on `EdvardGK/ifcfast` when ready.
- Decide on PyPI publishing strategy (wheels per platform via maturin in CI).
- Port `validate_against_ito.py` only if/when there's a real consumer.
