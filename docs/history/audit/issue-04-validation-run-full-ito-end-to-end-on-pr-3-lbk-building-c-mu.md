# Issue #4 — Validation: run full ITO end-to-end on PR #3 (LBK Building C, multi-semantic bbox + federated floors)

_Originally filed: 2026-05-11 · status at close: CLOSED_

_Imported from `EdvardGK/ifc-workbench#4` when ifcfast was extracted as a standalone repo._

---

Now that the schema bump (cache v7) + federated floor synthesis + CGAL hybrid kernel + native Rust tier-1 are all on `fastparse-v3-native-rust-tier1` (PR #3), the end-to-end **validation pipeline** needs a real run on the LBK Building C model set to show whether they actually move the headline numbers. Best run on your Windows box — this dev box (8 GB free + thrashing swap) keeps OOM-killing ifcopenshell on the 598 MB MEP file during the tier-2 stage.

## Baseline to beat

From the pre-session worklog (`docs/worklog/2026-05-11-08-50_Bbox-investigation-and-handover.md`):

```
ITO rows joined:        120,953 / 120,953   (100.0%)
IFC Entity:             100.0%
Name:                   100.0%
Predefined Type:        100.0%
Federated Floor:         56.1%   ← Solibri synthesises labels we didn't
Bbox Height (local Z):   92.3%
Bbox Length/Width:       72.4%
Cold parse total:        230 s for 955 MB / 5 models
```

## What should change in this PR

| field | expected delta | why |
|---|---|---|
| Federated Floor | 56% → >90% | `federated_floors` module clusters across all 5 models with the `LBK` rule (`Plan 01` → `C - Plan 1`, `Plan U1` → `Hav`) |
| Bbox `solibri_*` | ≈ 92%/72% (matches baseline) | same V1AB+Solibri-reorder logic, just emitted as a separate column |
| Bbox `local_*` | new metric, expected lower than solibri_* | raw element-local extents, no axis reorder — exposes which framing the ITO oracle uses per element |
| Bbox `world_*` | new metric, expected much lower for rotated elements | world AABB — rotated walls have inflated XY but correct Z |
| Cold parse total | 230 s → ≈ 60-90 s | native Rust tier-1 (~3 s for all 5 files) + CGAL hybrid kernel on tier-2 |

## Reproduction

```bash
# Pull and rebuild
git fetch origin
git checkout fastparse-v3-native-rust-tier1
git pull
cd crates/ifcfast && maturin develop --release && cd ../..

# Run validation
python scripts/validate_against_ito.py \
  --ito data/reference/20260510_ITO_SkiplumStandard_LBK.xlsx \
  --ifc-dir data/samples/LBK_C \
  --project LBK
```

Output lands in `docs/research/<timestamp>_ito-validation.{md,json}` and a per-row mismatch parquet. The markdown report has the headline table.

## What to look at in the output

1. **`Federated Floor` agreement**: should be >90% on the 5 LBK files. If it lands at 100% you'll know the LBK rule covers every storey; if it's ~95% the gap tells us which storey names need overrides added to `LBK_OVERRIDES` in `federated_floors.py`.

2. **`solibri_height` and `solibri_length_width` percentages**: should match the 92.3%/72.4% baseline (the data hasn't changed, just lives in a new column).

3. **`local_*` vs `world_*` deltas**: confirms our intuition that Solibri's ITO oracle uses the local-Solibri-reorder framing, not raw local or world.

4. **`Cold parse total`** in the output header: previously 230 s. Native tier-1 alone is ~3 s for all 5 files; the rest is tier-2 (bbox compute). CGAL hybrid on the iterator path should drop tier-2 a lot vs the prior OCCT-only.

If any of those don't land where expected, drop the per-row mismatch parquet here and I'll dig in.

## Why not run it here

Same OOM signature you hit earlier — this box has ~8 GB free and the 598 MB MEP file's `ifcopenshell.open()` pushes resident set past 7 GB. Exit code 144 (SIGTERM by oom-killer). Native tier-1 alone is fine (mmap + constant memory) but tier-2 bbox still needs the live ifcopenshell handle.

A `compute_bboxes_native_only` mode that skips ifcopenshell.open entirely (parametric extents from STEP refs in Rust) would solve this — that's a future PR, not in scope here.

Tagging since you'd asked about the validation arc in our prior conversation. No rush — flag when you have a window.

---

### Comment by @EdvardGK (2026-05-11)

Local run succeeded after fixing two regressions surfaced by the first attempt. **Final numbers:**

| field | baseline | run 1 | run 2 | run 3 (final) |
|---|---:|---:|---:|---:|
| GUID join | 100% | 100% | 100% | 100% |
| IFC Entity | 100% | 100% | 100% | 100% |
| Name | 100% | 65.3% | 90.1% | **100.0%** |
| Predefined Type | 100% | 99.9% | 99.9% | 99.9% |
| Federated Floor | 56.1% | 0% | 82.8% | **100.0%** |
| solibri_height | 92.3% | 92.3% | 92.3% | 92.3% |
| solibri_length_width | 72.4% | 72.4% | 72.4% | 72.4% |
| cold parse total | 230 s | 167 s | 146 s | 157 s |

`a4510cf` and `c5484ed` are now on the branch.

## Regressions fixed mid-validation

1. **`\X\HH` STEP escape was 5 chars, not 6** (`a4510cf` — lexer.rs)
   ISO-10303-21 short form for Norwegian å/ø/æ in Revit/MagiCAD/Archicad exports is `\X\HH` with no closing backslash. Decoder was requiring 6 chars, falling through, leaving literal escapes in the output. Recovered ~25 pp on the Name column.

2. **`\S\C` STEP escape entirely unhandled** (`c5484ed` — lexer.rs)
   Tekla/Revit emit `\S\E` -> Å, `\S\X` -> Ø for uppercase Norwegian chars. The escaped ASCII char's high bit gets set: `C | 0x80`. Closed the last 10 pp of the Name gap. Visible on `IfcBeam` / `IfcPlate` / `IfcElementAssembly` records mostly (`STÅLSØYLE`, `STÅLPLATE`, etc.).

3. **`lbk_rule` not idempotent** (`a4510cf`)
   `'Hav'` became `'C - Hav'`, `'C - U1'` became `'C - C - U1'`. Added prefix-already-present guard for `'Hav'` and any name starting with `'C - '`. Brought federated_floor 0% -> 83%.

4. **`lbk_rule` only checked most-common cluster name** (`c5484ed`)
   The basement-level cluster contained `Plan U1` (RIE), `C - U1` (ARK + RIVr), `2. C - U1` (RIVv) at ~600 mm elevation. Most-common was `C - U1` (3×) so the rule emitted `C - U1`. But `Plan U1` was in `LBK_OVERRIDES -> Hav`. Rule now scans **every** member of the cluster for an override; one match resolves the whole cluster. Plus expanded `LBK_OVERRIDES` with `C - U1`, `2. C - U1`, `C - bunnarmering`, `C - topparmering`, `Undefined` (RIBp's single-storey export), `HAV`, `1. Hav`. Closed the final 17 pp.

## Multi-semantic bbox columns confirm the framing-choice hypothesis

| dimension | local raw | local Solibri-reorder | world AABB |
|---|---:|---:|---:|
| Height | 62.0% | **92.3%** | 92.2% |
| Length / Width | 48.9% | **72.4%** | 3.2% |

Solibri-reorder is the right framing for the L/W headline (Solibri's choice). Raw local is worse because it doesn't pick the world-vertical axis as Height. World AABB matches Solibri on Height (axis-aligned products) but tanks on L/W because rotated walls inflate their world XY footprint.

The fact that we now emit all three on the same parquet row means a future LCA or QTO consumer can pick the semantic that matches their oracle, instead of being stuck with whichever heuristic the parser hard-coded. That's the v3 "do it better than Solibri" framing the worklog called out.

## When you have your window

The branch is `fastparse-v3-native-rust-tier1` at `d569e04`. Reproduction is unchanged from the original issue body — pull, `maturin develop --release`, run `validate_against_ito.py`. The new validation reports are checked in under `docs/research/2026-05-11-14-{09,20,28}_ito-validation.{md,json}` if you want to diff against your own run.

No rush; closing this issue since the headline question (do the v3 additions move the LBK numbers) is answered: yes, decisively.
