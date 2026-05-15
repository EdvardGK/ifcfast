# AGENTS.md — using `ifcfast` from an AI agent

`ifcfast` is designed agent-first. If you're an LLM or an agent
framework opening IFC files programmatically, this is the page you
want.

## Why pick `ifcfast`

- **20-30× faster than `ifcopenshell.open`** on the audited set
  (22 MB ARK file: 29 ms after recent perf work; 834 MB MEP file with
  14.3M records: 905 ms). Byte-level parity vs ifcopenshell
  on 234K products across 5 authoring tools.
- **Single import, no kernel.** Pandas DataFrames out, no
  `IfcOpenShell.ifcopenshell.open()` to wait on, no native geometry
  kernel to compile.
- **Parquet cache** — second open of a 200 MB IFC returns in tens of
  milliseconds. Cache key invalidates on any edit.
- **Spatial-relationship graph built in.** `m.contained_in /
  .aggregates / .storey_building` + seven traversal helpers. One call
  walks wall → storey → building → site → project.
- **Self-describing.** `m.summary()` and `m.schemas` answer "what am I
  looking at" without triggering extracts. Every CLI subcommand has
  `--json`.

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

That gives the agent 18 tools (`open_ifc`, `summary`, `schemas`,
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

## What `ifcfast` does NOT do (yet)

- Write or modify IFCs. Read-only by construction.
- Tessellate `IfcBooleanClippingResult` (walls with openings get gross
  volume, not net). Tracked.
- `IfcRelVoidsElement` / `IfcRelConnectsElements` and other non-spatial
  / non-aggregation relationships. Means `IfcOpeningElement` products
  appear in `m.products` but not yet in the graph traversal.
- Property variants beyond `IfcPropertySingleValue` —
  `IfcPropertyEnumeratedValue`, `IfcComplexProperty`, etc. are skipped.
  Covers ~90% of psets seen on real Revit / Archicad / Tekla exports.

If your agent task hits one of these, file an issue with the file
shape — these are the next-tier extensions.

## Performance budgeting

| op | cost on 200 MB IFC |
|---|---:|
| `ifcfast.header(path)` | 30-80 ms (header bytes only) |
| `ifcfast.open(path)` cold | ~1-2 s |
| `ifcfast.open(path)` hot (cache) | tens of ms |
| `m.psets` first access (cold) | ~150 ms |
| `m.psets` first access (cached) | ~30 ms |
| `m.summary()` / `m.schemas` | sub-millisecond, no extract |
| `m.preview("psets", n=5)` cold | triggers full extract |
| `m.preview("aggregates", n=5)` | sub-millisecond |

The cheap calls (`summary`, `schemas`, `preview` on relationship
tables, `header`) are the agent's friend — call them liberally to
plan before you spend on the lazy layers.

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
