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
| Per-product triangle meshes (verts, faces) | `m.meshes()` (`unit=` opt) |
| Sample a labeled point cloud (+ normals) | `m.point_cloud(per_m2=1000)` |
| Geometric quantities (volume, area) | `m.mesh_qto()` |
| Placement-vs-mesh sanity check | `m.drift` |
| "Which products live under this storey?" | `m.products_in(storey_guid)` |
| "What's the building of this wall?" | `m.building_of(wall_guid)` |
| Walk to project root from any guid | `m.ancestors(guid)` |
| Sample some rows from any table | `m.preview(table, n=5)` |
| Inspect a file from a shell pipeline | `ifcfast index FILE --json` |
| Plan work without paying extract cost | `ifcfast schema FILE --json` |
| Type catalogue (TypeBank-shaped) | `m.type_summary()` / `m.type_bank()` |
| ifcopenshell-style `by_type` | `m.by_type("IfcWall")` |
| What changed between v1 and v2? | `m.diff(other_path)` |

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

## Reveal-all geometry stance

When the mesh pipeline meets a composite solid (`IfcBooleanResult`,
`IfcBooleanClippingResult`, `IfcCsgSolid`) it does **not** perform the
boolean. Both operands are emitted as their own visible mesh segments
with compound tags like `boolean_first_operand|extrusion` (the host
wall) and `boolean_second_operand|halfspace_bounded` (the door clip).
This is deliberate: the file says "wall minus opening volume"; we
preserve both volumes so an agent or human can SEE the structure,
understand it, and edit it surgically rather than read a curated
summary. The glTF emitter writes each segment's `(start, count,
source)` into per-node `extras.segments` so viewers can colour /
split / filter by role.

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

## CLI quick reference

```bash
# Pipe-friendly. All accept --json.
ifcfast demo                       # works against bundled IFC
ifcfast index   FILE  --json       # tier-1 summary
ifcfast schema  FILE  --json       # column-level schema introspection
ifcfast extract FILE  --json       # data-layer extraction
ifcfast drift   FILE  --json       # placement-vs-mesh report
ifcfast cache   FILE  --json       # inspect / clear cache
```

## Reporting issues from an agent

If you hit something weird, the report worth sending includes:

1. `ifcfast index FILE --json` output (anonymise the path if needed).
2. The exact call that surprised you.
3. The schema you expected vs what you saw (use `ifcfast schema`).

That's enough for a maintainer (or the next agent on the file) to
reproduce. Open at <https://github.com/EdvardGK/ifcfast/issues>.
