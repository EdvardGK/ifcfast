# Session: QTO tabs (Types / Untyped / Materials / Layer sets), viewer-ready glTF, storey-scoped clicks

## Summary
Closed the last IfcBeam mesh-coverage gap on Duplex (composite-curve
profiles), pulled the demo's Python glTF post-process into the Rust
emitter (per-product PBR materials + Y-up root), and rebuilt the QTO
panel from a single IfcEntity list into four tabs (Types / Untyped /
Materials / Layer sets) all grouped by entity. The three lenses now
split cleanly by their natural filter axis: Spatial = entity-by-storey,
Vector = entity/storey, QTO = type/material — and ifcfast-mesh's
output is drop-in viewer-ready with no Python post-processing.

## Changes

### `ifcfast` (Rust core)
- `crates/core/src/mesh/profile.rs` — `curve_to_polyline` now handles
  `IfcCompositeCurve`: walks each `IfcCompositeCurveSegment`,
  recursively resolves the `ParentCurve`, honours `SameSense=.F.` by
  reversing, stitches segments (dedup endpoint joins, drop closing
  duplicate). Unblocks IfcBeam + any IfcArbitraryClosedProfileDef
  whose `OuterCurve` is a composite curve.
- `crates/core/src/mesh/gltf.rs` — emits one PBR material per product
  (`name = guid`, entity-aware default colour palette, translucent
  for IfcSpace + IfcOpeningElement); adds a `ifcfast_root` node with
  a −90° quaternion about X so the BIM-native Z-up scene reads
  Y-up per the glTF spec. Scene references only the root.
- Coverage on Duplex sample: **207 → 282 products meshed** (8 beams,
  13 coverings, 4 railings, 1 wall, etc.).
- Commit `0a658ff` on `EdvardGK/ifcfast@main`.

### `ifcfast-site` (Next.js demo)
- `components/selection-context.tsx` — selection model now carries
  five `kind`s: `entity` (with optional `storey_guid`), `storey`,
  `type`, `material`, `layer_set`, `untyped`. Storey-scoped entity
  selections come from the spatial-tree type-node click.
- `components/graph-view.tsx` — type nodes inherit `storey_guid` from
  their parent storey; `toggleEntity` now passes that scope so
  clicking "IfcWallStandardCase under Roof" highlights only roof
  walls (not all 56 walls in the model).
- `components/viewer.tsx` — `applySelection` matches by entity
  (storey-scoped), storey, type (`type_name`), material (set membership),
  layer-set, and untyped (`typed === false`).
- `components/qto-panel.tsx` — replaced the IfcEntity list with four
  tabs (Types / Untyped / Materials / Layer sets). Rows in every tab
  are grouped under an `IfcEntity` heading; the heading is itself
  clickable (`toggleEntity`) and individual rows filter by type /
  material / layer-set. Sub-label changes per tab so the IFC
  semantics are explicit ("not related to an IfcTypeObject" for
  Untyped, etc.).
- `public/sample/duplex.glb` — regenerated from the new ifcfast-mesh
  binary (282 products, 282 materials, Y-up root). No Python
  post-process step.
- `public/sample/duplex.graph.json` — added per-product
  `typed: bool`, `type_name: string`, `type_source: "ifctype" |
  "objecttype" | "none"`, `materials: string[]`, `layer_set: string | null`.
  Backed by a quick ifcopenshell walk that unwraps
  IfcRelDefinesByType + IfcRelAssociatesMaterial
  (incl. IfcMaterialLayerSet[Usage], IfcMaterialProfileSet,
  IfcMaterialList, IfcMaterialConstituentSet).
- `app/page.tsx` — footer status note rewritten now that Beam/
  Covering/Railing/Wall mesh; what's left is IfcRoof + IfcStair,
  which have **no body representation** in the source IFC.
- Three production deploys via `vercel deploy --prod` to ifcfast.com.

## Technical Details

### Why composite curves
Duplex's beams use `IfcArbitraryClosedProfileDef` whose `OuterCurve` is
an `IfcCompositeCurve` of 2-point `IfcPolyline` segments tracing the
W-shape outline. The previous `curve_to_polyline` returned `None` for
composite curves ("Phase 1A"), so beam extrusions fell out as
`item_unhandled`. Same pattern: covering, railing, wall, member —
all use composite outer curves on this file. Fixing it in `profile.rs`
unlocks every one of them in a single change.

### Why move materials + rotation into ifcfast-mesh
The demo had been baking these in a Python `pygltflib` post-process
that wasn't checked in. That collapsed the "drop any IFC in, get the
view" pitch into "drop any IFC in, then run this missing script." The
user's framing — *core stays data-only, presentation lives in a
sibling module* — fit cleanly: ifcfast-mesh is already a separate
binary/module in the same crate. Materials and Y-up are presentation
concerns that belong there, not in the parser. The door/window 1.003×
scale-boost (z-fight workaround) stays a demo-only post-process
because the real fix is boolean subtraction (ifcfast#4).

### Three lenses, three axes
The user's clean refactor of the panel architecture: IfcEntity is a
*container* (already filtered by the model tree under storeys, and by
the vector graph). What you actually count is a *type* or a
*material*. So:
- Spatial tree → entity by storey
- Vector graph → entity / storey
- QTO         → type / material / layer-set
The "Untyped" tab is a state, not a list — products without an
`IfcRelDefinesByType` link, regardless of what their `ObjectType`
string looks like. Rows in the Untyped tab roll up by `ObjectType`
(Revit's type-by-name pattern) so you can still drill into kinds.

### Revit type-id vs instance-id observation
`IfcXxx.ObjectType` on the Duplex walls carries a Revit family-type
id suffix (`:128360`), shared across all instances of that type.
`IfcXxx.Tag` (and the suffix on `Name`) is the per-instance Revit
element id. So 18 walls genuinely share one `ObjectType` string —
they're all instances of one type, even though Revit didn't bother
to emit an `IfcWallType` + `IfcRelDefinesByType` link.

## Next
- IfcSurfaceStyle → glTF PBR material colours in `ifcfast-mesh`
  (`ifcfast#3`). Replaces the entity-default palette with the
  authored colours. The plumbing is now in place — just needs the
  style walk in `gltf.rs`.
- Boolean subtraction of `IfcOpeningElement` from host walls/slabs
  (`ifcfast#4`) so the 1.003× door/window scale-boost demo hack can
  go away.
- Backport `typed` / `type_name` / `materials` / `layer_set` into
  `ifcfast` Python so the data layer is canonical, not Python-script
  scaffolded next to the sample.
- Ship a real generator script (or `ifcfast` CLI subcommand) for
  `duplex.{summary,types,qto,graph}.json` so the sample assets stop
  being one-off Python one-liners.

## Notes
- Push convention bedded down: this user only tests on the live
  deployed URL, never localhost; deployed three production builds
  during the session after that became clear. Memory saved in
  `feedback_test_on_deployed.md`.
- Cross-scope GH-issue convention also saved
  (`feedback_gh_issues_cross_scope.md`): title
  `<from-scope> -> <to-scope>: subject` so different agents/machines
  can hand off durably.
- Preview deploys are SSO-gated on this Vercel team, so they're not
  useful for user verification — production is the only viewable
  target.
