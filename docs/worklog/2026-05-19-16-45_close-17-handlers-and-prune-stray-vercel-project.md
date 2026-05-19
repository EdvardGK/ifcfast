## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `ef17aaa` → `49bb165` (three commits: `9c3e2aa`, `38a596f`, `49bb165`)
- **Session scope**: Close out #17's enumerated handler list (7 of 8 types) after the corpus-bias reframe; prune a stray `ifcfast` Vercel project that was failing on every push.
- **Touched paths**: `crates/core/src/mesh/{curveset.rs,csg_primitive.rs,revolved.rs,mod.rs}`, `crates/core/tests/mesh_reveal.rs`, `docs/worklog/2026-05-19-{09-08,10-32,16-45}_*.md`. Plus the deletion of the `ifcfast` Vercel project on the spruceforge org.
- **Parallel sessions observed**: none — `origin/main` advanced only via this session's three pushes.
- **Supersedes / superseded by**: this session extends `2026-05-19-10-32_17-evidence-survey-and-side-findings.md` — that session's "deferred handlers" became this session's shipped work.

## Summary

Session opened with the user reframing: "where are we on becoming the tool we want to be? The site is just a marketing surface and we weren't ready last time." That ended the prolonged site-catch-up deferral and reset focus on the library itself.

The first half-session ran #17's evidence survey on 8 production IFCs. Initial framing: "swept solids and CSG primitives didn't surface in this Revit/Magicad corpus, so they're latent; only `IfcGeometricCurveSet` is the actual live gap." The user pushed back hard and correctly: "We're making this for the industry, not narrow Norwegian scopes." The IFC spec is the backlog, not whatever showed up in `~/Downloads/`. That stance now lives in memory as `feedback_ifcfast_industry_scope`.

Second half-session shipped handlers for **7 of 8 #17 enumerated types**, plus the corpus-found one, across three commits. Tests added for each. CI green on all three pushes. The last remaining latent type is `IfcSurfaceCurveSweptAreaSolid` (profile-along-curve-on-surface) — substantially harder, parked as a follow-up.

Side-track at the end: user reported repeated Vercel build errors. Tracked it down — not `ifcfast-site` (Next.js, builds clean), but a **separate `ifcfast` Vercel project** that was wired to GitHub auto-deploy on the library repo. Vercel autodetected `pyproject.toml`, looked for a Flask/FastAPI entrypoint, failed in ~2 seconds every push. Library has no website to host; project shouldn't exist. Deleted with user approval.

## Changes

- `crates/core/src/mesh/curveset.rs` (NEW) — `IfcGeometricCurveSet` / `IfcGeometricSet`. Walks `Elements`, extracts `IfcPolyline` / `IfcIndexedPolyCurve` / `IfcCartesianPoint` as 3D points. Each line segment becomes a degenerate triangle `(a, b, b)` so the geometry surfaces with zero area. Tag: `curve_set`.
- `crates/core/src/mesh/csg_primitive.rs` (NEW) — all 5 `IfcCsgPrimitive3D` subtypes. Closed-form tessellation, no profile/curve deps:
  - `IfcBlock` → 12 triangles, exact AABB.
  - `IfcRightCircularCylinder` → 24-segment side ribbon + caps.
  - `IfcRightCircularCone` → 24-segment side fan + base.
  - `IfcSphere` → 12 latitudes × 24 longitudes (lat-lon tessellation).
  - `IfcRectangularPyramid` → 6 triangles (base + 4 sides).
  Per-type source tags: `csg_block`, `csg_cylinder`, `csg_cone`, `csg_sphere`, `csg_pyramid`.
- `crates/core/src/mesh/revolved.rs` (NEW) — `IfcRevolvedAreaSolid`. Discretises angle into 4-32 steps, rotates profile boundary around `IfcAxis1Placement` line at each step, emits side ribbons + end caps (skipped when angle ≈ 2π). Reuses `profile::extract` + earcutr for cap triangulation. Tag: `revolved`.
- `crates/core/src/mesh/mod.rs` — module registration, dispatch arms, source-tag list (added: `curve_set`, 5 `csg_*` tags, `revolved`).
- `crates/core/tests/mesh_reveal.rs` — added 5 new tests (curve-set, CSG-primitive-tags, block-dimensions-exact, sphere-tessellation-bracket, revolved-volume-tolerance). Updated the existing `unhandled_representation_appears_as_labeled_bucket` test to use `IfcSurfaceCurveSweptAreaSolid` (was `IfcRevolvedAreaSolid` — which we now handle). All 9 mesh-reveal tests pass.
- `docs/worklog/2026-05-19-09-08_Indexer-18-doorstyle-windowstyle-and-WIP-flush.md` — committed in `72628e3` (was untracked from the prior session).
- `docs/worklog/2026-05-19-10-32_17-evidence-survey-and-side-findings.md` — this session's first half.
- `docs/worklog/2026-05-19-16-45_close-17-handlers-and-prune-stray-vercel-project.md` — this file.
- **External**: `ifcfast` Vercel project deleted from `spruceforge` org (was auto-deploying the library repo on every push, failing with "no python entrypoint" in 1-3s).
- **GitHub**: comment on #17 reporting 7/8 types handled, only `IfcSurfaceCurveSweptAreaSolid` remains latent.

## Technical Details

**Why `IfcGeometricCurveSet` uses degenerate triangles, not real wireframe.** The honest answer is "this should be a `MeshFragment::Wireframe` variant carried through to OBJ `l` directives and glTF `LINES` primitive." That's a bigger plumbing change touching `ProductMesh`, writers, Python bindings, JSON serialisation. The degenerate-triangle workaround preserves the geometry (vertices + connectivity correct, line segments visible as wireframe in viewers that draw triangles as edges) and keeps QTO honest (zero `surface_area` / `volume` contributions). Cost: slightly inflated triangle counts on annotation-heavy ARK files. Worth a separate `MeshFragment::Wireframe` issue.

**Sphere tessellation tolerance.** Initial test had `assert ratio > 0.95` — failed at 0.906 because a 12-lat × 24-lon tessellation *inscribes* the sphere (chords cut inside the great circle) and predictably undershoots. Switched to a bracket `[0.85, 1.0]` that documents the inscribed-tessellation behaviour rather than papering over it. Density bump would close the gap but doubles vertex count; not worth it for a sanity test.

**Revolved-solid handling of full revolutions.** When `Angle ≈ 2π`, the start and end rings coincide. The handler skips end caps in that case (controlled by `FULL_REVOLUTION_EPSILON`) to avoid overlapping cap triangles at the seam. Vertices at the seam are still duplicated — proper dedup is polish, deferred.

**Vercel autodetection bites Python libraries.** Vercel scans the repo root, sees `pyproject.toml`, assumes Python web app, looks for one of 18 well-known entrypoint paths (`app.py`, `api/index.py`, `app/server.py`, etc.). For a library repo without any of those, it fails immediately. The right fix is to never link a library repo to Vercel in the first place — done.

## Next

The user's note: "then we keep pushing in a new session." This is mid-arc work, not a stopping point. Next session has a clean slate to pick the move.

Three concrete options ranked by leverage:

1. **Writer spike — byte-offset preservation + bit-identical no-op round-trip.** The qualitative leap from "X-ray" to "model-and-edit." Open the tracking issue, then make every entity carry `(byte_start, byte_end)` in the parser, then a writer that emits exact source bytes for untouched entities + re-serialised bytes for touched ones. Success criterion: `open(f).write(g)` where `f == g` byte-for-byte for any clean parse. This is the move that changes what ifcfast *is*.
2. **`IfcSurfaceCurveSweptAreaSolid`.** Closes #17 entirely. Profile sweeps along an arbitrary curve on a reference surface; profile orientation interpolated along the curve. Substantially harder than `IfcRevolvedAreaSolid` because the curve isn't an axis line and the surface anchors the profile plane. Bounded but non-trivial.
3. **`.ifczip` silent-drop (#19) + OOM scaling guard.** Two small fail-loudly fixes — neither blocks the writer, both close real production failure modes.

Default move if user gives no preference: option 1. Reveal-all is now ~95% complete (only one latent type left); more handler work is sharpening a finished blade. The writer is what's missing.

## Notes

- **No code on `ifcfast-site` this session.** Lots of unstaged changes still sitting in that working tree (`components/findings-view.tsx`, `app/dev/`, modified viewer/qto/graph, regenerated `public/sample/duplex.*`). Last actual deploy was 2 days ago. The site catch-up has now been deferred 4 sessions running. Per user framing, that's correct — the marketing surface waits on the product.
- **Two issues still queued but unfiled** (hook blocked on prior session, never explicitly OK'd): OOM scaling guard, writer-spike tracking. Both bodies are in the previous session's worklog. Next session can file them with explicit user OK or just embed the scope inline in the implementation commits.
- **Memory updates this session**: added `feedback_ifcfast_industry_scope.md` ("IFC spec is the backlog, not local corpus"). Replaces the old "always confirm pushes to default branch" stance with "just push" — both pre-existing memory updates from earlier in the session.
- **Vercel CLI is in `~/workspace/sidehustles/sprucelab/frontend/node_modules/.bin/vercel`**, not globally installed. Useful to remember when next session needs Vercel CLI access.
- **`ifcfast-site` is on `master`, not `main`**, and has no git remote at all — deploys are direct CLI uploads from the local working tree. Worth noting before any future site work.
