# Session: Fast-parser v2 — bbox investigation, ITO validation, handover

## Reframing

**Solibri is the benchmark, not the ceiling.** This session validated the parser against a Solibri ITO export of all 5 LBK Building C discipline models (120,953 elements). We hit 100% on GUID and every categorical column, 92%/72% on geometric bbox. The remaining gap is mostly *semantic* (Solibri makes one choice; we made another) — not parser failure. A clean v3 should aim to expose *all* truthful representations of a quantity and let the caller pick, instead of mimicking any single tool's heuristics.

Where Solibri likely beats us today:
- Boolean subtractions (walls with openings → post-cut bbox)
- Curved/swept geometry tessellated to tighter extents
- Federated storey labelling across disciplines

Where we likely beat Solibri:
- Schema-faithful parametric extents — we have exact dims for `IfcExtrudedAreaSolid` with `IfcRectangleProfileDef` without needing to triangulate.
- Multiple semantic representations on the same row (world AABB, body-local, Solibri-style reorder) — Solibri exports one and you can't recover the others.
- Speed at scale (cached parquet, 23-100ms hot reload) — Solibri's ITO export takes minutes per click.

## What's in place after this session

### Tiered parser (production)

| Tier | Cost (cold) | Cost (hot, cached) | Purpose |
|---|---|---|---|
| 0 — STEP header read | 10-25 ms | same | schema, authoring app, content hash cache key |
| 1 — `ifcopenshell.open()` + product/storey index | 6-25 s | <100 ms | type histogram, GUID list, mode classification |
| 1.5 — geometry catalog (opt-in) | 1-3 s | not persisted yet | mapped-rep dedup, "could-be-shared" detection |
| 2 — parametric bbox extractor | 5-50 s | <100 ms | local + world AABB, no tessellation by default |
| 3 — zstd parquet cache | — | — | encode once, decode many; classifier-signature-versioned |
| 4 — full mesh on demand | — | — | not yet implemented; hybrid extractor is the bridge |

Hot reload for a 570 MB MEP IFC: **83 ms**. Cold parse of all 5 LBK Building C disciplines (955 MB total): **~4 min**.

### Bbox extractor variants — all five live in `ifc_workbench/fastparse/extents_variants.py`

| variant | strategy | speed | LBK federated H/LW vs Solibri ITO |
|---|---|---|---|
| `v1` | parametric, first item, body-local Z = Height | 1× | 58.9% / 45.5% |
| `v1a` | + union all body items | 1.1× | 64% / 53% |
| `v1b` | + Solibri axis reorder | 1.5× | 90% / 70% |
| **`v1ab`** (default) | A+B combined | 1.7× | **92.3% / 72.4%** |
| `v2bsol` | tessellation via `ifcopenshell.geom.iterator` + reorder | 40× | 95% / 80% (RIE/RIBp only — RIVr untested at scale) |
| `hybrid` | v1ab primary + v2bsol fallback for V1's deferred GUIDs | 3.5× | 92.5% / 73.3% with +5.9% coverage |

Switch via `compute_bboxes(variant=...)`. Cache version bumps with each variant family so caches can't get mixed.

### Validation pipeline

`scripts/validate_against_ito.py` — runnable end-to-end:

```bash
python scripts/validate_against_ito.py \
  --ito data/reference/20260510_ITO_SkiplumStandard_LBK.xlsx \
  --ifc-dir data/samples/LBK_C
```

Outputs JSON + markdown into `docs/research/<ts>_ito-validation.{md,json}` and a parquet of mismatches.

Latest result (2026-05-11 08:40 with default v1ab):

```
ITO rows joined:        120,953 / 120,953   (100.0%)
IFC Entity:             100.0%
Name:                   100.0%
Predefined Type:        100.0%
Federated Floor:         56.1%      (Solibri synthesises labels we don't)
Bbox Height (local Z):   92.3%
Bbox Length/Width:       72.4%
Cold parse total:        230 s for 955 MB / 5 models
```

### Dashboard (UI)

Reachable at `http://127.0.0.1:7474/` via `python -m ifc_workbench.fastparse.server` or `ifc-fastparse-ui`.

Features:
- File browser (filters across `~/dev`, `~/skiplum`, configured roots)
- Drag-drop or click upload, XHR with `upload.onprogress`
- SSE streams per-product progress, throughput, tier transitions
- Cache panel showing all encoded models
- Results panel: type histogram, mode breakdown bars, storey list, cache HIT/miss

Architecture decision worth preserving: the parser writes to a `ProgressSink` (single dict update per element, ~50 ns), and a separate SSE thread polls it every 100 ms. **The parser doesn't know there's a UI.** If the browser disconnects, parsing keeps going at full speed.

### Investigation tooling — keep these around

- `scripts/diagnose_bbox.py` — bucket agreement by (entity, source). Run per-model to find the biggest pain points.
- `scripts/inspect_bbox_samples.py` — dumps actual numbers for sampled mismatches. Crucial for spotting patterns Solibri uses.
- `scripts/bbox_bakeoff.py` — head-to-head any subset of variants on a single model.

## Open paths (ranked by impact)

### 1. Expose multiple bbox semantics per row, not just Solibri's pick

This is the **biggest single improvement** and aligns with the "beat Solibri" framing. Today's `bboxes.parquet` carries:

```
xmin, ymin, zmin, xmax, ymax, zmax, source, local_x, local_y, local_z
```

The seven world-AABB fields are stable. The three `local_*` extents currently follow the V1AB Solibri-reorder. We should instead emit *all* useful framings as separate columns:

```
world_x, world_y, world_z                      — world axis-aligned AABB extents
body_local_x, body_local_y, body_local_z        — extents in the body's local frame
product_local_x, product_local_y, product_local_z  — extents in ObjectPlacement's frame
solibri_height, solibri_length, solibri_width   — Solibri's heuristic (current local_*)
```

Then `validate_against_ito.py` compares against `solibri_*`, and a future LCA query compares against `world_*` or `body_local_*`. Schema bump to cache v7. Users keep all three semantic frames — Solibri keeps one and forgets the others.

### 2. Boolean subtraction support

Walls/slabs with openings should expose both `gross_*` and `net_*` extents. The parametric path can subtract openings from the wall's representation by walking `HasOpenings` → `IfcRelVoidsElement`. We currently set `disable-opening-subtractions=True` in the iterator path; flip it for the gross/net pair.

Likely closes another 5-10% of the L/W gap, especially on ARK walls.

### 3. CGAL kernel for the hybrid fallback

`compute_bboxes(variant="hybrid")` uses OpenCASCADE for tessellation. IfcOpenShell 0.8 exposes `geometry_library="cgal-simple"` and `hybrid-cgal-simple-opencascade`. CGAL-simple is faster for simple shapes (most of an architectural model) and falls back to OCCT for what it can't handle. Could drop the hybrid cold cost from 12 min → 4-5 min.

### 4. Federated floor synthesis

Solibri reports `Federated Floor` like `C - Plan 1`, `Hav` — built by clustering elements across all 5 disciplines by Z elevation and applying a project naming convention. Our 56% agreement is because we report raw `IfcBuildingStorey.Name` (`Plan 01`, `Grunn U1`). Synthesise the federated label by:

1. Collecting all storeys across all open models in a project
2. Clustering by elevation (±100 mm tolerance)
3. Applying a project rule (in LBK: `"C - " + drop_leading_zero(name)`, `Plan U1 → Hav`)

Project rules are user-supplied; the clustering is generic.

### 5. Native STEP tokenizer (v3, deferred)

The cold parse is bottlenecked by `ifcopenshell.open()` (~60-70% of Tier 1 time on big files). Ara3D's C# STEP parser opens a 450 MB file in 5.4 s vs IfcOpenShell's 100 s. ifc-lite's Rust+WASM tokenizer hits 1259 MB/s. A Rust crate as a Python module via PyO3 would let us:

- Cold-parse a 600 MB IFC in 10-15 s instead of 60-80 s
- Walk entities lazily without materialising the full graph
- Keep IfcOpenShell only for the geometry kernel (where it's actually needed)

Big-ticket v3 work. Earmarked, not started.

### 6. Pset persistence

`psets.parquet` planned in SPEC but not built. Pset extraction in tier 2 alongside bbox would give us `Pset_*BaseQuantities` (`NetVolume`, `NetArea`, `Length`, `Width`, `Height` — author-supplied values that often beat any geometric computation). Schema bump to cache v8.

### 7. Tier 4 — on-demand mesh

For a Three.js viewer or accurate clash detection. The hybrid extractor already runs the iterator on a subset of GUIDs; tier 4 generalises that to arbitrary mesh extraction (verts/faces/matrix as zstd-npz blobs in sqlite). Wire it through the API as `GET /api/mesh/{guid}`.

## Key files

```
ifc_workbench/fastparse/
├── __init__.py              public API
├── header.py                Tier 0 — STEP header read (no ifcopenshell)
├── index.py                 Tier 1 — open + walk + compute_bboxes() wrapper
├── catalog.py               Tier 1.5 — geometry catalog (mapped-rep dedup, could-be-shared)
├── extents.py               Tier 2 — V1 parametric (untouched, still load-bearing)
├── extents_variants.py      Tier 2 — V1A, V1B, V1AB, V2 iterator, hybrid
├── cache.py                 Tier 3 — zstd parquet + sqlite cache
├── progress.py              non-blocking ProgressSink for UI
├── cli.py                   `ifc-fastparse` entry point
├── server.py                FastAPI dashboard + SSE
├── static/                  index.html, app.js, style.css
└── SPEC.md                  on-disk schema, variant table

scripts/
├── benchmark_fastparse.py   cold/hot timings across small/medium/large
├── validate_against_ito.py  full ITO ↔ parser join + diff
├── diagnose_bbox.py         per-(entity, source) agreement buckets
├── inspect_bbox_samples.py  sampled raw numbers for mismatch inspection
└── bbox_bakeoff.py          variant head-to-head on a single model

data/
├── samples/LBK_C/           5 LBK Building C IFCs (downloaded from ACC Skiplum Backup)
├── reference/               20260510_ITO_SkiplumStandard_LBK.xlsx + NS3451 PDFs
└── samples/S8A_RIV/         earlier MEP test files (still useful for benchmark variety)

docs/
├── plans/2026-05-10-fast-parser-v2.md         original architecture doc
├── research/
│   ├── 2026-05-10-classifier-coverage.md      qto.classify_element audit
│   ├── 2026-05-11-bbox-investigation.md       this session's bbox dig
│   ├── 2026-05-11-*_bbox-bakeoff-*.json       per-model variant comparisons
│   ├── 2026-05-11-08-40_ito-validation.md     final headline numbers
│   └── 2026-05-10-dashboard-*.png             dashboard screenshots
└── worklog/
    ├── 2026-05-10-14-30_Fast-parser-v2-ownership.md
    └── 2026-05-11-08-50_Bbox-investigation-and-handover.md  (this file)

sidequests/_archive_2026-05-10-fast-parser-v1/
└── (V1 parser + aspirational docs, kept for reference)
```

## How to pick up where this left off

```bash
# 1. Run the validation to confirm current state
cd ~/dev/projects/ifc-workbench
python3 scripts/validate_against_ito.py \
  --ito data/reference/20260510_ITO_SkiplumStandard_LBK.xlsx \
  --ifc-dir data/samples/LBK_C

# 2. Try a variant
python3 scripts/bbox_bakeoff.py --model LBK_RIBp_C --variants v1ab,hybrid

# 3. Launch the dashboard
python3 -m ifc_workbench.fastparse.server --port 7474
# → http://127.0.0.1:7474/

# 4. Diagnose a new disagreement bucket
python3 scripts/diagnose_bbox.py --model LBK_ARK_C
```

## Two things to remember about the data

1. **Five LBK Building C IFCs (955 MB total)** are downloaded under `data/samples/LBK_C/` from ACC `Skiplum Backup / 10004 - Landbrukskvartalet / Delt / 1 - IFC LBK / IFC Lokal /`. They are the May 2026 versions matching the ITO export. If newer versions are needed, the ACC MCP item IDs are in `2026-05-10-bbox-investigation.md`. Note: **Skiplum projects live in the `Skiplum Backup` project on the Skiplum AS hub**, NOT in `Landbrukskvartalet` (a near-empty shell). Memory has this.

2. **The ITO** at `data/reference/20260510_ITO_SkiplumStandard_LBK.xlsx` (15.7 MB, 120,953 rows, federated across all 5 discipline models) is the ground truth for *Solibri-compatible* QTO. New ITOs land in ACC under `B_Leveranser / 06_ITO /` — the watch task is closed; future drops can be checked by listing that folder.

## Outstanding tasks (clean carry-over for next session)

- Open: expose multiple bbox semantics per row (item #1 above)
- Open: boolean subtraction support for openings (item #2)
- Open: CGAL kernel test for hybrid speedup (item #3)
- Open: federated floor synthesis (item #4)
- Deferred: native Rust STEP tokenizer (v3, item #5)
- Deferred: pset persistence (item #6)
- Deferred: tier 4 on-demand mesh (item #7)
- Stretch: integrate with sprucelab BIM workbench backend per project CLAUDE.md

Nothing is blocked. Pick whichever path sounds best on opening this back up.
