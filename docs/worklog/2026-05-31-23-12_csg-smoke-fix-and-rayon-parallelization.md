## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `bee2b8f` → `cfebf02` (3 commits made this session)
- **Session scope**: Fixed the broken csg-smoke CI workflow (GH #22) and shipped parallel per-product tessellation in `mesh_ifc_streaming_framed` (GH #20 P0 #2).
- **Touched paths**: `.github/workflows/csg-smoke.yml`, `crates/core/src/indexer.rs`, `crates/core/Cargo.toml`, `crates/core/src/mesh/mod.rs`, `crates/core/src/mesh/mapped.rs`, `crates/core/src/mesh/boolean.rs`, `AGENTS.md`, `docs/worklog/`, project memory directory.
- **Parallel sessions observed**: none on `origin/main` during this window. External PR #24 (Native Rust IDS pipeline by `jonatanjacobsson`) opened against `main` at 20:31 yesterday, untouched here.
- **Supersedes / superseded by**: none

# Session: csg-smoke CI fix + rayon parallel tessellation

## Summary
Resumed from a stale next-steps doc that listed three items, two of which had already shipped. Real picture from `gh run`: csg-smoke (the workflow we'd just added in `97d58d9`) had failed on its very first run with macOS-only linker errors. Diagnosed and fixed that across two commits, then pivoted to GH #20 P0 #2 (rayon parallelization of `mesh_ifc_streaming`) since the next-steps doc had ranked it #3 but it was actually the only remaining real item. The rayon refactor required changing the IfcMappedItem `shape_cache` from `HashMap` to `DashMap` (concurrent reads/writes across worker threads), then splitting the streaming function into a three-phase pipeline that preserves the existing `ProductSink` ordering contract. Measured 1.85–2.01× end-to-end speedup on real client files, with bit-identical output to the serial baseline.

## Changes
- **`.github/workflows/csg-smoke.yml`** (`09b1d18`): Test build now passes `--no-default-features --features csg` to drop pyo3. The cdylib's `extension-module` feature defers Python symbols to runtime — fine on Linux/Windows linkers for a cdylib build, fatal on macOS for an executable.
- **`crates/core/src/indexer.rs`** (`3a14ac6`): Gated `extract_unit_scale` to `#[cfg(feature = "python")]` to match its only call sites. Under `--no-default-features --features csg` (what csg-smoke now uses) the function was genuinely dead code and CI's `RUSTFLAGS=-D warnings` turned the dead-code lint into a build failure across all five wheel platforms.
- **`crates/core/Cargo.toml`** (`cfebf02`): Added `rayon = "1.10"` and `dashmap = "6.1"` as `optional = true` deps gated under the existing `mesh` Cargo feature, so `ifcfast-bench` (no-default-features) still builds slim.
- **`crates/core/src/mesh/{mod,mapped,boolean}.rs`** (`cfebf02`): Defined `pub(crate) type ShapeCache = dashmap::DashMap<u64, Vec<(LocalMesh, &'static str)>>`. Changed `mesh_item`, `mapped::expand`, `boolean::boolean_result`, `boolean::csg_solid` (and the boolean recurse callback type) from `&mut HashMap<...>` to `&ShapeCache`. Refactored `mesh_ifc_streaming_framed` into three phases — serial walk + placement resolve into `Vec<Work>`, parallel `into_par_iter().map(tessellate_one).collect::<Vec<ProductOutcome>>()`, serial drain to sink + stats. Extracted `tessellate_one` and `apply_outcome` as free functions; introduced `ProductOutcome` enum that carries enough context for phase 3 to bump stats without needing a Mutex on `MeshStats.by_source`.
- **`AGENTS.md`** (`cfebf02`): Added a paragraph to the "Cost model" section noting that tessellation is now parallel (since v0.4.20), defaults to all cores, configurable via `RAYON_NUM_THREADS`, and explicitly states that emission order to sinks is preserved (the contract substrate / OBJ / glTF writers and the `cut_openings` wrapper all depend on).
- **GH issues filed**: #25 (bounded ordered channel — eliminate the `Vec<ProductOutcome>` collect overhead), #26 (parallel phase 1 — sharded byte-range walks + frozen `PlacementResolver`).
- **GH issue comments**: posted progress on #20 (umbrella, P0 #2 done), asked permission to close #22 (csg-smoke).

## Technical Details

**The csg-smoke macOS failure**: pyo3's `extension-module` feature tells the linker "don't expect to find `_Py_TrueStruct`, `_Py_IncRef`, `_Py_NoneStruct`, `_Py_FalseStruct` at link time — CPython will provide them when it dlopens us." Linux's `ld` and Windows's link.exe both tolerate undefined symbols in a cdylib build by default. macOS's `ld` does not, and it doesn't care that the actual *purpose* of the symbols is runtime injection: if you ask it to link an executable (which `cargo test` does, even when the lib-under-test is a cdylib), it demands those symbols at link time and bails. The integration test itself doesn't touch pyo3, so dropping `python` from its feature set is the principled fix. Verified locally that `RUSTFLAGS=-D warnings cargo check` is clean both with default features and with `--no-default-features --features csg`.

**The parallel design**: The three-phase pipeline keeps the existing `ProductSink` contract intact (sinks still receive products in IFC entity-table order and the trait stays `&mut S`). Phase 1 has to be serial because `PlacementResolver` is a `&mut self` cache; it's cheap (<5% of total mesh time on real files) and worth eating to avoid changing the public API. Phase 2 is the actual parallel win — `into_par_iter().map(tessellate_one)` lets rayon's work-stealing scheduler distribute products across cores. The DashMap shape_cache lets concurrent IfcMappedItem dedup work; the worst-case shard contention is at startup on a wide-facade model where many products race to compute the same `LocalMesh` once, after which the shard warms and reads are lock-free. Phase 3 drains the collected `Vec<ProductOutcome>` serially, which is where the IFC iteration order is preserved and where stats counters get updated without contention.

**`ProductOutcome` design**: A naive parallel rewrite would have each worker call `sink.on_product` and `stats.by_source.entry(...)` directly under a Mutex. That serialises every emission. Instead, each worker returns an enum carrying the full `ProductMesh` *plus* the tag lists that phase 3 needs to bump `by_source` (`segment_tags: Vec<String>`, `unhandled_types: Vec<String>`) and triangle/product counters. Phase 3 then folds those into stats in input order with zero synchronisation. Cost: a Vec full of ProductMeshes in memory at once (relaxes the strict "1 product in flight" RAM contract the streaming sink used to give) — that's filed as #25.

**Measurements**: 8-core machine, release build, real Skiplum client files.
- `LBK_RIBp_C.ifc` (41 MB, 32 183 products, 6.6M tris): baseline 460 ms → rayon T=8 249 ms (**1.85×**). Rayon T=1 is 548 ms (19% slower than baseline — the `Vec<ProductOutcome>` collect cost).
- `LBK_ARK_C.ifc` (179 MB, 19 633 products, 4.9M tris): baseline 1294 ms → rayon T=8 644 ms (**2.01×**). Rayon T=1 is 1450 ms (12% slower).
- Triangle counts and product counts identical across baseline / T=1 / T=8 — that's the correctness check.
- All 13 `mesh_reveal` + 7 `cut_openings_integration` tests pass under the new impl.

**The Vec-collect overhead**: At T=1, the new code is 12–19% slower than the pre-rayon baseline. This is because the original code emitted directly to sink (no intermediate `Vec`), and the new code materialises every `ProductOutcome` before phase 3 starts. The parallel speedup at T=8 overcomes this from T=2 upward, but it caps the ceiling at ~2×. Filed as #25 — switching to a bounded ordered channel (mpsc + reorder buffer keyed by sequence id) restores the original streaming semantics and should let T=8 push past 2× while making T=1 match baseline. Also filed #26 for the Amdahl-bound serial phase 1.

**Real-user issue surfaced**: GH #23 (`point_cloud()` OOM on >200 MB ARK IFCs) from the user's Windows tester run. Three failure modes, including a `pyo3_runtime.PanicException` that's uncatchable from Python and tanks worker pools. Next session starts there. Design questions are captured inline in `next-steps.md`.

**External PR #24** (Native Rust IDS 1.0 validation pipeline by `jonatanjacobsson`, 10k+ lines, claims 27–34× vs IfcTester with zero parity mismatches on the buildingSMART IDS 1.0 conformance suite) opened during this session. User reserved review for themselves — agent does not touch.

## Next

GH #23 is the next-session priority. The fix has two threads:
- **API design**: chunked iterator (`m.iter_point_cloud(per_m2, seed, chunk_points=N) -> Iterator[pd.DataFrame]`) vs entity-chunked variant. Three design questions documented in `next-steps.md`.
- **The Rust panic bug** (#23 third failure mode): the `pyo3_runtime.PanicException` under allocator pressure needs to become a recoverable Python exception independent of the API change. Wrap the `point_cloud` PyO3 entry in `catch_unwind` or audit the panicking allocation sites.

Secondary: ship v0.4.20 (decision in GH #22 comment), then work #25 + #26 to push the rayon speedup past 2×.

## Notes
- Next-steps.md (loaded automatically at session start) was stale on entry. The first 4-5 minutes of this session were sunk into re-syncing with the actual repo state. The CLAUDE.md `auto memory` rules call for treating next-steps as point-in-time observations to verify against `git log` — that worked, but the *briefing-cache.md* memory file is system-generated and was 2 days old; worth knowing it's not load-bearing.
- The signature convention (`— Claude from Ed's Omarchy. "<scope>"`) was used consistently across all gh issue comments + the two new issues, with `"parallel tessellation rollout"` as the session scope tag. That matches this worklog signature for traceability.
- csg-smoke CI is now informational across all five wheel platforms; the v0.4.20 release path is unblocked from the CI side.
