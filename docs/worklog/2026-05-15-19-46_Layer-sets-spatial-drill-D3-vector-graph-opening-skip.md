# Session: Layer-set composition · spatial drill-down · D3 vector graph · opening-skip + door scale-boost

## Summary
Picked up immediately after the prior worklog: QTO panel grew the
"Layer sets" tab to show the actual layered composition (materials
+ thicknesses) under each set, the spatial tree now drills row-click
all the way down to individual product instances, the Vector tab
became a real D3 force-directed graph (edkjo's `examples/graph-viewer`
template stripped down to just the graph), the whole graph palette
was re-tuned to the site's muted cream/rust theme, and the
ifcfast-mesh emitter learned to skip `IfcOpeningElement` and
scale-boost doors/windows 1.003× as interim z-fight remedies. Five
production deploys to ifcfast.com during the session.

## Changes

### `ifcfast` (Rust core)
- `crates/core/src/mesh/mod.rs`:
  - **Skip IfcOpeningElement at mesh time.** It's a void definition
    per IfcRelVoidsElement, not a renderable solid. Rendering it
    produced a third overlapping translucent layer on top of the
    wall + door/window z-fight. Counted as `opening_skipped` in
    `by_source` so the stat is still visible. −50 products on Duplex,
    no visual loss.
  - **Door/window 1.003× scale-boost** about the product's placement
    origin (recovers the Python post-process hack that was lost when
    GLB generation moved to pure Rust). Doors and windows now win
    the depth test reliably against wall surfaces inside their
    openings. Temporary — goes away when real boolean subtraction
    (ifcfast#4) lands.
- Coverage on Duplex: 282 → **232 products meshed** (the drop is the
  50 openings; everything previously visible is still visible).
- Pushed `9c8c784` to `EdvardGK/ifcfast@main`.

### `ifcfast-site` (Next.js demo) — five prod deploys to ifcfast.com
- `components/qto-panel.tsx` — Layer sets rows show `N layers · NNN mm`
  alongside the name, expandable ▸/▾ caret reveals the actual layered
  composition (material name + thickness mm per layer, indented).
  Click on the row body still toggles the layer-set filter; the
  caret expand toggle is independent.
- `components/graph-view.tsx` — row click on storey/type now both
  *filters* AND *ensures expanded* (was previously filter-only; you
  had to find the chevron). Drill goes all the way to individual
  product instances now. Lifted the 50-instance-per-type cap (the
  Duplex heaviest is 24; the cap was ahead of its time).
- `components/vector-graph.tsx` — complete rewrite from the
  hand-built SVG infinite-canvas radial layout to a D3 force-directed
  simulation (`forceSimulation + forceLink + forceManyBody +
  forceCenter + forceCollide`). Pan/zoom via `d3.zoom`, drag-to-pin
  via `d3.drag` with fx/fy, tooltip on hover. Cross-filter wired via
  the existing selection-context (storey node → toggleStorey,
  product node → toggleEntity scoped to its storey, container/bg
  click → clear). Selection re-application on data-attr change via
  MutationObserver, so filter changes don't rebuild the simulation.
  ResizeObserver re-centres when DataTabs reveals the tab (it was
  zero-sized at mount because of `display: none`).
- Adapted from edkjo's `examples/graph-viewer` template
  (`EdvardGK/ifcfast@examples/graph-viewer`); intentionally dropped
  the side type-browser, search input, legend bar, and info pane —
  just the graph.
- **Muted palette to match the site theme** — the dark-mode vibrant
  colours from edkjo's template fought the cream/rust theme. New
  palette: containers (project → site → building → storey) deepen
  in browns from walnut to clay; element classes share a cool
  gray-green family (sage, steel, stone); selected stroke uses the
  site accent `#e07c2f`. Link colours retuned to the line family.
  Container labels in near-fg dark warm gray.
- `public/sample/duplex.graph.json` grew top-level
  `material_layer_sets: { name → { layers: [{material, thickness_mm}],
  total_thickness_mm } }` (15 sets, 124 mm—550 mm depths).
- `public/sample/duplex.glb` regenerated after the opening-skip +
  door-scale change.
- `package.json` + `package-lock.json` — added `d3` + `@types/d3`.

### Conversation milestones
- The user clarified: **only tests on the deployed prod**, never
  localhost. Five production deploys this session as a result.
- The user clarified the architectural boundary: **core stays
  data-only; presentation lives in modules** (ifcfast-mesh is the
  presentation module). Materials + Y-up belong there, not in the
  parser.

## Technical Details

### Why IfcCompositeCurve mattered (carry-over from prior session)
Beams + coverings + railings + walls all hit
`curve_to_polyline → None` because their profile's `OuterCurve` was
`IfcCompositeCurve`. Fixing that one branch in `profile.rs`
unblocked every entity that uses `IfcArbitraryClosedProfileDef`
with a composite outer.

### Type-vs-Untyped distinction in the QTO panel
The user surfaced an IFC nuance worth recording: "Untyped" in the
formal IFC sense means **no IfcRelDefinesByType link**, regardless
of whether the product carries a type-looking name string. Revit
exports walls with `ObjectType = "Basic Wall:Interior - Partition
(92mm Stud):128360"` — a string with a Revit family-type id
suffix, all instances of a type sharing that string — but no
`IfcWallType` + `IfcRelDefinesByType` relation. So formally untyped.
The panel surfaces this honestly: rows group by `ObjectType` string,
counts stay correct, the Untyped tab description says "not related
to an IfcTypeObject."

### Storey-scoped entity selection
Spatial tree clicking on a type node (e.g. IfcWallStandardCase
under Roof) now passes the parent storey's GUID through, so the
viewer + QTO panel both scope to that storey instead of selecting
every wall in the model. Old behaviour was a real cross-filter bug.

### D3 in a hidden-then-revealed container
DataTabs hides inactive tabs with `display: none`, so the Vector
graph's wrapper has zero dimensions at mount. The d3 simulation
needs a numeric centre. Fallback: clamp the initial size to
400×400 minimum, then a ResizeObserver re-centres the force when
the tab actually shows.

### Selection cross-filter without rebuilding the sim
The graph is rendered once per data load. Selection changes don't
modify the node/link arrays — they re-style opacities and strokes
in place. To trigger the restyle from React without re-running the
simulation effect, the wrapper div carries a `data-selection-key`
attribute that React sets, and a `MutationObserver` inside the d3
layer watches that attribute. Cheap, correct.

## Next
- **ifcfast#4 — boolean subtraction.** The user explicitly wants
  this; the door/window scale-boost is a stopgap they recognise.
  Sketch:
  1. Walk `IfcRelVoidsElement` to build host → opening pairs.
  2. For extruded-solid openings cutting planar walls (~95% of
     architectural cases): 2D polygon difference on the host
     profile + re-extrude. Fast, robust, handles the common case.
  3. For arbitrary 3D voids (sloped skylights, circular cutouts in
     non-axis-aligned walls): full 3D mesh boolean via `manifold`
     or `csgrs`. Heavier, but needed for the long tail.
- IfcSurfaceStyle → glTF PBR (`ifcfast#3`) still pending. Replaces
  the entity-default palette in `gltf.rs` with the authored colours.
- Backport `typed` / `type_name` / `materials` / `layer_set` into
  the `ifcfast` Python data layer so `graph.json` doesn't need a
  one-off Python script next to it.
- Real generator script (or `ifcfast` CLI subcommand) for the
  sample's `duplex.{summary,types,qto,graph}.json` assets.

## Notes
- The user's testing flow assumes the live deployed URL; production
  deploys are gated by a local Claude Code permission hook (good
  guardrail — "you haven't pushed in 40 minutes" is not specific
  consent to `vercel deploy --prod`). Memory saved earlier in the
  day at `feedback_test_on_deployed.md`.
- ifcfast-site has no GitHub origin — deploys flow only via
  `vercel deploy --prod` from local. Worth deciding whether to add
  a GH remote (auto-deploy on push) or keep CLI-only.
- IfcRoof + IfcStair appear in the QTO with **no body
  representation** in the Duplex source — not a meshing bug, the
  IFC genuinely ships them without geometry. Footer note on the
  page reflects this.
- edkjo's `examples/graph-viewer` template (`build.py` +
  `template.html`) on the `examples/graph-viewer` branch is the
  reference for the full agent-handoff viewer. We integrated just
  the D3 graph layer; the rest (instance preview, type browser,
  search) is intentionally not in scope for the site.
