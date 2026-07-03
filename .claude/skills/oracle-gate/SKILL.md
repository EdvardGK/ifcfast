---
name: oracle-gate
description: Run the ifcfast ship gate for geometry/QTO changes — corpus differential vs ifcopenshell with per-class baselines, plus the test suites. Use before committing any change that can move mesh or QTO output (mesh/, qto, profile, placement, cut_openings, boolean paths).
---

# oracle-gate — the ifcfast geometry ship gate

Validates a geometry-affecting change against the real-model oracle
before it ships. Corpus differential beats synthetic tests here — see
the `feedback-corpus-differential-over-synthetic` project memory (two
episodes where synthetic gates passed a real regression).

## Preconditions

- venv active: `source .venv/bin/activate` (has ifcfast editable,
  ifcopenshell, pandas, numpy).
- Corpus present: `scratch/g55/G55_{ARK,RIB,RIE,RIV}.ifc`
  (`IFCFAST_CORPUS` uses ABSOLUTE paths — relative fails from crate cwd).
- Builds are serialized on this machine (16 GB — concurrent cargo or a
  parallel maturin OOMs). Run builds at the coordinator, one at a time.
- maturin gotcha: `env -u CONDA_PREFIX maturin develop` (fails when both
  VIRTUAL_ENV and CONDA_PREFIX are set).

## Steps (fan out 3–5 as parallel agents once the .so is built)

1. **Rust suite** (coordinator, serialized):
   `cargo test -p ifcfast-core` — plus the corpus-gated doc tests when
   the write axis is touched:
   `cargo test -p ifcfast-core --no-default-features --test doc_roundtrip --test doc_subset --test doc_rel_rules -- --include-ignored`
2. **Rebuild the dev .so** (coordinator, serialized):
   `env -u CONDA_PREFIX maturin develop`
3. **Python suite** (agent, sonnet):
   `python -m pytest tests/ -q` with `IFCFAST_CORPUS` set (~16 min full).
4. **Corpus sweep** (agent): for each affected model, minimum ARK + RIB:
   ```
   python -m tests.oracle.class_sweep scratch/g55/G55_ARK.ifc \
       --cache-dir scratch/g55/cache \
       --baseline scratch/g55/baselines/G55_ARK.json
   ```
   Exit 1 = a class drifted past tolerance. Cache files under
   `scratch/g55/cache/` are keyed by model name only — DELETE the cache
   for a model before sweeping a new build, or you'll diff stale data.
5. **Code review** (agent): review the diff with focus on winding/frame
   conventions, Polygon2D producers that bypass `profile::extract`, and
   cache-schema implications.

## Interpreting drift

- Drift toward ratio 1.0 = improvement. Update the baseline
  (`--write-baseline scratch/g55/baselines/<MODEL>.json`) and say so in
  the commit message.
- Drift away from 1.0 = investigate per-element before concluding: A/B
  against a pre-change build (`git stash` → rebuild .so → sweep with a
  separate cache dir → `git stash pop` → rebuild). Attribute every moved
  element to your change or a pre-existing bug; file pre-existing ones
  as issues (signed, per CLAUDE.md).
- Any QTO column value change ⇒ bump `_CACHE_SCHEMA_VERSION` in
  `python/ifcfast/header.py` with a changelog comment AND update
  AGENTS.md (project convention).

## Baselines

`scratch/g55/baselines/*.json` are LOCAL ONLY (client data, scratch/ is
gitignored). They record per-class {n, fast_sum, ios_sum, ratio} as of
the last validated state. Regenerate only after a sweep whose deltas are
fully attributed. If baselines are missing, recreate via
`--write-baseline` from a known-good commit, or accept a baseline-less
sweep and compare per-class ratios to the tables in the latest QTO
worklog / GH issue comments.
