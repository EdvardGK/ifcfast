# Origin

`ifcfast` started life as an internal sub-package inside a personal scratch
repo, `EdvardGK/ifc-workbench`, under `crates/ifcfast/` (Rust) and
`ifc_workbench/fastparse/` (Python). It was extracted as a standalone
project on **2026-05-13** at version `0.1.0`. This document records the
context so future contributors can follow the trail back when needed.

## Why extract

`ifc-workbench` had accumulated multiple half-finished experiments
(bbox variants, viewer prototypes, ITO/Solibri-specific scripts) and the
Python layer was structured around that history. The parser was the only
piece that had real, audited value, and it had no runtime dependency on
the rest of the repo. Carrying its name and structure through the wider
workbench layout would have meant explaining historical artefacts every
time someone new looked at the code, so it was given its own home with
a clean tree.

## What changed in the extraction

The internal logic was carried over verbatim — every Rust source file,
every extractor, every test was either copied as-is or renamed to fit
the new layout. The user-facing surface area is what was reshaped:

| in `ifc-workbench`                              | in `ifcfast`                       |
|---|---|
| `crates/ifcfast/`                               | `crates/core/`                     |
| Rust lib name `_ifcfast`                        | `_core` (imported as `ifcfast._core`) |
| Python package `ifc_workbench.fastparse`        | `ifcfast`                          |
| `qto.py` (entity-mode policy)                   | `ifcfast.classify`                 |
| `cache.py` + `cache_data.py` (two cache layers) | `ifcfast.cache` (unified)          |
| `index.py` / `IndexedModel`                     | `ifcfast.model` / `Model`          |
| `IFC_WORKBENCH_CACHE` env var                   | `IFCFAST_CACHE`                    |
| `~/.cache/ifc-workbench/`                       | `~/.cache/ifcfast/`                |
| Public entry point `open_fast(path)`            | `ifcfast.open(path)`               |
| Bin names `ifcfast-bench`, `ifcfast-mesh`       | unchanged                          |

The bbox variant runners (`extents_variants`, `extents`) and the
Solibri-ITO validator script were left behind — they were experiments
and project-specific glue, not core library functionality. They may
be reintroduced once the use case is clearer.

The federated-floor synthesis module was already project-agnostic in
`ifc-workbench` (see Issue #7), so it ported directly as
`ifcfast.federated_floors` with no logic changes.

## Provenance of the audit issues

The eight GitHub issues that drove the parser's audits and bug fixes
have been carried over as documentation in
[`docs/history/audit/`](audit/). They are the historical record of how
the parser was validated — the closing comments contain the parity
numbers against ifcopenshell on real production files. Read them when
you want to understand a design decision; they remain the best source
of truth on why specific entities, properties, or encodings are handled
the way they are.

The original issues live at `https://github.com/EdvardGK/ifc-workbench/issues/{1,2,4,5,7,8,9}`
(Issue #6 stayed in `ifc-workbench` as an open Solibri-ITO pipeline
ticket rather than a parser concern).

## Pre-extraction git history

The pre-extraction commit history lives on the `fastparse-v4-fragments-emitter`
branch of `EdvardGK/ifc-workbench`. The `ifcfast` repo starts from a
clean root commit; do not expect a continuous git history across the
boundary.
