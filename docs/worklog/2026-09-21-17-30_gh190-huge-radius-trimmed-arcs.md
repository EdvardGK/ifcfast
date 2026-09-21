## Agent signature
- **Agent**: `claude-fable-5-1` (coordinator; opus agent wrote profile.rs, sonnet agent wrote gltf.rs)
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `40a8226` → `a206057` (+ this worklog commit); tag `v0.5.3` = `a206057`
- **Session scope**: GH #190 huge-radius trimmed conic arcs (f64 + 1e-9 rad rule) and GH #189 glTF quantization flag; v0.5.3 release
- **Touched paths**: crates/core/src/mesh/profile.rs, crates/core/src/mesh/gltf.rs, python/ifcfast/header.py, AGENTS.md, docs/worklog/
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: none

## Summary

GH #190 (filed today from the ifc-check side): five `IfcBeam` in a Geometry Gym
IFC2X3 export (`KNM_RIB.ifc` v2 on ACC, Kistefos, 11.3 MB) meshed to 13 000–32 000 km
with `volume_reliable = True`. Reproduced on the real file, mechanism traced, fixed,
oracle-gated.

### Mechanism (verified, not inherited)

The issue's theory ("SenseAgreement + descending trims sweep the complement arc")
was wrong; `trim_angle` already prefers the CARTESIAN trim point, and the sense
handling was right. Two real defects in `profile.rs`:

1. `arc_span` treated any sweep ≤ 1e-6 rad as "coincident trims → full
   revolution". The beam's closing edge is an `IfcTrimmedCurve` on an `IfcCircle`
   of radius 6 514 797 m sweeping 9.7e-7 rad (a 6 m chord), so it became the
   full 41 000 km circle. 6 of the file's 959 trimmed curves have sweep ≤ 1e-6
   (radii 6.5e6–1.6e7 m).
2. `conic_arc` / `trim_angle` sampled in f32: `centre + R·cos t` with the centre
   6.2e6 m away has a 0.5 m ulp, so every huge-radius arc's endpoints landed up
   to ~1 m off the neighbouring polyline vertices. 107 of the 959 arcs have radius
   > 1e4 m. This was a **silent QTO error on ~75 more beams** the report did not
   see (e.g. `0vMTExvmrEOxoCX422Z53a`: 0.455 m³ vs oracle 1.319 m³).

### Fix (`profile.rs`, cache schema v33 → v34)

- Trimmed-conic path is f64 end-to-end (`DVec2` helpers
  `cartesian_point_2d_f64` / `direction_2d_f64` / `placement2d_origin_dir_f64`),
  cast to `Vec2` only at emit.
- `arc_span` in f64, coincident-trim epsilon 1e-9 rad (doc comment records why
  an absolute angular epsilon is the wrong unit and why 1e-9 is still far above
  f64 noise). Authored `(0, 2π)` / `(a, a)` still give a full turn.
- Chord count and sector-area scale still go through the f32 `chord_count` /
  `arc_area_scale` path with `sweep as f32`, so ordinary arcs keep their segment
  count and scale.
- CARTESIAN-vs-PARAMETER preference and MasterRepresentation untouched.
- Tests: `arc_span_keeps_a_sub_microradian_sweep`;
  `geometry_gym_huge_radius_trimmed_arc_stays_a_six_metre_chord` — the repro
  profile chain verbatim (#196790–#196810), asserting |x| ≤ 1, |y| ≤ 3.1, area
  within 0.2 % of the oracle 3.994 m², arc endpoints bit-identical to the
  polyline vertices (were ~0.5 m off).

### Evidence

Repro model, `tests.oracle.class_sweep` before → after (ifcopenshell 0.8.5):

| class | n | before ratio | after ratio |
|---|---|---|---|
| IfcBeam | 514 | 2.7e19 | 1.0000 |
| IfcColumn | 280 | 1.0000 | 1.0000 |
| IfcFooting | 56 | 1.0000 | 1.0000 |
| IfcSlab | 1 | 1.0000 | 1.0000 |

Per-element A/B (`scratch/g190/ab.py`): 80 of 851 moved, **all 80 carry a
trimmed conic** (0 without), after-state has **0 elements > 0.5 % from oracle**;
4 "moved away" are at the 1e-5 relative level. The five reported beams: 1.3971 /
1.4089 / 2.5806 / 3.4123 / 2.5782 m³ vs oracle 1.3979 / 1.4104 / 2.5817 / 3.4127 /
2.5803, all `closed`, `volume_reliable = True` honestly now. Drift table: the
five `ok` rows (ratio 0.5 on a 13 000 km extent) are gone; all 851 read `info`
(placement-vs-centroid ~430–540 m is the file's convention, pre-existing).

Not built: the issue's proposed "mesh extent > 100× model AABB" tripwire — the
mesh itself defines the model AABB, so the reference is circular, and any
per-class outlier rule would label legitimately large elements. Root fix removes
the class of failure; `arc_span` remains absolute-epsilon (1e-9 rad = 6.5 mm of
arc on a 6.5e6 m circle — no corpus case anywhere near it).

### GH #189

`want_quant = n_baked > 0 || n_groups > 0` in `gltf.rs` (`79873a5`, sonnet agent):
an all-instanced glTF packed u16 positions without declaring
`KHR_mesh_quantization`. Two writer tests (all-instanced two-cube fixture,
baked-only guard). AGENTS.md glTF section notes the declaration rule.

### Gate

One chained background script (`scratch/g190/gate.sh`), never fanned out:

- `cargo test -p ifcfast-core`: 463 passed, 0 failed, 5 ignored; clippy `-D warnings`
  and `cargo fmt --check` clean (both agents).
- `class_sweep --refresh-fast` vs baselines: G55 ARK / RIB / RIV / RIE — "no class
  drifted past ±0.005" on all four, every drift column +0.0000. RIV and RIE had no
  ios cache and ran the full ifcopenshell pass (~50 min).
- pytest: 371 passed + 8 warnings without corpus; corpus write-axis
  (hotswap / local_frame / roundtrip / mutate / subset over 4 G55 files):
  61 passed, 3 skipped in 1 h 13 min. Gotcha re-learned: `IFCFAST_CORPUS`
  is a colon-separated FILE list, not a directory — a directory passes
  `path.exists()` and fails inside `ifcopenshell.open`.
- wasm: `crates/wasm/build.sh` + `parity.mjs` 18/18, `stream.mjs` 22/22 after
  regenerating the Duplex sidecars (one qto area moved 2e-11 relative — the f64
  ulp) and bumping the wasm `CACHE_SCHEMA_VERSION` mirror to 34 (the `cache_key`
  parity check catches a forgotten mirror bump). Site `4aa2944` pushed.
- Repro file: `scratch/g190/KNM_RIB_v2.ifc` (ACC Kistefos, lineage
  `brNeky0ZRTi0t_aHkNRIOA` **version 2** — v3 on ACC is a 0.2 MB Revit
  re-export without the Geometry Gym geometry). Before/after sweep caches and
  `ab.py` in `scratch/g190/`.

### Release

`a206057` release: v0.5.3 (CHANGELOG covers #188 + #189 + #190). Tag pushed
after CI green — see the addendum below for the CI/tag outcome.

## Next
- v0.5.3 tag: confirm the release workflow published to PyPI + GH release.
- #190 comment posted; closing is the reporter's/Ed's call (issue not opened by
  this agent). #189 closed by `Fixes #189`.
- Open queue unchanged: #187 classifier (self-touching solids), #186 canonical
  entity casing, #185 materials rollup, #182 unit audit, #117 substrate residues.
- Theoretical hole left in `arc_span`: the 1e-9 rad coincidence rule is still
  absolute (6.5 mm of arc on a 6.5e6 m circle). A radius-relative rule needs the
  radius in scope; only worth doing if a file ever authors such an arc.
- `arc_area_scale` keeps its own absolute `t > 1e-6` guard (returns 1.0 below);
  harmless for near-straight chords, noted for completeness.
- Scratch: `scratch/g55/cache_170_*` / `cache_clash170*` (hundreds of MB) and
  `scratch/g55/cache_gate178` + `cache_ab_head` still pending Ed's say-so;
  `scratch/g190/` (~40 MB) is this session's evidence, keep until v0.5.3 is out.

## Addendum — release outcome

- CI on `a206057`: `ci` + `csg-smoke` green. Tag `v0.5.3` pushed; release
  workflow run 35660429066 green (linux x86_64, windows, macos aarch64 +
  intel, sdist, Publish to PyPI). `pip index versions ifcfast` → 0.5.3.
- GitHub release `ifcfast v0.5.3` created from the CHANGELOG section (the
  workflow publishes to PyPI only; the release object is manual, as for
  v0.5.2).
- Site `4aa2944` pushed (sidecars + wasm at `78354e4`).
