## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `e07c9c7` → `179f7d8` (one commit this thread; session start was `1bd4b94`)
- **Session scope**: build a mesh round-trip fidelity oracle — parse mesh → rebuild IFC via hotswap → re-parse → compare (deviation / byte-identity)
- **Touched paths**: tests/oracle/mesh_roundtrip.py, tests/test_mesh_roundtrip.py, docs/worklog/ (this file)
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: none (companion to `2026-07-08-16-16_141-truth-expansion-del3-del4.md`, the earlier thread of the same session)

## Summary

Built a **self-referential mesh round-trip fidelity gate** — the loop the
user asked for: read a mesh into a (verts, faces) frame, rebuild the IFC
body from that frame (`m.hotswap`), re-parse, and compare for deviation /
byte-identity. It fills the space between the two gates that already
existed: `doc_roundtrip.rs` (STEP-record byte-identity, geometry
untouched) and `test_geometry_oracle` (mesh volume vs ifcopenshell — one
axis, needs another kernel). This one drives the *mesh* through the writer
and needs no reference kernel, so it runs in the main `pytest -q`.

## Changes
- **`tests/oracle/mesh_roundtrip.py`** (new) — reusable adapter + CLI
  (`python -m tests.oracle.mesh_roundtrip MODEL.ifc [...] --limit N --cycles K`).
  Metrics per element: vertex/face **counts**, symmetric **Hausdorff**
  (scale-relative tol), signed **volume + winding_flip**, connectivity
  multiset (**faceset_equal**, informational), and **exact_array**
  (verts+faces bit-identical). `regressions()` gates on the robust axes;
  `roundtrip_element(cycles=K)` re-feeds each output as the next input
  (idempotence).
- **`tests/test_mesh_roundtrip.py`** (new) — committed IFC4 fixtures
  (byte-identical assertion), IFC2x3 fixture (set+volume+topology
  preserved, not byte-identical), two regression **detectors** (winding
  flip, dropped triangle — proves the gate fails on what it claims to
  catch), and an `IFCFAST_CORPUS` real-file gate. 29 passed incl. the RIV
  corpus sweep + hotswap corpus.

## Technical Details
- **The pattern is the one the `m.hotswap` docstring documents** (GH #127
  "decimate-in-place round-trip"): `mesh(frame="local")` returns exactly
  the frame hotswap writes verbatim, so no `ObjectPlacement`
  double-application.
- **Dialect split, measured on real G55 models**: IFC4 direct geometry
  (RIE) → **byte-identical** local mesh (100% `exact_array`). IFC2x3
  (RIB_Prefab) → re-tessellates/reorders → same vertex SET + volume +
  topology, *not* byte-identical. IFC4 **mapped-geometry MEP families**
  (RIV sprinklers/terminals) → ~1e-5 native-unit serialisation jitter
  (only 35% byte-identical) — benign: counts/volume/winding preserved.
- **Tolerance rework (the real lesson).** First cut used a fixed `1e-6`
  Hausdorff tol + `dp=6` position-keyed topology → false-flagged 26/40 RIV
  elements as "topology changed." Traced it: counts always preserved,
  deviation scales with coordinate magnitude → it's IFC-text writer
  precision, not a defect. Fix: **all tolerances scale-relative to each
  element's bbox diagonal** (`HAUS_REL=1e-6`, `VOL_REL_TOL=1e-4`, face
  quantum `1e-4·diag`); connectivity is diagnostic, not gated (a real
  topology change also trips count/volume, which ARE gated).
- **Disk fix**: hotswap re-serialises the *whole* file, so a 40-element
  sweep of the 168 MB ARK filled `/tmp` (quota). `roundtrip_element` now
  deletes each element's outputs after its cycles — but only after (the
  next cycle's model re-reads its source on hotswap, so eager per-file
  unlink broke multi-cycle; cleanup deferred to the element's `finally`).

## Next
- Optional: extend the oracle beyond `Body` — hotswap only touches the
  Body rep, so non-Body geometry isn't gated. Low priority.
- The RIV mapped-geometry ~1e-5 jitter observation composes with #145
  (narrow-phase misses on the same MEP terminal families) — if #145 turns
  out to be a mapped-geometry extraction issue, this oracle is where a fix
  would be regression-gated.

## Notes
- Test-only change: no source/geometry/QTO output moves, so the
  `/oracle-gate` ship gate does not apply, and no agent-visible primitive
  was added (oracle modules are dev tooling, not in AGENTS.md).
- Companion thread this session: #141 truth-expansion (5 sweepable clash
  rounds, GH #144 cache bug, GH #145 narrow-phase misses) — see the
  16:16 worklog.
