# #50 first-class federation shipped (cache schema v29)

## Agent signature
- **Agent**: `claude-fable-5`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `256f79b` → `b1112ca` (1 commit this session)
- **Session scope**: implement GH #50 first-class federation per the design comment on the tracker (federate() + clash list sugar + source_model column), oracle-gated.
- **Touched paths**: crates/core/src/bundle/parquet_sink.rs, crates/core/src/bin/bundle.rs, crates/core/src/clash/{engine,sink,source}.rs, crates/core/src/lib.rs, crates/core/tests/{bundle,clash}_integration.rs, python/ifcfast/{__init__,clash,federate,header}.py, tests/test_federate_parity.py, tests/oracle/{federate,clash_sweep}.py, AGENTS.md, CHANGELOG.md
- **Parallel sessions observed**: none (`git log origin/main --since="2026-07-07 10:30"` shows only this session's commit)
- **Supersedes / superseded by**: continues 2026-07-07-10-19_143-band-probe-speedup.md

## Summary
GH #50 shipped in `b1112ca` and auto-closed. `ifcfast.federate(bundles, out_dir, *, on_collision, reference_only)` is the oracle hand-merge promoted to product (oracle module stays frozen as the differential spec); `clash([a,b,…])` federates into a content-keyed cache dir (`cache_root()/federated/<key>`, atomic-rename publish) and runs the single-dir engine; `instances.parquet` gains `source_model` (Utf8 non-null, IFC stem at bundle time, re-stamped to constituent dir name by federate) → cache schema 29; `clashes.parquet` + DataFrame gain `source_model_a/b`; `ClashOptions.reference_only` drops both-sides-reference pairs engine-side. AGENTS.md carries the new Federation section + `(guid, source_model)` join-key rule.

## Evidence (numbers)
- Gate: cargo 21 binaries 0 failures; full corpus pytest **283 passed / 3 pre-existing skips** (6m22s, release .so); parity suite 18/18; TMK13 sweep on fresh v29 bundles: **13/13 pair recall, 47/49 topics, 92 064 pairs — count-identical to baseline**, exit 0 "no regression". Base clash 26.6 s + supplemental 0.26 s on this tree.
- Parity is BITWISE (IPC-stream bytes): `pa.Table.equals` is NaN-hostile — a NaN-bearing column (`volume_prism_bound_m3`, NaN on closed rows) does not equal itself (verified: `c.equals(c)` → False, bit pattern 0x7fc00000 both sides). Any future NaN-payload drift fails the gate loudly.
- Fixture facts for the parity tests: `geom_box.ifc`/`hotswap_body.ifc`/`minimal.ifc` are metre-scale (`unit_scale=1`); `hotswap_roundtrip.ifc` is mm (`0.001`) — it feeds the mixed-unit failure test. Same-fixture-twice federation gives guaranteed guid collisions + cross-model hard clashes.
- Opus diff review: schema/batch lockstep, warn-path oracle parity, cache keying, engine filter all CONFIRMED clean; 1 should-fix (unvalidated `reference_only` in clash() = silent filter no-op → fixed: substrate-level name validation + bare-string TypeError) + 2 nits fixed (single-element list swallowed `on_collision`; non-atomic cache publish → temp dir + `os.replace`, loser adopts winner's merge).
- Harness hardening: `ensure_bundle` cache-hit now also requires the `source_model` column (version check alone misses same-version schema changes on a dev tree — exactly this session's case; stale v28 sweep caches were gio-trashed).

## Gotchas recorded
- A stray editable install (`~/workspace/sidehustles/sprucelab/cli/tests`) shadows top-level `tests` in this venv — `tests.oracle` imports need the try/except fallback used in test_federate_parity.py.
- `on_collision` affects the federation cache key only for `dedup` (warn/fail produce identical tables, keyed together); `fail` re-raises from the cached sidecar's recorded collisions.
- `clash()` list sugar writes `clashes.parquet` into the federation cache dir; `df.attrs["federated_dir"]` + `["federation"]` expose it.

## Next
1. **Truth expansion** (#141): ACC downloads per census groups — RIV+RIE@2026-03-09 unlocks 2 rounds; 03-18+03-23 unlocks 2 more; Alle_saker needs the Sept-2025 set (39 clean pairs). Sweeps cost ~27 s now; `clash([a,b])` + `source_model_a/b` can replace the sweep's sidecar cross-split when convenient (oracle module itself stays frozen).
2. **#143 deferred items** (open): Qbvh broad phase (streaming scale, #67 territory) + both-sides `include_classes` mode.
3. **Ed**: full Solibri checking export (Excel, per-rule results) → precision reconciliation + real rule tolerances (settles the 20.7 mm `c971ef58` case).
4. Release bundling: v29 + federate() are unreleased on main; bundle with the next release per the bundle-releases convention.
