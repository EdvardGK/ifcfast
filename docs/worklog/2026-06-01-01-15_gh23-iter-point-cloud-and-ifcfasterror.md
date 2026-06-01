## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `cfebf02` → `30e078d` (1 commit made this session)
- **Session scope**: Shipped streaming `m.iter_point_cloud(...)` + `ifcfast.IfcfastError` for GH #23 (OOM + uncatchable Rust panic on large ARK point clouds), bumped to v0.4.20.
- **Touched paths**: `crates/core/src/lib.rs`, `python/ifcfast/__init__.py`, `python/ifcfast/model.py`, `tests/test_smoke.py`, `AGENTS.md`, `Cargo.toml`, `crates/core/Cargo.toml`, `pyproject.toml`.
- **Parallel sessions observed**: none on `origin/main` during this window. External PR #24 (Native Rust IDS pipeline by `jonatanjacobsson`) remains open and untouched.
- **Supersedes / superseded by**: none

# Session: GH #23 — streaming point cloud + IfcfastError

## Summary

Picked up from the previous session's `next-steps.md` priority 1: GH #23 (`point_cloud()` OOMs on 200 MB–1 GB ARK IFCs, three failure modes including the uncatchable `pyo3_runtime.PanicException`). Locked the public-API design with the user before coding (`AskUserQuestion`): generator yielding DataFrames, fixed `chunk_points` boundary, panic fix shipped together. Then implemented:

1. A `catch_panic` helper at the PyO3 boundary plus a `pyo3::create_exception!`-defined `IfcfastError` exception, re-exported through `ifcfast.IfcfastError`. Wrapped the five geometry-pipeline entry points (`sample_point_cloud`, `iter_point_cloud`, `extract_meshes`, `mesh_qto`, `analyse_drift`). The remaining data-extractor entry points are filed as follow-up GH #27.
2. A `PointCloudIter` pyclass (`#[pyclass]` + `__iter__` / `__next__`) backed by a `std::sync::mpsc::sync_channel(2)`. A worker thread runs `mesh_ifc_streaming_framed`; a `StreamingCloudSink` flushes every `chunk_points` rows over the channel. `__next__` moves the receiver out of `Option`, releases the GIL for `recv()`, then puts the receiver back (Receiver is `Send` but not `Sync`, so the borrow-capture pattern doesn't satisfy pyo3's `Ungil` bound — taking ownership round-trip does). Worker panics are caught and shipped back as an `Err(String)` over the channel, then raised as `IfcfastError` on the next `__next__`.
3. `m.iter_point_cloud(per_m2, seed, unit, chunk_points)` in `python/ifcfast/model.py`, scaling each yielded chunk's xyz + global_shift by the user's unit factor.
4. 7 new pytest tests covering chunk-sum-equals-single-shot, global_shift stability across chunks, unit-factor per chunk, determinism, `chunk_points=0 → IfcfastError`, public `IfcfastError` re-export, and early-drop liveness.
5. AGENTS.md updates: new "Streaming point cloud" section, IfcfastError convention entry, `iter_point_cloud` row in the decision-tree table.

Bumped version to v0.4.20 across `Cargo.toml` workspace + `crates/core/Cargo.toml` + `pyproject.toml` + `python/ifcfast/__init__.py`. No cache schema change (vertex / instance shapes are unchanged).

Posted resolution comment on GH #23, filed follow-up GH #27 for wrapping the remaining `_core` pyfunctions. Tag push to `v0.4.20` is held pending user confirmation (and the open decision in GH #22 about flipping `csg` into default features in this release or the next).

## Changes

- **`crates/core/src/lib.rs`** (~+425 lines): added panic-helper module (`IfcfastError` exception, `panic_payload_to_string`, `catch_panic`). Wrapped five geometry pyfunctions in `catch_panic(|| { ... })`. Added `CloudChunk` payload type, `StreamingCloudSink` (impls `ProductSink`, honours `Arc<AtomicBool>` stop flag for early-drop short-circuit), `PointCloudIter` pyclass with `__iter__`/`__next__` releasing the GIL via the take-and-restore pattern, and the `iter_point_cloud` pyfunction that spawns the worker thread (catching panics with `catch_unwind(AssertUnwindSafe(_))` and surfacing the message over the channel). Registered `IfcfastError` and `PointCloudIter` in the `_core` pymodule.
- **`python/ifcfast/__init__.py`**: re-exported `IfcfastError` from `._core` (with a Python-defined fallback for source-only imports), added to `__all__`. Bumped `__version__` to 0.4.20.
- **`python/ifcfast/model.py`**: added `Model.iter_point_cloud(per_m2, seed, unit, chunk_points)` — generator-style wrapper around `_core.iter_point_cloud`, scaling xyz + global_shift per chunk.
- **`tests/test_smoke.py`** (+119 lines): 7 new tests at the end of the file, all against the `_MM_CUBE` fixture.
- **`AGENTS.md`**: added "Recoverable Rust failures raise `ifcfast.IfcfastError`" to conventions, new "Streaming point cloud" section after the conventions list, `iter_point_cloud` row in the decision-tree table.
- **GH issue comments**: posted resolution on #23; opened follow-up #27 (panic-safety sweep for remaining `_core` pyfunctions).
- **Version bump**: 0.4.19 → 0.4.20 across `Cargo.toml`, `crates/core/Cargo.toml`, `pyproject.toml`, `__init__.py`.

## Technical Details

**The Send-but-not-Sync `Receiver` problem in `__next__`.** First implementation captured the receiver by reference (`let rx = self.rx.as_ref()?; py.allow_threads(|| rx.recv())`). That fails to compile: `mpsc::Receiver<T>` is `Send` but explicitly *not* `Sync`, and pyo3's `allow_threads` bound is `F: Ungil + FnOnce()`, which transitively requires every captured reference to be `Send` (which would need the pointee to be `Sync`). Three options:

1. Wrap in `Arc<Mutex<Receiver>>` — adds a lock per recv and is allocator overhead.
2. Use `Arc<Receiver>` — doesn't help, `Arc<T>: Send` requires `T: Sync`.
3. Move the receiver into the closure, return it back through a tuple.

(3) is the cheapest: `Self.rx: Option<Receiver>` already carries the niche for the take, the closure consumes the receiver, recv returns and the closure returns `(rx, result)`, then the caller puts the rx back. No locking, no allocation, no API change. Documented inline.

**Panic surfacing across the worker thread.** The thread closure is wrapped in `catch_unwind(AssertUnwindSafe(_))` exactly the way the helper does for sync calls, but the payload can't return through `?` — the worker is on its own thread. Instead, on panic the worker builds a message string from the payload (via the same `panic_payload_to_string` helper used by `catch_panic`) and sends it as `Err(String)` over the channel. `__next__` matches on `Ok(Err(msg))` and raises `IfcfastError` with the message. The receiver is `take`d and `worker` dropped so subsequent `__next__` calls return `None` — same StopIteration behaviour as a normal drain.

**Early-drop liveness via `Arc<AtomicBool>`.** Without a stop signal, dropping the iterator would let the worker run the entire mesh pass (potentially minutes on a 1 GB ARK) just to discard the output once the receiver is gone. The atomic flag is checked at the top of `on_product` and after every flush; flipped both by `Drop for PointCloudIter` and by the sink itself when `tx.send` fails (consumer dropped during a flush). The mesh-pass loop continues iterating product entries — the sink just becomes a no-op — but the per-product tessellation work is skipped, which is where the wall time lives. Verified by `test_iter_point_cloud_early_drop_does_not_hang` (read one chunk, drop the iterator, start a second iter and confirm it completes).

**Bit-identical determinism with the single-shot API.** The sink uses the same per-product splitmix64 seed derivation (`seed ^ ifc_id * GOLDEN_RATIO`, three multiplies and xorshifts), the same `sample_mesh` function, the same `BakeFrame::Local + global_shift_for + unit_scale` repositioning math. The only mechanical difference is *when* points get materialised (per-chunk flushes vs single buffer at end); the actual sample coordinates and normals are identical. `test_iter_point_cloud_chunks_sum_matches_single_shot` verifies sum-of-len equality; tighter byte-level equality across the streaming/batch surfaces was tested ad-hoc but not codified since the row ordering between the two iteration paths is allowed to differ (substrate-style: iteration order over the IFC entity table is preserved on both, but a streaming flush mid-product gives a different *intra-product* ordering than batch).

**Why a generator pyclass and not a callback API.** The user explicitly preferred the generator surface (vote captured in this session's `AskUserQuestion`). Composes with the standard library (`itertools`, `for ... in ...`, `list()`), supports `chunk.to_parquet(...)` directly without a sink callback, and pyo3's `#[pyclass]` with `__iter__`/`__next__` is the canonical way to expose a Rust iterator to Python — no need for a state-machine reinvention.

**Why catch_panic was applied to five entry points and not all of them.** GH #23's three failure modes all surfaced in `point_cloud`. The other geometry calls (`meshes`, `mesh_qto`, `analyse_drift`) traverse the same mesh pass and could panic identically, so wrapping them is principled (and the test suite proves they keep working). The lighter data extractors haven't been observed to panic in practice — wrapping them is hygiene, not a fire. Filed as follow-up GH #27 to keep this PR focused on the user's actual reported issue.

**Workflow tests passing**: `cargo test -p ifcfast-core --release --test mesh_reveal` (13/13) and `--test cut_openings_integration` (7/7) confirm the geometry pipeline didn't regress under the catch_panic wrapping. `pytest tests/` (76/76) confirms the Python surface — new iter_point_cloud tests + all existing `point_cloud` / `meshes` / `mesh_qto` regression tests pass.

## Next

**v0.4.20 release.** Tag push held pending user confirmation. Two open decisions to fold in before tagging:

1. Whether to flip `csg` into default Cargo features in this release (so the published wheel ships `cut_openings` ready) or wait for v0.4.21. GH #22 has the open comment asking permission. The csg-smoke workflow validates `csg` builds on all 5 wheel platforms, so the release path is unblocked from the CI side regardless of which way this goes.
2. Whether to bundle GH #27 (panic-safety sweep) into v0.4.20 or hold for v0.4.21. The wrapping is mechanical (~30 minutes) but it would broaden the diff scope.

**GH #20 P0 #3 (rayon parallelization further work).** Issues #25 (bounded ordered channel — eliminates the `Vec<ProductOutcome>` collect overhead) and #26 (parallel phase 1 — sharded byte-range walks + frozen `PlacementResolver`) remain. #25 should unlock T=1 matching baseline and let T=8 push past 2×; #26 removes the Amdahl tail for the path to 4–16× on big files.

**PR #24** (external Native Rust IDS validation pipeline) still open, still untouched per user's reservation.

## Notes

- The `AskUserQuestion` lock at the start of this session ("Generator + chunk_points + ship-together") matched the recommendations in the previous session's `next-steps.md`; the design-three-question pattern there was load-bearing for keeping API-shape decisions explicit before code went into a public release.
- The `pyo3::create_exception!` macro emits a `gil-refs` cfg lint warning under pyo3 0.22.x. It's upstream macro hygiene, not something I introduced — pre-existing on the wheel build per `cargo check` warnings. Filed mentally as "would clear on pyo3 0.23+ migration".
- The `IfcfastError` is exposed both via `from ifcfast import IfcfastError` (preferred) and `from ifcfast._core import IfcfastError` (raw). The `__init__.py` re-export carries a Python-defined fallback for source-only imports (e.g. type checkers reading the package without the native extension built), so `import ifcfast` doesn't fail in dev tooling contexts.
- This session continued the `— Claude from Ed's Omarchy. "iter_point_cloud + IfcfastError for #23"` signature tag across the commit, GH #23 resolution comment, and GH #27 filing.
