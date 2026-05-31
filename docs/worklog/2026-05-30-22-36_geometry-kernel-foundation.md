## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `e2e09e7` → `e2e09e7` (no commits this session — all edits unstaged)
- **Session scope**: Set up first-class geometry kernel module (`crates/core/src/geom/`) on `parry3d` + `nalgebra`, with broad-phase AABB overlap and narrow-phase mesh-mesh intersection / distance primitives.
- **Touched paths**: `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `crates/core/src/geom/mod.rs` (new), `crates/core/src/geom/mesh.rs` (new), `crates/core/src/geom/broad_phase.rs` (new), `crates/core/src/geom/narrow_phase.rs` (new)
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: continues `2026-05-30-14-34_fingerprint-columns-phase-1a.md` (same session — both unstaged together)

## Summary

After a long design conversation, the user explicitly rejected the "ship easy 80% solution first, see if we need more" recommendation and asked for the highest-value route. The first-principles read: ifcfast has no geometry kernel — every meaningful upcoming feature (net booleans, clash detection, OOBB, mesh repair, future offsetting) wants kernel-level capability, so build it once properly rather than bolt on N feature-specific shortcuts.

This session lays the foundation: a `geom` feature-gated module on `parry3d 0.17` + `nalgebra 0.33`, with the two primitives the clash engine needs first — broad-phase (pairwise AABB overlap with tolerance) and narrow-phase (mesh-mesh `intersects` + `min_distance`). Two new feedback memories were also captured: "highest-value route" and "reversible → just go" (the user grants authority to implement reversible work directly).

## Changes

**Code (all under the new `geom` feature flag, off by default):**

- `crates/core/Cargo.toml` — new `geom` feature plus optional deps `parry3d = "0.17"`, `nalgebra = "0.33"`. Gated off the default Python build (`default = ["python"]`) — wheel-default decision deferred.
- `crates/core/src/lib.rs` — `pub mod geom` behind `#[cfg(feature = "geom")]`.
- `crates/core/src/geom/mod.rs` — module docs explaining the engine-first / filter-after stance, public API re-exports, and the queued submodules (CSG, PCA / OOBB, manifold repair).
- `crates/core/src/geom/mesh.rs` — `build_trimesh(vertices: &[f32], indices: &[u32]) -> Result<TriMesh, MeshBuildError>`. Bridges ifcfast's flat-buffer mesh format into `parry3d::shape::TriMesh` (which builds its BVH at construction). Rejects misaligned buffers, empty buffers, and out-of-bounds indices with structured errors instead of panicking. Reveal-all stance: never substitute a fallback shape.
- `crates/core/src/geom/broad_phase.rs` — `AabbF32` struct (world-AABB + id) and `pairs_overlapping(boxes, tolerance) -> Vec<(u32, u32)>`. O(N²) sweep with per-box tolerance expansion done upfront. Sub-ms for Duplex-scale (~300 instances). Spatial-grid accelerator deferred until a real model proves it's needed.
- `crates/core/src/geom/narrow_phase.rs` — `intersects(a, b) -> Result<bool>` and `min_distance(a, b) -> Result<f32>`. Wraps `parry3d::query::intersection_test` and `parry3d::query::distance`. Identity-isometry calls because ifcfast already bakes world coords into `ProductMesh.vertices` under `BakeFrame::World`.

**Memory (in `~/.claude/projects/.../memory/`):**

- `feedback-highest-value-route.md` — durable rule: default to the principled / robust option, not the "ship easy 80% first" shortcut. User has runway; depth is the bar.
- `feedback-reversible-actions-just-go.md` — durable rule: for reversible work, implement the agreed direction directly without asking permission. Stop only for irreversible actions or genuine sparring.
- `feedback-agents-md-upkeep.md` (from earlier in the same session) — keep `AGENTS.md` current with every agent-facing surface change.
- `MEMORY.md` — index updated with all three new feedback memories.

**Repo docs (from earlier in the same session, still unstaged):**

- `AGENTS.md` — new "Substrate output" section documenting `ifcfast-bundle`, the two-table parquet design, all instance columns including the v0.4.19 fingerprint set, and a worked DuckDB cross-model duplicate-detection example. CLI quick reference now includes `ifcfast-bundle`.
- `CHANGELOG.md` — `[Unreleased]` updated with the fingerprint-column additions and cache schema bump.
- `CLAUDE.md` (new, repo root) — mirrors the AGENTS.md upkeep rule so non-Claude agents and fresh sessions see it without depending on the auto-memory directory.

## Technical Details

**parry3d 0.17 API gotchas worth remembering:**

- `TriMesh::new(points, tris)` is **infallible** in 0.17 — returns `TriMesh` directly, not `Result`. (Newer parry versions made it fallible; the `MeshBuildError::KernelRejected` variant is kept for that future-proofing without firing today.)
- `Aabb::loosened(margin)` doesn't exist — expand manually by subtracting `tolerance` from `mins` and adding to `maxs`.
- `Aabb::intersects(other)` requires the `BoundingVolume` trait to be in scope: `use parry3d::bounding_volume::{Aabb, BoundingVolume};`.
- `TriMesh` doesn't implement `PartialEq`, so `assert_eq!(build_trimesh(...), Err(...))` fails to compile. Use `assert!(matches!(...))` instead.

**Float-precision in tolerance-band test design:** A near-miss test using exact-touch tolerances (gap 10 cm, expansion 5 cm each side) failed because `1.1 - 0.05` doesn't land at exactly `1.05` in IEEE 754. Use slightly looser/tighter tolerance values (4 cm to confirm no-pair, 6 cm to confirm pair) instead of relying on exact-touch arithmetic at the boundary.

**Test inventory:** 17 new geom tests (5 mesh-bridge, 7 broad-phase, 5 narrow-phase) — all pass. 4 bundle integration tests still pass (no regression from the kernel addition). 13 mesh_reveal tests still pass. Total: 74 lib tests + 4 integration + 13 mesh_reveal = 91 tests green.

**Why broad-phase is intentionally O(N²) for now:** For Duplex's ~300 instances that's 45k pair comparisons, sub-millisecond. The BVH inside each `TriMesh` is doing the heavy lifting in the narrow phase, where it actually matters. Adding a spatial-grid accelerator at this layer is premature optimisation until a 100k-instance model proves it.

**Why narrow-phase uses identity isometry:** ifcfast's mesh extractor under `BakeFrame::World` already produces world-coord vertices, so we pass `Isometry::identity()` for both shapes' transforms. If we later support running clash on local-frame meshes with their per-instance transforms applied at query time (cheaper for shared representations), the API extends to take an `Isometry<f32>` per shape.

## Next

Phase 1b continuation (next session, in rough priority):

1. **`ifcfast.clash()` Python primitive** — wrap the broad+narrow kernels into a substrate-aware CLI / Python entry that reads `instances.parquet` + `representations.parquet`, runs broad-phase on `bbox_min_xyz`/`bbox_max_xyz`, narrow-phases the candidate pairs against materialised `TriMesh` shapes, writes `clashes.parquet`. Output columns: `(id_a, id_b, guid_a, guid_b, class_a, class_b, kind, min_clearance_m, intersection_volume_m3?)`.
2. **OOBB + principal-axis fingerprint columns** via `nalgebra` PCA. Bumps cache schema v5 → v6. Two more columns on the instance table that let agents catch rotation-variant duplicates (a beam at 45° matches a beam at 45° even though their AABBs differ).
3. **Net booleans via `manifold3d`** — the big behaviour change. CSG kernel brought in behind the same `geom` feature. `IfcBooleanResult` resolves to its actual net solid, openings emit as separate clean products. This fixes the latent `volume_m3` bug on every wall-with-window in the substrate today.
4. **`ifcfast.intersection_volume(a, b)`** — once `manifold3d` is in, the narrow-phase output gains `intersection_volume_m3` from a real CSG intersection on the candidate pair. Promotes clash classification from "do they touch" to "how badly do they overlap".

Parallel / kernel-independent:

5. **Visual styles extractor** — `IfcStyledItem` / `IfcSurfaceStyleShading` / `IfcColourRgb` parsing. Half a day; no kernel dep. Closes Gap 1 for model-viewer integrations.

## Notes

- All this session's work is **unstaged**. Combined with the earlier substrate fingerprint-column work in the same session and the docs updates, the next commit batch is roughly: (a) v0.4.19 fingerprint columns + cache schema bump, (b) AGENTS.md substrate + CLI docs, (c) repo CLAUDE.md, (d) CHANGELOG, (e) geom kernel module. Recommend either one commit for the v0.4.19 user-visible release and one for the kernel scaffolding, or one bundled commit if shipping the kernel scaffolding as part of the same release.
- Workspace version still reads `0.4.18`. Bump to `0.4.19` whenever we commit, since [[cache-schema-versioning]] was bumped this session (v4 → v5) and that change goes out with the next release.
- The `_core::geom` module is currently lib-only — there's no PyO3 binding yet. The Python API surface for clash detection lands in the next session along with task #1 above.
- `parry3d` brings `nalgebra` transitively, so the explicit `nalgebra = "0.33"` dep in Cargo.toml is for our own PCA / linear-algebra work (Phase 1b task #2). If we end up never reaching into `nalgebra` directly, drop the explicit dep.
