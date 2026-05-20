# Hierarchical / instanced substrate — chip-class-density pattern landed

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `49ab208` → uncommitted (6 files modified, ready for commit)
- **Session scope**: Strategic-pivot session — convert the row-per-product substrate (shipped in `49ab208`) into the two-table `representations.parquet` + `instances.parquet` layout, so a 5000-window facade writes one rep + 5000 transforms instead of 5000 baked-geometry copies. The headline strategic move from the plan at `~/.claude/plans/whats-next-real-and-zippy-mist.md`.
- **Touched paths**: `crates/core/src/mesh/mod.rs`, `crates/core/src/mesh/mapped.rs`, `crates/core/src/mesh/boolean.rs`, `crates/core/src/bundle/record.rs`, `crates/core/src/bundle/parquet_sink.rs`, `crates/core/src/bin/bundle.rs`
- **Parallel sessions observed**: `none` — `git log origin/main --since` since session start returns only the prior session's `49ab208`.
- **Supersedes / superseded by**: none

## Summary

The pre-instancing substrate (commit `49ab208`, shipped 5 hours ago) bakes a full `vertices_le` + `indices_le` blob into every product row even when the mesh kernel had already de-duplicated the shape via `IfcMappedItem`'s `shape_cache`. The kernel knew; the substrate forgot. This session lifts that knowledge into the file format itself: the kernel now emits `(rep_step_id, instance_transform)` per fragment, and the substrate writer fans each `ProductMesh` into a representation row (the actual geometry, written once per unique `rep_id`) and an instance row (identity + semantics + a `rep_id` foreign key + the 4x4 transform that places that rep into world space).

Validated end-to-end on Duplex (286 instances → 259 unique reps, one rep referenced by 14 cabinet instances) and on the 834 MB ST28_RIV MEP file: substrate dropped from ~180 MB single-file (pre-split) to 68.6 MB across the two files — **62% smaller**, with no loss of geometric fidelity or the reveal-all X-ray invariant.

Key validation numbers from ST28_RIV:
- **15.66x sharing on `IfcFlowTreatmentDevice`** (1,065 instances → 68 unique shapes)
- 5.94x sharing on `IfcFlowController`, 5.09x on `IfcFlowTerminal`
- 47 reps each referenced by >100 instances
- one rep referenced by 1,002 instances (a MagiCAD MEP family that appears 1000x)
- referential integrity: every `instance.rep_id` resolves into `representations.parquet`; no orphan reps

The "QTO of an nm-level chip" query — `SELECT class, COUNT(*) AS instances, COUNT(DISTINCT rep_id) AS unique_shapes FROM instances GROUP BY class` — now runs as designed and demonstrates the EDA pattern (cell library + instance arrays) transplanted into AECO at the substrate layer.

## What changed, by layer

### Mesh kernel (`crates/core/src/mesh/`)

`MeshFragment::Mesh` now carries `rep_step_id: u64` + `instance_transform: Mat4`:

- For direct geometry dispatch (extrusion, faceset, brep, csg, revolved, curveset): `rep_step_id = item_id`, `instance_transform = Mat4::IDENTITY`. Two products referencing the same `IfcExtrudedAreaSolid` step_id will dedup.
- For `IfcMappedItem` expansion in `mesh/mapped.rs`: the previous `transform_mesh(&mesh, composed)` call (which baked the per-instance composition into the vertex stream) is removed. The untransformed source mesh passes through, and `composed = t_target * t_origin` is attached as the fragment's `instance_transform`. Nested `IfcMappedItem` instances multiply transforms (`composed * inner_xform`) so deeper nests still produce a single per-instance transform.
- For composite handlers (`boolean::boolean_result`, `csg_solid`, `retag`): operand fragments thread through unchanged — they keep whatever `rep_step_id` / `instance_transform` the leaf set.
- The cache (`shape_cache: HashMap<u64, Vec<(LocalMesh, &'static str)>>`) returns the lookup key (`item_id`) as `rep_step_id` and identity as `instance_transform` — accurate because the cache only stores non-composite direct geometry.

`mesh_ifc_streaming` now applies `effective = world * instance_transform` per vertex when baking world-coord geometry (so back-compat consumers like the glTF / OBJ / stats writers see the same world positions as before), AND builds a `parts: Vec<InstancePart>` parallel to `segments` carrying the untransformed local mesh + per-instance transform for each fragment.

`ProductMesh` gained `ifc_id: u64`, `parts: Vec<InstancePart>`, `world_transform: [f32; 16]`. Existing `vertices` / `indices` / `segments` are unchanged — the 9 existing `mesh_reveal.rs` integration tests pass unmodified.

### Substrate writer (`crates/core/src/bundle/`)

`ProductRecord` → split into `RepresentationRecord` + `InstanceRecord` (in `record.rs`). `pair()` → `pair_split()` returns `(Option<RepresentationRecord>, InstanceRecord)`. The `pick_rep_id` policy:

1. Single-fragment product with non-zero `rep_step_id`: `rep_id = parts[0].rep_step_id`, `source_kind = "shared_or_direct"`. Local mesh from `parts[0].local_*` (this is what dedupes across instances). Transform = `world * parts[0].instance_transform`.
2. Multi-fragment composite (boolean walls, multi-item representations): `rep_id = mesh.ifc_id` (product step_id, guaranteed unique), `source_kind = "composite"`. The rep carries the world-baked geometry; instance transform is identity. Cross-product composite dedup needs content hashing — queued as a follow-on optimization.

`ParquetSink` (in `parquet_sink.rs`) holds two `ArrowWriter<File>`s + a `HashSet<u64>` for rep_id dedup. `create_in_dir(out_dir, bundle)` opens `representations.parquet` + `instances.parquet`. On each `on_product` the rep_record (if any and new) is buffered to `pending_reps`; the instance_record is always buffered to `pending_insts`. Row-group flush is independent per file (default 1024 rows). `finish()` returns `(products_written, reps_written)`.

The bundle binary (`bin/bundle.rs`) now takes an output **directory** instead of a single file (default: `{stem}.bundle/` next to the input). It also emits a `view.sql` with the canonical join view so DuckDB consumers that don't care about instancing can read `products` as if the schema hadn't changed:

```sql
CREATE OR REPLACE VIEW products AS
  SELECT i.*, r.source_kind AS rep_source_kind, r.mesh_source AS rep_mesh_source,
         r.vertex_count AS rep_vertex_count, r.triangle_count AS rep_triangle_count,
         r.vertices_le AS rep_vertices_le, r.indices_le AS rep_indices_le,
         r.segments AS rep_segments, r.local_bbox_min_xyz AS rep_local_bbox_min_xyz,
         r.local_bbox_max_xyz AS rep_local_bbox_max_xyz
    FROM instances i
    LEFT JOIN representations r USING (rep_id);
```

## Architecture stance

This is the **lasting AECO contribution** half of the substrate story. The pre-instancing substrate solved "stream the geometry without OOMing" (the `49ab208` slice). This session solves "scale the substrate to chip-class density" — the same SQL works on a building, a campus, a city, or (in principle) a chip layout, because the substrate is hierarchical at the schema level. EDA / GDSII / OASIS have used cell-library + instance-array for decades; AECO has had `IfcRepresentationMap` since IFC2 but every downstream tool flattens it on the way out. The substrate now refuses to flatten.

Reveal-all invariants preserved:
- `segments` (the structural-provenance column) moved from instances to representations — the shape of geometry IS a rep property, not a per-instance one. `boolean_first_operand|extrusion` etc. tags survive.
- Composite reps (`source_kind = "composite"`) carry their multi-segment provenance unchanged.
- `unhandled:IFCXXX` stats unaffected.
- World-coord `ProductMesh.vertices` / `.indices` / `.segments` still populated for back-compat readers (stats.rs, gltf.rs, obj.rs).

## Numbers

| File              | products | unique reps | inst/rep | substrate size |
|-------------------|----------|-------------|----------|----------------|
| Duplex (2.4 MB)   | 286      | 259         | 1.10x    | 0.1 MB         |
| ST28_RIV (834 MB) | 85,976   | 51,563      | 1.67x    | 68.6 MB        |

ST28_RIV's 1.67x average sharing ratio is lower than the plan's 10-20x prediction because MagiCAD writes ~47% of fragments as `faceset_sbsm` (each its own step_id, no IfcMappedItem dedup unless the same step_id is reused), and even within `mapped` paths it generates many unique `IfcRepresentationMap` entries. The TAIL distribution is the win: 47 reps explain >4700 instances, one rep explains 1002 instances, 15.66x sharing on `IfcFlowTreatmentDevice`. Content-hashing identical-geometry-across-step_ids is the queued follow-on that closes the gap on Tekla / ArchiCAD / MagiCAD exports that don't lean as hard on `IfcRepresentationMap`.

## Tests

- 9 existing `tests/mesh_reveal.rs` integration tests pass unmodified (X-ray invariant preserved end-to-end).
- 3 `bundle::tests::normalize_*` unit tests pass.
- No new tests yet for the rep dedup specifically — the substrate-shape assertions ride on the live Duplex + ST28 outputs. A unit test that builds a synthetic IFC with two `IfcMappedItem` instances pointing to the same `IfcRepresentationMap` and asserts `representations.num_rows == 1, instances.num_rows == 2` is queued as the natural addition.

## Neste

1. **Commit + push to `origin/main`.** Six files modified, all on the substrate / instancing path. Per the user's standing memory `feedback_confirm_push_to_default_branch` (push to default-branch without preamble when scope is authorized), commit and push as a single "instancing" change.
2. **Synthetic test for rep dedup.** Build a minimal IFC with two MappedItem instances → assert rep count = 1, instance count = 2, both transforms differ. Cheap, would have caught a class of regressions if I'd had it.
3. **Update README + dashboards.** The bundle CLI now emits a directory; the site's "how to read the substrate" section and any external README examples need the `view.sql` mention. (The plan's "DuckDB ergonomics layer" — already shipped.)
4. **The follow-on plan opens up.** With the substrate hierarchical:
   - Move 2 (write-back via USD composition layers) becomes well-defined — `rep_id` is the natural anchor for a USD prim, and per-instance overrides ARE USD layers.
   - Move 3 (Parquet 2.0 GEOMETRY + 3D Tiles) becomes additive — rep tessellations are now small enough to fit into 3D Tiles tilesets per-rep instead of per-instance.
   - The next-step "string interning + zero-clone regrouping" tactical work still applies to the pre-pass.
5. **`.ifczip` silent-drop (#19)** still blocks running this on the real 1 GB Sannergata.

## Apne sporsmal

- **Content-hash dedup** for direct geometry (and across-step_id duplicates inside MappedItem paths) — listed in plan as a future optimization. The 62% size reduction on ST28 happened WITHOUT it; a follow-on that adds `xxhash3` content hashing on `local_vertices_le + local_indices_le + segments_canonical` would likely close 20-40% more on Tekla / ArchiCAD exports that don't lean on `IfcRepresentationMap`. Worth measuring before committing the CPU cost.
- **Composite rep_id stability** — today multi-fragment composites use `mesh.ifc_id` (product step_id) as `rep_id`, which is stable across reruns but never dedupes. A content hash here is what unlocks composite dedup (two identical boolean walls in different storeys collapse to one rep). Same xxhash3 dependency as the bullet above.
- **No Python API yet for the split.** `python/ifcfast/cache.py` still reads a single `products.parquet`. With the schema split, the bundled output now lives in a directory with `representations.parquet` + `instances.parquet` + `view.sql`. Either: (a) point Python at the `view.sql` and read the join, (b) teach Python to JOIN at read time, or (c) emit a third pre-joined `products.parquet` for back-compat readers (defeats the point on RAM/space — discouraged).
