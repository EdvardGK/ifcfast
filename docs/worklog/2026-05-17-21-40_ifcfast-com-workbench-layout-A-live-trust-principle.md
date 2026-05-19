# Session: Layout A live on ifcfast.com + trust-first principle codified

## Summary

Shipped the 4-tile QTO workbench (layout A from `/dev/workbench`) as the
main demo on ifcfast.com. Multiple deploys converged through aggressive
back-and-forth on what an ifcfast demo can honestly claim. The defining
output is not the UI itself but the codified principle: **no silent
drops** — every entity / row / tile / mesh / measure renders, even when
null, with explicit attribution. Saved as a feedback memory.

## Changes

### `inbox/ifcfast-site` (Next.js — main work)
- **`app/page.tsx`**: replaced `ThreeLensSection` (3-col viewer + 2-col tabbed data) with the 4-tile 2×2 workbench at 780px tall on lg, ≥420px per tile on mobile. Same narrow-band wrapper kept.
- **`app/dev/workbench/page.tsx`**: 5-layout switcher (A–E) for dev.
- **`app/dev/workbench/dash-tile.tsx`**: KPIs + treemap + materials bar, all source-aware, scope-respecting. Replaced hand-rolled squarified treemap (silently dropped tiles when remaining rect hit zero) with `d3.treemap`. Treemap sizing now uses `layoutValue` (floor for null tiles) so no-data entities still render as muted hatched "no data" tiles.
- **`app/dev/workbench/findings.ts`**: structural finding categories + new "wrapper-with-body" finding (catches IfcRoof↔IfcSlab pattern); spatial-orphan now respects voids edges.
- **`components/ifc-palette.ts`** (new): formula-driven palette, memoized `rangePalette`/`stableEntityPalette`, plus `COLOR_CALLOUT = #1d8a8a` (teal — complement to rust accent) used everywhere for selection state. Severity colors in the rust family — no traffic-light scheme.
- **`components/selection-context.tsx`**: `source` field added per kind + new `kind: "instance"` + `toggleInstance(guid, opts)`. Source-aware filtering means widget that triggered selection stays full; others adapt.
- **`components/vector-graph.tsx`**: node click → `toggleInstance` (shift-click for entity-class fallback); `isMatch` handles all kinds; selected nodes get 1.6× radius + callout stroke; new `void` link kind (rust-dotted) for IfcRelVoidsElement edges; stable colors via `stableEntityPalette`.
- **`components/qto-panel.tsx`**: scope-aware across all selection kinds (was storey-only); added Findings sub-tab; added m³/m²/m + IsExternal/LoadBearing/FireRating columns with `BoolBadge` tri-state helper; layer-set rows still expand.
- **`components/findings-view.tsx`**: shrunk from 12 hardcoded "ifcfast bug" findings to 2 capability-gap notes after we decided value-judgments don't belong here; severity colors from palette; callout-soft for selected row.
- **`components/viewer.tsx`**: `instance` kind handler; ACCENT swapped to teal callout.

### `inbox/ifcfast` (Rust + Python)
- **`scripts/generate_sample_sidecars.py`** (new, ~350 lines): comprehensive sidecar emitter. Includes psets/quantities/materials/classifications/per-product mesh stats/spaces (sibling collection)/voids edges/typedness/buildings/sites/projects. Aggregate-rollup pass joins descendant mesh stats to wrapper entities (IfcRoof gets m³ from its aggregated IfcSlab). Provenance section explicit about which fields came from ifcopenshell shim vs ifcfast core.
- **`crates/core/src/mesh/mod.rs`**: removed silent `IfcOpeningElement` skip (line 93–98). Openings now ship in the GLB; rationale comment kept explaining the overlap until boolean subtraction lands.
- Rebuilt `ifcfast-mesh` release binary, regenerated `duplex.glb` (500 KB) + all sidecars.

### `inbox/ifcfast` (docs)
- **`README.md` + `AGENTS.md`**: earlier tone-down — "fastest" superlative dropped, "When ifcfast fits" framing, `ifcopenshell` as complementary not competitor.
- **`EdvardGK/sprucelab#16`** (live GH edit): partnership proposal rewritten — dropped "category move" framing, dropped Solibri-vs-Sprucelab tagline, dropped "OOM on 8 GB box" jab, kept the numbers.

### Memory
- **`feedback_no_silent_drops.md`** (new): codifies the trust-killer pattern + how to apply across sidecars, dashboards, mesh, parser surfaces.

## Technical Details

**The trust principle** crystallised through multiple instances of the same anti-pattern this session:
1. `ifcfast-mesh` silently skipping `IfcOpeningElement` ("opening_skipped" stat never surfaces)
2. ifcfast indexer silently filtering `IfcSpace` from products (sibling collection only)
3. Sidecar emitting `0` instead of `null` when mesh wasn't extracted (looks like a real zero measurement)
4. `page.tsx` footer claiming "no body representation" as cover story for unimplemented `IfcBooleanClippingResult`
5. DashTile treemap `.filter(value > 0)` dropping 6 entity classes when sized by m³/m²
6. IfcRoof showing null QTO despite the slab it aggregates having real m³ (attribution gap)

Each one looks small in isolation. Together they form a credibility-loss pattern the user named explicitly: "trust killer". Fix is always the same shape — render with explicit attribution, never silently filter.

**Aggregate rollup pattern**: discovered by direct data inspection. IfcRoof in the Duplex carries no body representation itself; its aggregated IfcSlab does (m³=61.76). Generator now computes per-guid descendant rollup and exposes `m3_direct` + `m3` (effective = direct OR rollup) + `m_source` field. DashTile sums from products directly using `m3` for tile sizing. Avoids double-counting in totals because consumers can pick `m3_direct` when summing across the whole model.

**Solibri ITO comparison** (unfiltered): every shared entity matched on count perfectly. ifcfast genuinely fast at parsing. Gaps quantified: 6 entity classes with no mesh, 2-4× area inflation on classes that DO mesh (no opening subtraction). These map cleanly to Rust Phase 3 work.

## Next

1. **Phase 3 (Rust mesh)**: implement `IfcBooleanClippingResult` + `IfcRelVoidsElement` subtraction + `IfcCsgSolid` + `IfcAdvancedBrep` in `crates/core/src/mesh/`. This is the chunk that finally removes the 6 capability findings.
2. **Phase 2 (Rust core)**: promote `IfcSpace` from `EntityKind::Space` sibling collection to full `Product` so the ifcopenshell typedness/voids enrichment in the generator becomes optional.
3. **Phase 4 (Per-product QTO struct)**: extend `ProductStats` with per-face area, footprint, length/width/thickness, profile area, layer-resolved volumes.
4. **Landing page audit**: hero, features grid, benchmark table, code showcase — check every claim against what ifcfast actually does today. We toned down `README.md`/`AGENTS.md` but the landing copy still implies broader parity than reality.
5. **Capability matrix on the site**: explicit table of done vs WIP. Same data the Findings tab surfaces, bigger.

## Notes

- Tasks #9–11 (open) track the Rust phases above. All multi-session.
- The "trust first → love → loyalty" sequencing the user set as project north star applies to every future change: trust is the gate.
- Vercel deploy is `npx vercel --prod --yes` from `inbox/ifcfast-site/`. Last successful URL: `ifcfast-site-fdrmb2upw-spruceforge.vercel.app`, aliased to `ifcfast.com`.
- The d3-hierarchy treemap replacement was a useful lesson: hand-rolled layout algorithms hide bugs in edge cases that battle-tested libs handle. Reach for the lib when the math gets hairy.
- One genuine confusion early on: I built the workbench inside `sprucelab` for ~2 hours before the user redirected to `ifcfast-site`. Recovery clean. Lesson: when language gets prescriptive ("4 equal tiles", "treemap"), confirm the target surface explicitly instead of inferring from a stale anchor.
