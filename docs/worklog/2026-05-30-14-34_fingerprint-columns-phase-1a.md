## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `e2e09e7` → `e2e09e7` (no commits this session — edits unstaged)
- **Session scope**: Phase 1a of the clash-control feature — add per-instance geometric fingerprint columns (centroid_xyz, vertex_count, triangle_count) to the substrate.
- **Touched paths**: `crates/core/src/bundle/record.rs`, `crates/core/src/bundle/parquet_sink.rs`, `crates/core/tests/bundle_integration.rs`, `python/ifcfast/header.py`
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: none

## Summary

Long design conversation in this session converged on a phased build plan for federated-model clash control + cross-discipline duplicate detection, with the architectural stance: **engine-first, filter-after**. Canonical clash dataset is the artifact; filters (connectivity dismissal via `IfcRelConnectsPorts` / `IfcRelAggregates`, dup classification, space-anchored clustering, BCF emit) are queries on top of it. Same-system membership is metadata, not a filter input. Spaces (typically owned by ARK) are the clustering frame — stage-0 discovery returns "has spaces / who owns them" as recommendations the agent acts on, not a hardcoded pipeline step.

Phase 1a delivers the cheap half: substrate fingerprint columns that let agents do broad-phase queries (centroid distance, bbox overlap, complexity match) directly in DuckDB without re-running the parser or recomputing midpoints on every join. Phase 1b will add the actual clash engine (parry3d-backed BVH + mesh-mesh intersection) producing the canonical clash parquet.

## What changed

Three new columns on `instances.parquet`:

- `centroid_xyz` — `FixedSizeList[Float32, 3]`, world-AABB midpoint when geometry exists, falls back to `placement_xyz` for geometryless products (so location queries don't collapse no-body elements onto world origin).
- `vertex_count` — `UInt32`, world-baked mesh vertex count, zero when geometryless.
- `triangle_count` — `UInt32`, world-baked mesh triangle count, zero when geometryless.

Implementation:

- `crates/core/src/bundle/record.rs` — added three fields to `InstanceRecord`; populated in `pair_split()` from the already-computed `world_bbox` and `mesh.vertices`/`mesh.indices` lengths. Centroid fallback for geometryless triggers on `mesh.vertices.is_empty()`.
- `crates/core/src/bundle/parquet_sink.rs` — extended `build_instance_schema()` and `build_instance_batch()`: three new Field declarations, three builders, three appends, three Arc-wrapped arrays in the output vector.
- `python/ifcfast/header.py` — bumped `_CACHE_SCHEMA_VERSION` from 4 to 5 with a new history entry documenting the v0.4.19 column additions. The bump invalidates existing caches via `_compute_cache_key()` which hashes the schema version.
- `crates/core/tests/bundle_integration.rs` — new test `fingerprint_columns_carry_centroid_and_counts` locks in the contract: wall (extruded solid) has centroid inside bbox + at AABB midpoint + positive vertex/triangle counts; geometryless space falls back to placement_xyz + reports zero counts.

## Verification

- `cargo test -p ifcfast-core --features bundle` — 4 bundle_integration tests pass (new fingerprint test + 3 pre-existing), 13 mesh_reveal tests pass.
- `cargo clippy --features bundle --tests` clean on the four edited files (45 pre-existing PyO3 `useless_conversion` warnings in `lib.rs` are unrelated to this work and were present before).
- `maturin develop --release` succeeds; new wheel installed in venv.
- Bundle binary run on real-world `Duplex_A_20110907.ifc`: 289 instance rows, all 3 new columns present in the expected schema position, all 286 meshed rows have centroid inside their AABB, all 3 geometryless rows correctly fall back to `placement_xyz`.
- Python `tests/test_smoke.py` — 17 passed (no regressions in the user-facing flow).

## What this unlocks

Agents can now compose broad-phase clash candidates and duplicate detection as pure DuckDB queries against `instances.parquet`. For example, cross-model duplicate candidates collapse to a centroid-distance + bbox-overlap join:

```sql
SELECT a.guid, b.guid, ...
FROM model_a.instances a
JOIN model_b.instances b
  ON sqrt(
       (a.centroid_xyz[1]-b.centroid_xyz[1])^2 +
       (a.centroid_xyz[2]-b.centroid_xyz[2])^2 +
       (a.centroid_xyz[3]-b.centroid_xyz[3])^2
     ) < 0.05  -- 5cm
 AND abs(a.aabb_volume_m3 - b.aabb_volume_m3) / nullif(a.aabb_volume_m3, 0) < 0.05
 AND abs(a.vertex_count::int - b.vertex_count::int) < 10;
```

The narrow-phase mesh-mesh intersection still needs the clash engine — that's Phase 1b.

## Not yet done (Phase 1b candidates)

- OOBB + principal-axis fingerprint columns (deferred — these need PCA via parry3d or nalgebra; cleaner to add together with the clash engine since parry3d brings both).
- `ifcfast.clash()` primitive — parry3d-backed BVH, mesh-mesh narrow-phase, parquet of `(id_a, id_b, kind, intersection_volume, containment_ratio, min_clearance)`.
- `ifcfast.inspect_federation([files...])` discovery — preflight metadata (space owner, mesh coverage, recommendations).
- BCF emit downstream.

## Next session

Either (a) Phase 1b clash engine with parry3d, or (b) verify with real federated models that the fingerprint columns are doing what agents need (a quick DuckDB-driven duplicate-detection notebook against a federated test set) before committing to the engine. (b) is the cheaper learning, but only if there's a federated test set handy.
