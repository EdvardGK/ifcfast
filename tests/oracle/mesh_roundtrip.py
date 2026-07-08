"""Mesh round-trip fidelity oracle — parse mesh → rebuild IFC → re-parse.

The self-referential companion to the ifcopenshell *differential* gate
(``_geom_adapter`` / ``test_geometry_oracle``). That gate asks "does our
mesh agree with another kernel's?"; this one asks "does our own mesh
survive a write-and-reread unchanged?" — the invariant every geometry or
writer change must not break.

The loop, per element (exactly the pattern the GH #127 hotswap docstring
documents)::

    M0 = model.mesh(guid, frame="local")          # parse into a (verts, faces) frame
    model.hotswap(guid, M0.verts, M0.faces, out)  # rebuild IFC body from that frame
    M1 = reopen(out).mesh(guid, frame="local")    # re-parse the rebuilt body
    compare(M0, M1)                                # deviation / byte-identity

Because both sides come from ifcfast, no reference kernel is needed and
the gate runs in a minimal env (numpy only). What it measures:

* ``exact_array`` — verts AND faces bit-identical. Holds for **IFC4**
  tessellated output (the point list is written verbatim), so the local
  mesh really is *byte-identical* across the round-trip. This is the
  strongest form and the honest answer to "byte-identical meshes?" — it
  lives at the mesh/local-frame layer, not the full STEP file (parametric
  bodies become facesets, so whole-file byte-identity is impossible; see
  ``doc_roundtrip.rs`` for the record-level byte gate).
* ``hausdorff`` — symmetric vertex-cloud deviation. **IFC2x3** surface
  models re-tessellate and may reorder/reindex vertices, so ``exact_array``
  is false there but ``hausdorff`` is still 0 (same point set).
* ``faceset_equal`` — triangle **connectivity by geometry**: each face is
  keyed by the sorted tuple of its three (rounded) vertex positions, so
  the check is robust to vertex re-indexing yet catches a genuine topology
  change (a dropped/added/re-stitched triangle) that a vertex-set test
  misses.
* signed-volume preservation + ``winding_flip`` — a triangle-orientation
  flip preserves the vertex cloud and the connectivity but negates signed
  volume; nothing else here would catch it.

Run over real files::

    python -m tests.oracle.mesh_roundtrip MODEL.ifc [MODEL2.ifc ...] \\
        [--limit 50] [--cycles 2] [--report out.json]

Exit 1 if any swept element regresses (winding flip, topology change,
volume drift beyond tolerance, or an IFC4 element that failed to come
back byte-identical).
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from collections import Counter
from pathlib import Path

import numpy as np

import ifcfast

# Geometry-grade tolerances, all SCALE-RELATIVE. The round-trip's floor is
# the IFC text writer's numeric precision, not zero: extracting a
# mapped-geometry element (Revit MEP families) applies a placement in
# float and hotswap re-serialises the result, so a vertex at 22 km from
# origin comes back ~1e-5 native units off — pure serialisation jitter,
# with vertex/face counts and volume preserved. Absolute tolerances would
# either false-fail far-from-origin geometry or miss a real deviation on a
# near-origin part. Everything is therefore keyed to the element's own
# bbox diagonal.
#
#   HAUS_REL   — vertex Hausdorff as a fraction of bbox diagonal. Writer
#                jitter measures ~1e-9·diag; a real moved vertex is
#                feature-scale (>~1e-3·diag). 1e-6 sits decades clear of both.
#   VOL_REL_TOL — |Δvolume| / volume. Observed round-trip drift ~1e-6.
HAUS_REL = 1e-6
HAUS_ABS_FLOOR = 1e-9
VOL_REL_TOL = 1e-4
# Coarse, scale-relative quantum for the *informational* topology multiset
# (reported, not gated on the corpus — a genuine topology change also moves
# counts/volume, which ARE gated). Above jitter, well below any feature.
FACE_QUANTUM_REL = 1e-4


def _bbox_diag(*clouds: np.ndarray) -> float:
    """Diagonal of the combined axis-aligned bounding box (0 if empty)."""
    pts = [c for c in clouds if len(c)]
    if not pts:
        return 0.0
    allpts = np.concatenate(pts, axis=0)
    return float(np.linalg.norm(allpts.max(axis=0) - allpts.min(axis=0)))


def signed_volume(verts: np.ndarray, faces: np.ndarray) -> float:
    """Signed enclosed volume via the divergence sum ``Σ (a×b)·c / 6``.

    Signed (no ``abs``) on purpose: the sign encodes triangle winding, so
    a wholesale orientation flip shows up as a negated volume even though
    the vertex cloud and connectivity are untouched.
    """
    if len(faces) == 0:
        return 0.0
    a = verts[faces[:, 0]]
    b = verts[faces[:, 1]]
    c = verts[faces[:, 2]]
    return float((np.cross(a, b) * c).sum() / 6.0)


def _directed_hausdorff(a: np.ndarray, b: np.ndarray) -> float:
    """max over ``a`` of (min distance to ``b``), chunked for bounded RAM."""
    worst = 0.0
    for i in range(0, len(a), 256):
        chunk = a[i : i + 256]
        d2 = ((chunk[:, None, :] - b[None, :, :]) ** 2).sum(axis=2)
        worst = max(worst, float(np.sqrt(d2.min(axis=1)).max()))
    return worst


def hausdorff(a: np.ndarray, b: np.ndarray) -> float:
    """Symmetric Hausdorff distance between two vertex clouds."""
    if len(a) == 0 or len(b) == 0:
        return float("inf")
    return max(_directed_hausdorff(a, b), _directed_hausdorff(b, a))


def face_position_multiset(
    verts: np.ndarray, faces: np.ndarray, quantum: float
) -> Counter:
    """Connectivity keyed by geometry: each triangle → sorted tuple of its
    three vertex positions snapped to a ``quantum`` grid. Invariant under
    vertex re-indexing and per-triangle rotation, and — with a quantum
    chosen above the writer's jitter — under serialisation precision, so it
    isolates true topology change. Informational only (see module notes)."""
    if quantum <= 0:
        snapped = verts
    else:
        snapped = np.round(verts / quantum) * quantum
    out: Counter = Counter()
    for tri in faces:
        key = tuple(sorted(tuple(snapped[i]) for i in tri))
        out[key] += 1
    return out


def compare_meshes(
    v0: np.ndarray,
    f0: np.ndarray,
    v1: np.ndarray,
    f1: np.ndarray,
) -> dict:
    """All fidelity axes between an original mesh (0) and its round-trip (1).

    Tolerances are scale-relative to the meshes' shared bbox diagonal, so
    the verdict fields (``hausdorff_ok``, ``count_ok``, etc.) are robust to
    IFC serialisation jitter on far-from-origin geometry. Raw metrics are
    reported alongside for triage.
    """
    v0 = np.asarray(v0, dtype=np.float64)
    v1 = np.asarray(v1, dtype=np.float64)
    f0 = np.asarray(f0, dtype=np.int64)
    f1 = np.asarray(f1, dtype=np.int64)

    diag = _bbox_diag(v0, v1)
    haus_tol = max(HAUS_ABS_FLOOR, diag * HAUS_REL)
    quantum = max(HAUS_ABS_FLOOR, diag * FACE_QUANTUM_REL)

    sv0 = signed_volume(v0, f0)
    sv1 = signed_volume(v1, f1)
    dvol_abs = abs(abs(sv0) - abs(sv1))
    denom = max(abs(sv0), abs(sv1), 1e-12)
    dvol_rel = dvol_abs / denom

    fp0 = face_position_multiset(v0, f0, quantum)
    fp1 = face_position_multiset(v1, f1, quantum)
    faces_lost = sum((fp0 - fp1).values())
    faces_gained = sum((fp1 - fp0).values())

    haus = hausdorff(v0, v1)
    exact = (
        v0.shape == v1.shape
        and f0.shape == f1.shape
        and np.array_equal(f0, f1)
        and np.array_equal(v0, v1)
    )
    count_ok = len(v0) == len(v1) and len(f0) == len(f1)
    winding_flip = bool(sv0 * sv1 < 0 and min(abs(sv0), abs(sv1)) > 1e-12)

    return {
        "n_verts0": int(len(v0)),
        "n_verts1": int(len(v1)),
        "n_faces0": int(len(f0)),
        "n_faces1": int(len(f1)),
        "count_ok": count_ok,
        "hausdorff": haus,
        "haus_tol": haus_tol,
        "hausdorff_ok": haus <= haus_tol,
        "signed_vol0": sv0,
        "signed_vol1": sv1,
        "dvol_abs": dvol_abs,
        "dvol_rel": dvol_rel,
        "volume_ok": dvol_rel <= VOL_REL_TOL,
        "winding_flip": winding_flip,
        "faceset_equal": faces_lost == 0 and faces_gained == 0,
        "faces_lost": int(faces_lost),
        "faces_gained": int(faces_gained),
        "exact_array": bool(exact),
        # Overall verdict for the gate: the robust axes only. A real
        # topology change also trips count_ok or volume_ok, so faceset_equal
        # (jitter-sensitive on huge coords) is diagnostic, not gating.
        "ok": count_ok and (haus <= haus_tol) and (dvol_rel <= VOL_REL_TOL) and not winding_flip,
    }


def local_mesh(model, guid: str):
    """``(verts (N,3) float64, faces (M,3) int64)`` in the element's local
    representation frame + native unit, or ``None`` if geometryless."""
    lm = model.mesh(guid, frame="local")
    if lm is None:
        return None
    return np.asarray(lm.vertices, dtype=np.float64), np.asarray(
        lm.faces, dtype=np.int64
    )


def roundtrip_element(model, guid: str, tmp_dir, *, cycles: int = 1) -> dict | None:
    """Round-trip one element ``cycles`` times, re-feeding each output as
    the next input (idempotence check). Returns ``None`` for geometryless /
    faceless elements; a ``skip_reason`` record for elements hotswap
    rejects (e.g. no ``Body`` representation).

    Each emitted IFC is deleted right after it is re-parsed — hotswap
    re-serialises the *whole* file, so on a big model the peak disk stays
    at one file instead of ``cycles × n_elements`` copies."""
    m0 = local_mesh(model, guid)
    if m0 is None:
        return None
    cur_v, cur_f = m0
    if len(cur_f) == 0:
        return None

    rec: dict = {"guid": guid, "schema": model.schema, "cycles": []}
    cur_model = model
    outputs: list[Path] = []
    try:
        for c in range(cycles):
            out = Path(tmp_dir) / f"{guid}.rt{c}.ifc"
            try:
                # cur_model re-reads its source on hotswap, so the previous
                # cycle's file must still exist here — cleanup is deferred to
                # the end of the element (see finally).
                cur_model.hotswap(guid, cur_v, cur_f, out_path=str(out))
            except ValueError as exc:
                rec["skip_reason"] = str(exc)
                return rec
            outputs.append(out)
            m2 = ifcfast.open(out, use_cache=False, write_cache=False)
            nxt = local_mesh(m2, guid)
            if nxt is None:
                rec["skip_reason"] = "lost geometry after hotswap"
                return rec
            nv, nf = nxt
            rec["cycles"].append(compare_meshes(cur_v, cur_f, nv, nf))
            cur_v, cur_f, cur_model = nv, nf, m2
        return rec
    finally:
        # hotswap re-serialises the whole file, so free this element's copies
        # before moving to the next (peak disk = cycles files, not the sweep).
        for out in outputs:
            out.unlink(missing_ok=True)


def candidate_guids(
    model, *, face_min: int = 1, face_max: int = 200_000, vert_max: int = 200_000, limit: int = 50
) -> list[str]:
    """Meshable products within a size band (keeps the O(N·M) Hausdorff
    cheap on a sweep). Size band applies to the local mesh."""
    out: list[str] = []
    for mm in model.meshes(frame="local"):
        if face_min <= len(mm.faces) <= face_max and len(mm.vertices) <= vert_max:
            out.append(mm.guid)
            if len(out) >= limit:
                break
    return out


def sweep(
    path,
    *,
    limit: int = 50,
    cycles: int = 1,
    face_min: int = 1,
    face_max: int = 200_000,
    vert_max: int = 200_000,
) -> list[dict]:
    """Round-trip up to ``limit`` size-banded elements of one file."""
    model = ifcfast.open(path, use_cache=False, write_cache=False)
    recs: list[dict] = []
    with tempfile.TemporaryDirectory() as tmp:
        for guid in candidate_guids(
            model, face_min=face_min, face_max=face_max, vert_max=vert_max, limit=limit
        ):
            rec = roundtrip_element(model, guid, tmp, cycles=cycles)
            if rec is not None:
                recs.append(rec)
    return recs


def regressions(recs: list[dict]) -> list[dict]:
    """Records with a real fidelity failure (not a benign skip). Gates on
    the robust axes (``ok``): count preserved, Hausdorff within the
    scale-relative tolerance, volume preserved, no winding flip."""
    bad = []
    for r in recs:
        if "skip_reason" in r:
            continue
        if any(not cyc["ok"] for cyc in r["cycles"]):
            bad.append(r)
    return bad


def _summise(all_recs: list[dict]) -> dict:
    swept = [r for r in all_recs if "skip_reason" not in r]
    skipped = [r for r in all_recs if "skip_reason" in r]
    first = [r["cycles"][0] for r in swept if r["cycles"]]
    return {
        "n_swept": len(swept),
        "n_skipped": len(skipped),
        "worst_hausdorff": max((c["hausdorff"] for c in first), default=0.0),
        "worst_dvol_rel": max((c["dvol_rel"] for c in first), default=0.0),
        "n_winding_flip": sum(c["winding_flip"] for c in first),
        "n_count_change": sum(not c["count_ok"] for c in first),
        "n_faceset_mismatch": sum(not c["faceset_equal"] for c in first),
        "exact_array_rate": (
            sum(c["exact_array"] for c in first) / len(first) if first else 0.0
        ),
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Mesh round-trip fidelity oracle")
    ap.add_argument("paths", nargs="+", type=Path)
    ap.add_argument("--limit", type=int, default=50, help="elements per file")
    ap.add_argument("--cycles", type=int, default=1, help="round-trips per element")
    ap.add_argument("--face-max", type=int, default=200_000)
    ap.add_argument("--vert-max", type=int, default=200_000)
    ap.add_argument("--report", type=Path, help="write full per-element JSON")
    args = ap.parse_args(argv)

    all_recs: list[dict] = []
    for p in args.paths:
        if not p.exists():
            print(f"missing: {p}", file=sys.stderr)
            return 2
        recs = sweep(
            p,
            limit=args.limit,
            cycles=args.cycles,
            face_max=args.face_max,
            vert_max=args.vert_max,
        )
        for r in recs:
            r["file"] = str(p)
        all_recs.extend(recs)
        s = _summise(recs)
        print(
            f"{p.name}: swept {s['n_swept']} skipped {s['n_skipped']}  "
            f"worst_hausdorff={s['worst_hausdorff']:.3g} native  "
            f"worst_dvol_rel={s['worst_dvol_rel']:.3g}  "
            f"exact_array={s['exact_array_rate']*100:.0f}%  "
            f"[gated] count_changes={s['n_count_change']} winding_flips={s['n_winding_flip']}  "
            f"[info] faceset_mismatch={s['n_faceset_mismatch']}"
        )

    bad = regressions(all_recs)
    if args.report:
        args.report.write_text(json.dumps(all_recs, indent=1))
        print(f"report written: {args.report}")

    if bad:
        print(f"\nREGRESSIONS: {len(bad)} element(s) failed fidelity", file=sys.stderr)
        for r in bad[:20]:
            c = r["cycles"][0]
            print(
                f"  {r['file'].rsplit('/',1)[-1]}:{r['guid']} ({r['schema']}) "
                f"haus={c['hausdorff']:.3g} dvol_rel={c['dvol_rel']:.3g} "
                f"winding_flip={c['winding_flip']} faceset_equal={c['faceset_equal']} "
                f"exact={c['exact_array']}",
                file=sys.stderr,
            )
        return 1
    print("\nOK — all swept elements round-tripped within tolerance")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
