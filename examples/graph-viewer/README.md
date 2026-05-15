# Graph viewer

A standalone HTML viewer for the spatial decomposition graph exposed by
ifcfast 0.1.0:

- **Type browser** (left) — types grouped by IFC class, instance counts,
  search. Uses `IfcRelDefinesByType` when `ifcopenshell` is available,
  falls back to name-based clustering otherwise.
- **Force-directed graph** (centre) — one node per IFC product + the spatial
  crown (storey, building, site, project). Edges are `IfcRelAggregates`
  (solid) and `IfcRelContainedInSpatialStructure` (dashed). Click a node
  to inspect it; click a type to filter the graph to its instances.
- **3D preview + info pane** (right) — top half is a "weapon-select" style
  isolated preview of the selected instance (camera framed and turntable
  rotating, like a product preview). Bottom half is the property /
  quantity / material / classification / decomposition detail.

Everything is inlined into a single HTML file — D3 and Three.js come from
CDNs, but the model data and the optional GLB are embedded. No server
required; open the file directly.

## Run

```bash
pip install ifcfast ifcopenshell        # ifcopenshell is optional

python build.py path/to/model.ifc        # writes graph_view.html
python build.py model.ifc --out my.html
```

With no arguments it renders `tests/fixtures/minimal.ifc` from the repo
so you can see the layout end-to-end.

## 3D preview (optional)

The preview pane needs the `ifcfast-mesh` binary, which lives in the
upstream [`EdvardGK/ifc-workbench`](https://github.com/EdvardGK/ifc-workbench)
repo (not yet shipped with the ifcfast PyPI wheel). Build it once:

```bash
git clone https://github.com/EdvardGK/ifc-workbench
cd ifc-workbench/crates/ifcfast
cargo build --release --bin ifcfast-mesh --no-default-features --features mesh
```

Then point `build.py` at the resulting binary:

```bash
python build.py model.ifc --mesh-bin ../ifc-workbench/crates/ifcfast/target/release/ifcfast-mesh
```

If `ifcfast-mesh` is on your `PATH`, `build.py` finds it automatically.
Use `--no-mesh` to skip the 3D preview entirely.

## What's in the data layer

Every product node carries:

| field            | source                                   |
|------------------|------------------------------------------|
| `entity`, `name`, `predefined_type`, `object_type`, `tag` | `ifcfast.open(...).filter()` |
| `step_id`        | `ProductRow.step_id`                     |
| `parent_guid`    | unified aggregation graph                |
| `storey_guid`    | storey-relating `IfcRelContainedInSpatialStructure` |
| `mode`           | `ifcfast.classify` element-mode policy   |
| `psets`          | `model.psets`                            |
| `qtos`           | `model.quantities`                       |
| `mats`           | `model.materials`                        |
| `cls`            | `model.classifications`                  |
| `type_id`        | `IfcRelDefinesByType` or name-stripped fallback |

Type-level info is computed in `build.py`:

- **Materials (union)** across all instances of the type
- **Common props** — `(pset, prop)` pairs that exist on every instance with
  identical values. These are effectively type-level data carried per-instance.
- **Varying props** — `(pset, prop)` pairs that vary across instances, with
  either a "missing on N inst" count or a "distinct values" count.

## Output

`graph_view.html` is fully self-contained. On a 1.6 MB IFC with ~1,400
products and the optional 3D preview, the resulting HTML is ~4 MB
(d3 + three.js stay external; only model data + GLB are inlined).
