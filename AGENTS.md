# AGENTS.md — using `ifcfast` from an AI agent

`ifcfast` is designed agent-first. If you're an LLM or an agent
framework opening IFC files programmatically, this is the page you
want.

> **Status: early & unverified.** `ifcfast` is under active
> development and has not been validated against established tools.
> Treat its output as provisional — cross-check against
> `ifcopenshell` or your existing toolchain before relying on it, and
> [open an issue](https://github.com/EdvardGK/ifcfast/issues) when
> something looks wrong. It complements `ifcopenshell` (which owns
> geometry kernels, schema, and authoring) rather than replacing it.

## What `ifcfast` gives you

- **Single import, no kernel.** A native (Rust) core reads the IFC
  STEP data section directly. Pandas DataFrames out, no geometry
  kernel to compile or load on the hot path.
- **Data layers as pandas.** Property sets, quantities, materials,
  classifications — long-format DataFrames you can filter, join,
  pivot, export.
- **Geometry without a CAD kernel.** Per-product triangle meshes
  (`m.meshes()`), area-weighted point-cloud sampling with normals
  (`m.point_cloud()`), and geometric quantities (`m.mesh_qto()`) —
  handed back as numpy / pandas, ready for trimesh / Open3D.
- **Spatial-relationship graph built in.** `m.contained_in /
  .aggregates / .storey_building` + seven traversal helpers. One call
  walks wall → storey → building → site → project.
- **Self-describing.** `m.summary()` and `m.schemas` answer "what am I
  looking at" without triggering extracts. Every CLI subcommand has
  `--json`.
- **Parquet cache.** The second open of a file reuses extracted
  tables; the cache key invalidates on any edit or library change.

## Via MCP (zero-config, any agent ecosystem)

Drop `ifcfast` into Claude Desktop, Cursor, ChatGPT MCP, or any
MCP-aware client — no custom integration code:

```bash
pip install 'ifcfast[mcp]'
```

```json
{
  "mcpServers": {
    "ifcfast": { "command": "ifcfast-mcp" }
  }
}
```

That gives the agent a set of tools (`open_ifc`, `summary`, `schemas`,
`preview`, `types`, `by_type`, `parent`, `children`, `ancestors`,
`descendants`, `storey_of`, `building_of`, `products_in`, `diff`, …)
plus the `ifcfast://agents-guide` resource (this document).

## The 30-second ramp

```python
import ifcfast

# Bundled demo — works without any external IFC.
m = ifcfast.open(ifcfast.example_path())

print(m.summary())     # schema, counts, available tables, samples
print(m.schemas)       # column-level introspection
print(m.preview("aggregates", n=3))
```

Or paste this into your system prompt:

```python
print(ifcfast.system_prompt())
```

Returns a compact paragraph covering every public method. Stable across
releases (additions only, never reorganisations).

## Decision tree for common tasks

| You want… | Call this |
|---|---|
| File metadata only, no parse | `ifcfast.header(path)` |
| Tier-1 index (products + storeys) | `m = ifcfast.open(path)`; `m.summary()` |
| Property sets / quantities / materials | `m.psets` / `m.quantities` / `m.materials` |
| Classification refs (NS 3451, OmniClass, …) | `m.classifications` |
| Per-product triangle meshes (verts, faces) | `m.meshes()` (`unit=`, `cut_openings=` opts) |
| Sample a labeled point cloud (+ normals) | `m.point_cloud(per_m2=1000)` |
| Stream a point cloud in bounded-RAM chunks | `m.iter_point_cloud(per_m2=1000, chunk_points=1_000_000)` |
| One-call viewer export to glTF binary | `m.to_gltf("out.glb")` (cuts on, instancing on, quant on) |
| Geometric quantities (volume, area) | `m.mesh_qto()` (cut_openings=True by default since v0.4.28) |
| Placement-vs-mesh sanity check | `m.drift` (SI columns; check `m.world_coordinate_baked` for the file-level signal) |
| "Which products live under this storey?" | `m.products_in(storey_guid)` |
| "What's the building of this wall?" | `m.building_of(wall_guid)` |
| Walk to project root from any guid | `m.ancestors(guid)` |
| Sample some rows from any table | `m.preview(table, n=5)` |
| Inspect a file from a shell pipeline | `ifcfast index FILE --json` |
| Plan work without paying extract cost | `ifcfast schema FILE --json` |
| Type catalogue (TypeBank-shaped) | `m.type_summary()` / `m.type_bank()` |
| ifcopenshell-style `by_type` | `m.by_type("IfcWall")` |
| Iterate every product as `ProductRow` | `for p in m:` (or `m.products`, `m.filter(entity=...)`) |
| Count of products (matches `m.products`) | `len(m)` |
| Same data as a pandas DataFrame | `m.products_df` |
| What changed between v1 and v2? | `m.diff(other_path)` |

## Substrate output (DuckDB-queryable parquet)

For multi-file / cross-session / pipeline workflows, the in-memory
Python API isn't always the right shape. The `bundle` step emits a
**two-table parquet substrate** you can query with DuckDB, polars, or
any arrow-aware tool. Drive it from Python or the CLI:

```python
import ifcfast
info = ifcfast.bundle("model.ifc")             # -> model.bundle/
info = ifcfast.bundle("model.ifc", "out/")     # explicit out_dir
```

```bash
ifcfast bundle path/to/model.ifc               # -> path/to/model.bundle/
ifcfast bundle path/to/model.ifc out/          # explicit out_dir
# writes:
#   out/instances.parquet        (one row per IfcProduct)
#   out/representations.parquet  (one row per unique mesh shape, dedup'd)
#   out/view.sql                 (DuckDB JOIN view that wires them)
```

**Why two tables?** A 5000-window facade with one shared
`IfcRepresentationMap` writes ~1 representation row + 5000 instance
rows, not 5000 copies of the same geometry. Working-set RAM stays
bounded on 1 GB+ files.

**Instance columns include** (non-exhaustive — schema is self-
describing via `pq.read_schema(...)`):

- Identity / structure: `ifc_id`, `guid`, `class` (normalised — "Wall"
  not "IfcWallStandardCase"), `source_class`, `name`, `tag`,
  `storey_guid`, `aggregates_parent_guid`, `type_guid`, `rep_id`.
- Placement / world: `transform` (4×4 col-major), `placement_xyz`.
- World-AABB: `bbox_min_xyz`, `bbox_max_xyz`.
- **Geometric fingerprint** (since v0.4.19, cache schema v5):
  `centroid_xyz` (world-AABB midpoint, falls back to `placement_xyz`
  for geometryless products), `vertex_count`, `triangle_count`.
- QTO: `volume_m3`, `aabb_volume_m3`, `surface_area_m2`, orientation-
  bucketed area columns, `largest_surface_m2`, `smallest_surface_m2`,
  `surface_count`, `mesh_quality` (`"closed"` / `"open_shell"` /
  `"degenerate"`).
- Semantic payload: `materials`, `psets`, `quantities`,
  `classifications` (list-of-struct columns — `UNNEST` in DuckDB).
  Each `psets` and `quantities` struct carries `source`
  (`"instance"` or `"type"`) so consumers can distinguish a value
  declared directly on the product from one inherited via
  `IfcRelDefinesByType` (since v0.4.29 — see Conventions).
- Per-face stream: `surfaces` (one entry per distinct planar face).

**Why the fingerprint columns matter for agents:** they let you
compose cross-model duplicate detection, version-diff, and
broad-phase clash candidate filtering as pure DuckDB queries — no
re-parse, no recompute on every join. Example: candidates for
"same physical thing modeled twice across discipline models":

```sql
SELECT a.guid AS a_guid, b.guid AS b_guid, a.class, b.class
FROM 'ark/instances.parquet' a
JOIN 'rib/instances.parquet' b
  ON sqrt(
       (a.centroid_xyz[1]-b.centroid_xyz[1])^2 +
       (a.centroid_xyz[2]-b.centroid_xyz[2])^2 +
       (a.centroid_xyz[3]-b.centroid_xyz[3])^2
     ) < 0.05                                              -- 5cm
 AND abs(a.aabb_volume_m3 - b.aabb_volume_m3)
     / NULLIF(a.aabb_volume_m3, 0) < 0.05;                 -- ±5% volume
```

**Cache schema version** is at `_CACHE_SCHEMA_VERSION` in
`python/ifcfast/header.py` — when it bumps, the column set changed.
Old caches become orphaned automatically.

### Narrow-phase clash (`ifcfast.clash()`)

True mesh-mesh intersection runs against the same substrate. The
engine reads `instances.parquet` + `representations.parquet`,
broad-phases on the `bbox_*` columns, narrow-phases candidate pairs
against world-baked `parry3d` TriMeshes, and writes a third file —
`clashes.parquet` — next to the inputs:

```python
import ifcfast

# default: hard clashes only, writes clashes.parquet alongside
df = ifcfast.clash("model.bundle/")

# soft-clash: also emit pairs within 50 mm clearance
df = ifcfast.clash("model.bundle/", tolerance_m=0.05)

# cross-discipline only — suppress wall-vs-wall, slab-vs-slab
df = ifcfast.clash(
    "model.bundle/",
    exclude_self_class=["Wall", "Slab"],
)
```

`clashes.parquet` columns:

| column           | type    | meaning                                                  |
|------------------|---------|----------------------------------------------------------|
| `ifc_id_a/b`     | UInt64  | STEP entity ids — join back to `instances.parquet`       |
| `guid_a/b`       | Utf8    | IFC GUIDs                                                |
| `class_a/b`      | Utf8    | normalised classes (`"Pipe"`, not `"IfcPipe"`)           |
| `kind`           | Utf8    | `"hard"` (meshes intersect) or `"clearance"` (within tol)|
| `min_distance_m` | Float32 | minimum mesh-to-mesh distance, metres; `0.0` for hard    |

The engine is just the *fact* layer — does this pair touch, by how
much. **Policy** (wall-meets-slab is normally not a real clash;
clashes inside the same opening assembly are noise; BCF emit;
discipline routing) lives in the layer above and queries
`clashes.parquet` joined to `instances.parquet`. Example:
"every MEP-vs-structure hard clash on level 3":

```sql
SELECT c.guid_a, c.guid_b, c.class_a, c.class_b
FROM 'model.bundle/clashes.parquet' c
JOIN 'model.bundle/instances.parquet' ia ON c.ifc_id_a = ia.ifc_id
JOIN 'model.bundle/instances.parquet' ib ON c.ifc_id_b = ib.ifc_id
WHERE c.kind = 'hard'
  AND ia.storey_name = 'Level 3'
  AND (ia.class IN ('Pipe','Duct','CableCarrier')) XOR
      (ib.class IN ('Pipe','Duct','CableCarrier'));
```

**Units.** `tolerance_m` and `min_distance_m` are always in metres,
regardless of the source IFC's linear unit. The substrate records
the project's unit scale as parquet schema metadata
(`ifcfast.unit_scale`) and the clash engine converts at load time.

## Conventions you can rely on

- **Traversal helpers never raise on unknown guids.** Missing → `None`
  for scalars, `[]` for lists. Safe to call without guarding.
- **DataFrames are long-format, one row per fact.** No nested fields,
  no JSON-in-cell. Easy to filter, easy to join, easy to dump to Excel.
- **Missing values are `nan` for strings (pandas `StringDtype`).** Use
  `.isna()`, not `== None`. If you're cross-checking against
  `ifcopenshell` (which returns `None`), coerce on the comparison side.
- **All CLI subcommands accept `--json`** and emit a stable JSON shape.
  See `python/ifcfast/cli.py` for the exact keys; the top-level
  `"path"` and `"tables"` fields are guaranteed-present.
- **Cache version is in the manifest** (`~/.cache/ifcfast/{key}/meta.json`)
  — bumping the library invalidates incompatible caches automatically.
- **Recoverable Rust failures raise `ifcfast.IfcfastError`** (since
  v0.4.20). The geometry-pipeline entry points
  (`m.point_cloud`, `m.iter_point_cloud`, `m.meshes`, `m.mesh_qto`,
  `m.drift`) catch Rust panics at the PyO3 boundary and surface them
  as `IfcfastError` instead of the uncatchable
  `pyo3_runtime.PanicException`. Wrap geometry calls in
  `try: ... except ifcfast.IfcfastError: ...` if you need per-file
  resilience in a corpus pipeline.
- **`m.psets` inherits type-level properties by default** (since
  v0.4.29, cache schema v7). Properties carried on an
  `IfcTypeObject.HasPropertySets` and bound via
  `IfcRelDefinesByType` surface on every related instance, tagged
  `source = "type"`. Properties declared directly on the instance
  via `IfcRelDefinesByProperties` carry `source = "instance"` and
  shadow same-named type properties (instance wins on collision —
  matches `ifcopenshell.util.element.get_psets(..., should_inherit=True)`).
  Filter with `m.psets[m.psets.source == "instance"]` if you need
  the pre-v0.4.29 shape. Common payoff: manufacturer / type marks /
  fire ratings on Revit / Tekla / Archicad exports that live at the
  type level were silently dropped before this fix (GH #36).
- **`m.quantities.unit_step_id` falls back to project defaults**
  (since v0.4.29, cache schema v8). When `IfcQuantity*.Unit` is null
  — the common Revit / ArchiCAD authoring pattern — the column now
  resolves to the project's `IfcUnitAssignment` `IfcSIUnit` for the
  quantity's kind (`Length`→`LENGTHUNIT`, `Area`→`AREAUNIT`,
  `Volume`→`VOLUMEUNIT`, `Weight`→`MASSUNIT`, `Time`→`TIMEUNIT`).
  `Count` stays null — it's dimensionless. Explicit per-quantity
  `Unit` refs still win (no fallback fires when the slot is set).
  Resolution is `IfcSIUnit`-only; `IfcConversionBasedUnit` /
  `IfcDerivedUnit` resolution is a separate feature (GH #43).
- **`m.quantities` inherits type-attached quantities by default**
  (since v0.4.29, cache schema v10). Mirrors the `m.psets` story
  (`IfcTypeObject.HasPropertySets` accepts ANY
  `IfcPropertySetDefinition` — `IfcElementQuantity` is one such
  subtype). Type-attached quantities surface on every related
  instance tagged `source = "type"`; instance-declared quantities
  carry `source = "instance"` and shadow same-named type quantities
  on `(qto_name, quantity_name)` collision. Unit fallback from the
  v0.4.29 #43 fix runs on inherited rows too, so the
  `unit_step_id` column is usable even when the type-side quantity
  omits the explicit Unit slot. Filter `m.quantities[m.quantities.source == "instance"]`
  for pre-v0.4.29-shape behaviour (GH #45).
- **`m.psets` marks unrecognised property classes instead of
  dropping them** (since v0.4.29, cache schema v9). Any
  `IfcSimpleProperty` subclass ifcfast doesn't have a per-class
  parser for surfaces as a row with `value = None` and
  `value_type = "unhandled:IFCXXX"` (e.g.
  `"unhandled:IFCPROPERTYREFERENCEVALUE"`). Enumerate gaps with
  `m.psets[m.psets.value_type.fillna("").str.startswith("unhandled:")]`.
  Same release also adds `IfcPropertyTableValue` as a recognised
  class — its row carries
  `value = "defining1=>defined1, defining2=>defined2, ..."`,
  `value_type` taking the DefinedValues axis type. Both changes
  trace to GH #38.

## Streaming point cloud (`m.iter_point_cloud(...)`)

`m.point_cloud(per_m2, seed)` materialises the entire sampled cloud
in one DataFrame — for 200 MB – 1 GB ARK IFCs that DataFrame doesn't
fit in 32 GB workstation RAM and the failure modes (Arrow realloc,
Python `MemoryError`, Rust panic) can lock the host.

```python
for chunk in m.iter_point_cloud(per_m2=200.0, seed=0,
                                 chunk_points=1_000_000):
    chunk.to_parquet(out_dir / f"part-{i:04d}.parquet")
    i += 1
```

- Peak RAM is ~`chunk_points × 80 B` (xyz + normals + guid + entity
  strings), independent of total point count.
- Every chunk has the same columns as the single-shot
  `point_cloud()` output, plus `df.attrs["global_shift"]` (identical
  across chunks for a given file).
- A single product whose samples cross a chunk boundary splits; the
  `guid` column still tags every row, so a groupby-by-GUID across
  chunks reconstructs the per-product sample set.
- The Rust mesh pass runs on a worker thread; `__next__` releases the
  GIL while waiting on the worker, so Python KeyboardInterrupt fires
  promptly and you don't block other Python work during tessellation.
- Dropping the iterator early (e.g. `it = ...; next(it); del it`)
  signals the worker via an atomic stop flag — no thread leak.

## Reveal-all geometry stance

When the mesh pipeline meets a composite solid (`IfcBooleanResult`,
`IfcBooleanClippingResult`, `IfcCsgSolid`) it does **not** perform the
boolean by default. Both operands are emitted as their own visible
mesh segments with compound tags like `boolean_first_operand|extrusion`
(the host wall) and `boolean_second_operand|halfspace_bounded` (the
door clip). This is deliberate: the file says "wall minus opening
volume"; we preserve both volumes so an agent or human can SEE the
structure, understand it, and edit it surgically rather than read a
curated summary. The glTF emitter writes each segment's `(start, count,
source)` into per-node `extras.segments` so viewers can colour /
split / filter by role.

**Opt-in cut: `m.meshes(cut_openings=True)`.** For viewer / rendering
work where you want the net solid (doors and windows as actual
holes), pass `cut_openings=True`. The mesher then folds every
`boolean_second_operand|...` segment into the host via CSG
(`manifold3d`) before returning, so the output has a single segment
per product tagged `cut_openings`. The substrate stays reveal-all —
this flag only affects `m.meshes()` / `m.iter_meshes()` callers,
not `instances.parquet` / `representations.parquet`. Requires a
wheel built with the `csg` feature (raises `RuntimeError` otherwise).

**`m.mesh_qto(cut_openings=True)` is the default since v0.4.28** —
authored `Qto_*Volume` values are net (openings subtracted), and so
is the new geometric default. Pass `cut_openings=False` if you want
the gross (uncut) host volume.
Both opening patterns are covered: **in-representation** booleans
(`IfcBooleanClippingResult(host, opening)`) AND **cross-product**
openings (`IfcRelVoidsElement` linking a separately-modelled
`IfcOpeningElement` to a solid host). Cross-product openings are
suppressed from the visible product set in cut mode (they're
cutters, not user-visible products) and folded into their host's
net solid; in reveal-all (`cut_openings=False`, the default) both
the host and the opening still emit as separate products with
their full operand-by-operand fidelity preserved.

Unhandled representation types (e.g. `IfcRevolvedAreaSolid`,
`IfcSurfaceCurveSweptAreaSolid`) appear as `unhandled:IFCXXX`
entries in `mesh_stats.by_source` so you can see exactly what the
file contained that wasn't tessellated, instead of a silent drop.

## What `ifcfast` does NOT do (yet)

- Write or modify IFCs. Read-only by construction. (Round-trip
  editing is the next major milestone — see north-star below.)
- True boolean / CSG composition. By design — we surface BOTH
  operands instead, per the reveal-all stance above. If you need
  net geometry, compose the segments downstream.
- Curved-surface tessellation for `IfcAdvancedBrep` — the face
  loops are triangulated as polygons, marked `advanced_brep_approx`.
- `IfcRelConnectsElements` and other non-spatial / non-aggregation
  relationships beyond the four already extracted
  (`IfcRelContainedInSpatialStructure`, `IfcRelAggregates`,
  `IfcRelVoidsElement`, `IfcRelDefinesByType`). File an issue with the
  relation name + a sample if you need another one.

## North star: surgical modelling via code

The reveal-all stance is the foundation for "read → edit → write"
round-trips. Today the parser is read-only. The path to editing is:
preserve per-entity byte offsets, expose a write-back surface that
mutates the in-memory STEP buffer, and emit a deterministic
serialiser. Tracked separately — until then, ifcfast is the X-ray
that tells you exactly what's in the file so you know what to
change.

If your agent task hits one of these, file an issue with the file
shape — these are the next-tier extensions.

## Cost model (relative, not benchmarked)

No hard numbers here — `ifcfast` isn't benchmarked yet. The useful
distinction is *which calls are cheap and which trigger work*:

- **Free / near-free** — `m.summary()`, `m.schemas`,
  `ifcfast.header(path)`, and `m.preview()` on the relationship
  tables. No data-layer extraction. Call these liberally to plan.
- **Triggers a parse / extract** — `ifcfast.open(path)` (first time,
  before cache), the lazy data layers (`m.psets`, `m.quantities`,
  `m.materials`, `m.classifications`, `m.drift`), and the geometry
  calls (`m.meshes()`, `m.point_cloud()`, `m.mesh_qto()`).
- **Cheap on re-open** — a previously-opened file reuses its parquet
  cache until the file (or library version) changes.

Plan with the free calls, then spend on the extracts you actually
need.

**Tessellation is parallel** (since v0.4.20). `m.meshes()` and
everything downstream (substrate emit, `m.mesh_qto()`,
`m.point_cloud()`) tessellate one product per rayon worker thread.
Defaults to all available cores; cap with `RAYON_NUM_THREADS=<n>` in
the environment if you need to share the host. Emission order to
sinks is **still IFC entity-table iteration order** — the parallel
phase fans out, results are reordered back into source order before
the streaming sink sees them. So existing consumers that relied on
stable order (substrate writer, OBJ/glTF writers, the cut_openings
wrapper) keep that contract. End-to-end speedup at 8 cores is
~2.4–2.6× on real files (since v0.4.22; the earlier ~2× cap was
the serial phase-1 Amdahl tail, removed in GH #26 by parallelising
the entity-table walk + parsing with a frozen `PlacementResolver`
cache).

**RAM is bounded across the parallel mesh pass** (since v0.4.21).
Workers stream `(seq, ProductOutcome)` over a lock-free bounded
channel (`crossbeam-channel`) instead of materialising every
outcome into one large `Vec` before drain. Peak in-flight memory =
`channel_cap × per_product_mesh ≈ a few MB` for typical AEC files,
independent of total product count — so 1 GB IFCs through the
substrate writer don't balloon RSS during tessellation. There's
also a `RAYON_NUM_THREADS=1` fast path that bypasses the channel
entirely (no scaffolding cost on single-thread hosts).

**glTF output uses `EXT_mesh_gpu_instancing`** (since v0.4.23) for
products that share a single-fragment representation. Each rep
emits one shared mesh + one node with per-instance TRS attributes;
per-instance identity (`guid`, `entity`, `segments`) goes into
`node.extras.instances` as a parallel array indexed by instance
order. Multi-fragment products (booleans, CSG composites) and
rep-unique singletons fall through to the baked path (one mesh per
product, world-coord vertices) — backwards-compat with viewers
that don't read `EXT_mesh_gpu_instancing` is preserved for those.
Pick-to-BIM: viewers should read `node.extras.guid` (baked) OR
`node.extras.instances[instance_id].guid` (instanced).

**All positions are `KHR_mesh_quantization` u16** (since v0.4.25).
Baked positions: per-node `translation` = AABB min and `scale` =
range/65535, the runtime reconstructs world coords as `translation
+ scale * u16_vertex`. Instanced positions: the per-rep quant
denorm is baked into each per-instance TRS via
`(T, R, S) := (T_inst + R_inst*(S_inst⊙T_quant), R_inst, S_inst⊙S_quant)`
so the instanced node carries no local TRS and the per-instance
TRS goes straight from u16 to world. Quantization error is
±range/131070 — for a 1 m-spanning mesh that's ±15 μm, well under
the precision an IFC-authored model carries anyway. Combined size
savings on real files: LBK_RIBp_C 118.5 MB → 56 MB (52% smaller,
2.1× compression) with instancing + quantization stacked; LBK_ARK_C
86.9 MB → 68 MB (22% smaller from quantization alone where
instancing was a near-no-op).

**One-call viewer export: `m.to_gltf(out_path, cut_openings=True)`**
(since v0.4.25). Defaults are viewer-optimal — opening geometry
subtracted from host walls via the manifold-csg boolean path
(both in-rep `IfcBooleanClippingResult` and cross-product
`IfcRelVoidsElement`), `KHR_mesh_quantization` u16 positions,
`EXT_mesh_gpu_instancing` when `cut_openings=False` (the cut
modifies per-product geometry, so instancing is disabled with
cuts on). Per-product identity carries through `node.extras.guid`
(baked) and `node.extras.instances[instance_id].guid` (instanced).
The wheel ships with `csg` in the default Cargo features (since
v0.4.25), so `pip install ifcfast` is enough — no extras or
build-from-source needed.

## CLI quick reference

```bash
# Pipe-friendly. All accept --json.
ifcfast demo                       # works against bundled IFC
ifcfast index   FILE  --json       # tier-1 summary
ifcfast schema  FILE  --json       # column-level schema introspection
ifcfast extract FILE  --json       # data-layer extraction
ifcfast drift   FILE  --json       # placement-vs-mesh report
ifcfast cache   FILE  --json       # inspect / clear cache
ifcfast bundle  FILE [OUT_DIR]     # parquet substrate (see "Substrate output")
                                   # writes instances.parquet +
                                   # representations.parquet + view.sql

# Narrow-phase clash against a bundle directory. From Python:
#   ifcfast.clash(bundle_dir, tolerance_m=0.0)
# A standalone `ifcfast-clash` binary is also built from the core
# crate (`cargo build --release --bin ifcfast-clash`), but it is
# NOT shipped on PyPI — driving clash from Python is the supported
# wheel-side path.
```

## Reporting issues from an agent

If you hit something weird, the report worth sending includes:

1. `ifcfast index FILE --json` output (anonymise the path if needed).
2. The exact call that surprised you.
3. The schema you expected vs what you saw (use `ifcfast schema`).

That's enough for a maintainer (or the next agent on the file) to
reproduce. Open at <https://github.com/EdvardGK/ifcfast/issues>.
