# Session: ifcfast.com — three-lens viewer, cross-filter, infinite canvas, mesh fix

## Summary

Built out ifcfast.com from a static landing into a working interactive
demo: 3D viewer + tabbed data panel (QTO / Spatial tree / Vector
canvas) with full cross-filter wired through a `SelectionContext`.
Per-product GLB materials made the viewer-side filter granular —
clicking an entity OR a storey in any panel highlights matching
products in accent orange and ghosts the rest. Diagnosed and partly
fixed the mesh-coverage gap: `IfcFaceBasedSurfaceModel` was being
routed to a handler that rejected its inner `IfcConnectedFaceSet`,
silently dropping every covering and railing. Fix unlocked an extra
67 meshed products on the Duplex demo IFC (+ 15K triangles).

## Changes

### `ifcfast` (Rust, mesh crate)

- `crates/core/src/mesh/brep.rs` — relaxed `closed_shell` to accept
  `IfcConnectedFaceSet` (same shape as `IfcClosedShell`/`IfcOpenShell`
  — `CfsFaces: LIST OF IfcFace` at attribute 0, since shells are
  specialised connected-face-sets per the schema). Added
  `face_based_surface_model` and `shell_based_surface_model` that
  walk the FBSM/SBSM outer list and union the inner face-set meshes.
- `crates/core/src/mesh/mod.rs` — dispatch now routes
  `IFCFACEBASEDSURFACEMODEL` and `IFCSHELLBASEDSURFACEMODEL` to the
  new walkers (was bundled into `closed_shell`, which immediately
  bailed because the type guard didn't match).
- Filed `EdvardGK/ifcfast#5` (mesh coverage gap, was launch-blocker
  per partnership #2/#16). Diagnostic ifcopenshell run confirmed:
  - `IfcCovering` / `IfcRailing` use `IfcFaceBasedSurfaceModel`
    → fixed this session.
  - `IfcRoof` / `IfcStair` have 0 representations in Duplex — not a
    bug, just empty types (Duplex uses `IfcSlab[predefined_type=ROOF]`
    and stair flights instead).
  - `IfcBeam` uses `SweptSolid`/`IfcExtrudedAreaSolid` (same primitive
    as walls/slabs which render fine) — still doesn't mesh, separate
    bug, not fixed this session. Tracked in #5.

### `ifcfast-site` (Next.js, brand-new repo this session)

- `app/page.tsx` — single-page landing. Hero with animated typewriter
  terminal, features grid (6 cards), animated benchmark chart,
  three-lens demo section, MCP install widget (Claude Desktop / Cursor
  / Claude Code / VS Code), code showcase, footer.
- `components/viewer.tsx` — `<model-viewer>` wrapped with diorama-
  themed backdrop (warm-clay-to-pale-sky radial gradient + inset
  vignette). Per-product material cross-filter via `model.materials`
  API: selection sets matching materials to accent-orange OPAQUE,
  non-matching to alpha-0.03 BLEND. New ghost on/off toggle: when
  off, `IfcSpace` and `IfcOpeningElement` (translucent-by-nature)
  hide entirely, and filter-non-matching go to alpha 0 instead of
  dim (isolation mode).
- `components/qto-panel.tsx` — type rollup with per-row click-to-
  filter and storey scoping (when a storey is selected, rows
  recompute from products in that storey). Dual quality-flag dots
  per row: orange for no-geom, muted for untyped. Summary banner
  shows global counts.
- `components/graph-view.tsx` — Spatial tab; conventional expand/
  collapse model tree (project / site / building / storey / type /
  product), monospace, BIM-tool conventions. Click a row → cross-
  filter. Dimmed branches when filter active.
- `components/vector-graph.tsx` — Vector tab; infinite SVG canvas.
  All nodes (project + site + building + storeys + per-storey
  type-bins + individual product dots) laid out once in a radial
  layout; positions then live in state so users can drag any node
  to reposition. Background drag pans, wheel zooms around cursor,
  reset/+/− buttons in the corner. Selection just highlights/dims
  — never resets the layout.
- `components/data-tabs.tsx` — tabbed wrapper used by the side
  panel. Initial implementation used render-function props which
  blew up Next 16's server-side serialization; reshaped to children-
  by-index with display:none toggling.
- `components/selection-context.tsx` — shared cross-filter state.
  Three kinds: null / entity / storey. Click toggles, second click
  on same item clears.
- `public/sample/duplex.glb` — bundled demo asset, post-processed
  with `pygltflib` to: (1) wrap all scene roots in a Z-up → Y-up
  rotator node, (2) assign one PBR material per product GUID
  named with the GUID so the viewer can address products
  individually, (3) scale doors/windows by 1.003 to win the depth
  test against gross wall geometry (the proper fix is boolean
  subtraction — tracked at ifcfast#4). Source IFC:
  buildingSMART community sample (CC BY 4.0), attribution in
  `public/sample/ATTRIBUTION.md`.
- `public/sample/duplex.{summary,types,qto,graph}.json` — pre-baked
  data assets produced via `ifcfast.open()` + ifcopenshell for the
  quality flags (no_geometry + untyped per entity).

### Vercel

- Project `ifcfast-site` in the `spruceforge` Vercel team. Deployed
  many revisions during the session. Production alias is
  `https://ifcfast-site.vercel.app` (public; preview URLs have SSO
  protection enabled by the team).
- Custom domain `ifcfast.com` added; user must change GoDaddy
  nameservers or add an A-record `76.76.21.21` to flip live.

### Public-facing scrub

- Replaced all real project names (Sannergata / LBK_ARK_C / ST28_RIV)
  in `README.md` / `AGENTS.md` / `CHANGELOG.md` / sprucelab issue #16
  / ifcfast issue #2 / two session worklogs with anonymised file-
  shape descriptors. Public surfaces are now clean.

## Technical Details

The infinite-canvas Vector graph was the trickiest piece. Initial
attempt re-laid-out the entire graph on every selection change
(focus-storey mode, focus-entity mode, default mode), which gave a
"slide-show" feel — user wanted persistence. Refactored to compute
the layout once at data-load time, then store node positions in
React state. Drag handlers (`onPointerDown` on a node) capture the
pointer, compute layout-coordinate deltas from screen-pixel deltas
via the SVG's `viewBox` ratio, and update the node's position in
state. Click-vs-drag disambiguation by tracking total pointer
movement: <3 pixels = click, ≥3 = drag.

Wheel zoom needed a non-passive event listener (`addEventListener
('wheel', ..., { passive: false })`) bound on the wrapper div so
`preventDefault()` stops the page from scrolling. React's synthetic
`onWheel` is passive by default — wouldn't have worked. First
implementation bound to the SVG ref and got swallowed by some
parent scroller; moving to the wrapper div fixed it.

For the viewer cross-filter, `<model-viewer>` doesn't expose per-
node visibility, only `model.materials`. Solution: bake one material
per product GUID at GLB post-process time, store the GUID as the
material's `name`. On the JS side fetch a `guid → {entity,
storey_guid}` lookup from the same graph.json the panels use,
then on selection iterate materials and decide each one's fate.
Cost: GLB grew from ~200 KB (10 shared materials) to ~515 KB (272
unique materials) on the Duplex demo. Trivial — the win is granular
per-instance filtering through a public-API ceiling.

Diorama mood for the viewer backdrop came after a feedback loop:
black bg → user said "too dark"; warm cream gradient → user said
"building blends with bg". Final: radial gradient anchored at center-
bottom going warm-clay → pale blue-grey, with an inset shadow at
top and bottom for vignetting. Reads like a museum architectural
model. Environment-image="neutral" + tone-mapping="commerce" + a
1.2 shadow intensity completes the staged-render feel.

Accent color cycled red → orange (`#e07c2f`) at user request — works
better with the wall-cream palette.

The "quality flags" feature came out of a reframe: rather than
chasing rendering for `IfcRoof` (which has no geometry in Duplex)
and `IfcStair` (which is a header-only assembly), surface those as
data-quality signals. ifcopenshell diagnostic walks `Representation`
+ `IfcRelDefinesByType` per entity, emits per-entity flags
(`no_geometry`, `untyped`) and a global summary. Two-dot indicator
in the QTO panel (orange = broken, muted = informational).

## Next

1. **Fix IfcBeam mesh** — uses standard `IfcExtrudedAreaSolid` so it
   should render like walls. Probably an entity allowlist or
   extrusion edge case (the diagnostic showed an
   `IfcArbitraryClosedProfileDef` profile, but slabs use the same).
   Need to add a debug print in `mesh::mesh_ifc` to see what status
   the 8 beams land at. Last gap before mesh-coverage is launch-ready.
2. **DNS for `ifcfast.com`** — user to flip GoDaddy A-record to
   `76.76.21.21` or nameservers to `ns{1,2}.vercel-dns.com`.
   Vercel auto-issues SSL after.
3. **IfcSurfaceStyle → glTF PBR** (ifcfast#3) — get colours from the
   IFC instead of the Python post-process palette. Long-term fix.
4. **Boolean subtraction** (ifcfast#4) — fixes the door z-fighting
   properly. Currently scaling doors/windows by 1.003 as a hack.
5. **Storey filter still doesn't work for the spatial / vector views
   completely** — entity-only highlighting in spatial tree on a
   storey selection sometimes leaves wrong branches lit. Review the
   `isMatch` for storey kind in `graph-view.tsx`.

## Notes

- Big tactical lesson: when most user-visible feedback is about UI
  polish (color, layout, hover state), the diagnostic IS the feature.
  The "data quality flags" pivot — from "fix all entities to render"
  to "flag what's empty / untyped" — was the user's idea, and it's
  more useful than mesh coverage alone.
- Next.js 16's Turbopack build refuses to serialize function props
  passed to client components even if both ends are client. Reshape
  to children + index pairing. Documented inline in `data-tabs.tsx`.
- `<model-viewer>` (Google's web component) is the right call for
  "I want a 3D viewer with zero three.js setup". 50 KB, declarative,
  exposes a Materials API that's enough for entity-level cross-
  filter when you bake per-product materials. For per-node tricks
  (highlight individual hovered product) you'd need to drop down to
  three.js + GLTFLoader; not worth it for v1.
- Duplex_A bundled stats: schema IFC2X3, 268 products, 13 storeys,
  CC BY 4.0, ~2.4 MB. Fetched from
  `https://media.githubusercontent.com/media/buildingsmart-community/
  Community-Sample-Test-Files/main/IFC%202.3.0.1%20(IFC%202x3)/
  Duplex%20Apartment/Duplex_A_20110907.ifc`.
- The mesh fix touched the *Rust* core directly (the second time
  this week), but isn't yet released on PyPI. Site demo runs the
  local-built `ifcfast-mesh` binary against the demo IFC; user-
  install of `ifcfast` from PyPI is still v0.1.0.
