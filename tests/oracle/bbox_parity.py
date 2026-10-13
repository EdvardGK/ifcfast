"""Whole-model per-element world-AABB parity: ifcfast meshes vs ifcopenshell.

The geometry-position differential behind GH #218 (large-radius arc
profiles 146 mm off ifcopenshell on KNM_ARK with ``volume_reliable=True``),
promoted from the one-off investigation scripts into a repeatable tool in
the style of :mod:`tests.oracle.class_sweep`. Volume is placement- and
tessellation-invariant, so the class sweep cannot see a shape that is the
right size in the wrong place; the world AABB can.

Per element (keyed by GlobalId) it records, on both sides, the world-frame
axis-aligned bounding box in metres, the signed-tetra volume and the
triangle count, then prints:

* the distribution of ``max |corner delta|`` over shared elements
  (p50 / p90 / p99 / max, counts > 1 mm / > 1 cm / > 10 cm);
* the same per entity class, worst GUID first;
* the top-N elements with their six per-corner deltas and both volumes;
* the coverage differential (classes only one side meshed);
  ``--export-sweep-cache`` hands class_sweep the ifcopenshell volume in
  class_sweep's own convention (local-frame vertices about the representation
  origin, ``vol_local``) plus an ``indeterminate`` flag per element;
* how many elements have an origin-dependent signed volume (net area vector
  ≠ 0: open or inconsistently wound shells — GH #229 / #230). For those the
  ``vol ff / ios`` ratio printed in the top-N is an artefact of where each
  side integrates from, not a kernel divergence.

Usage (repo root, venv active)::

    python -m tests.oracle.bbox_parity MODEL.ifc --cache-dir DIR \\
        [--mode cut|nocut] [--top 20] [--threads 4] \\
        [--refresh-fast]                 # new build: redo ifcfast, keep ios
        [--dump-elements out.json]       # per-element rows
        [--export-sweep-cache DIR]       # seed class_sweep's ios cache

Frames (AGENTS.md, GH #179 / #188)
----------------------------------
ifcfast side: ``m.iter_meshes(cut_openings=…)`` returns *shifted* world
``float32`` vertices in metres; absolute world = ``v + it.global_shift``
(added in ``float64`` here). ifcopenshell side: DEFAULT settings (local
coordinates — ``use-world-coords`` segfaults on some corpora, see
:mod:`tests.oracle._geom_adapter`) and each shape's
``transformation.matrix`` (16 doubles, column-major) applied in numpy:
``world = M.T @ [x, y, z, 1]``. That is exactly what ``use-world-coords``
computes, without the risky code path. Both sides end up in absolute
metres; the residual from the ``float32`` shifted frame is ≤ 1 µm·(extent
from the shift anchor in km).

Modes (GH #194 / #122)
----------------------
``cut`` (default): ``meshes(cut_openings=True)`` vs ifcopenshell defaults
(openings subtracted). ifcfast folds every ``IfcFeatureElementSubtraction``
into its host and emits no row for it, so those are dropped from the ios
side before comparing. ``nocut``: ``meshes(cut_openings=False)`` — the
``mesh()`` / substrate / ``clash()`` / wasm contract — vs
``disable-opening-subtractions``; openings are compared like any product.
Spatial containers (``IfcSite`` / ``IfcBuilding`` / ``IfcBuildingStorey``)
are never meshed by ifcfast and are reported as coverage, not divergence.

Caches: ``<stem>_ff_bbox.json`` (both ifcfast modes, one open) and
``<stem>_ios_bbox_<mode>.json`` under ``--cache-dir``. ONE ifcopenshell
process at a time, machine-wide (RAM): wrap the first run in
``flock /tmp/ifcfast-oracle.lock``.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np

MODES = ("cut", "nocut")
CORNERS = ("xmin", "ymin", "zmin", "xmax", "ymax", "zmax")
SPATIAL = {"IfcSite", "IfcBuilding", "IfcBuildingStorey", "IfcSpatialZone"}


def _tet_volume(v: np.ndarray, f: np.ndarray, origin: np.ndarray | None = None) -> float:
    """abs signed-tetra volume of (verts, tris) about ``origin`` (default: vertex mean).

    Only origin-independent when the net area vector is zero (closed AND
    coherently wound). See :func:`_shell_stats` and GH #230.
    """
    if len(f) == 0:
        return 0.0
    c = v - (v.mean(0) if origin is None else origin)
    a, b, cc = c[f[:, 0]], c[f[:, 1]], c[f[:, 2]]
    return float(abs(np.einsum("ij,ij->i", a, np.cross(b, cc)).sum()) / 6.0)


def _shell_stats(v: np.ndarray, f: np.ndarray) -> dict:
    """Origin-independence evidence for one mesh (GH #230).

    ``area`` = total triangle area, ``snorm`` = |Σ face-area-vectors| (zero for
    a closed, coherently wound shell), ``vol_min`` / ``vol_mean`` = |signed
    volume| about the bbox-min corner (what ``qto.rs::compute`` uses) and the
    vertex mean. When ``snorm > 1e-3 * area`` or the two volumes differ by
    > 0.1 % the element's "volume" is a function of the origin and no kernel
    number is a reference for it.
    """
    if len(f) == 0:
        return {"area": 0.0, "snorm": 0.0, "vol_min": 0.0, "vol_mean": 0.0}
    av = 0.5 * np.cross(v[f[:, 1]] - v[f[:, 0]], v[f[:, 2]] - v[f[:, 0]])
    return {
        "area": float(np.linalg.norm(av, axis=1).sum()),
        "snorm": float(np.linalg.norm(av.sum(0))),
        "vol_min": _tet_volume(v, f, v.min(0)),
        "vol_mean": _tet_volume(v, f),
    }


def is_indeterminate(rec: dict) -> bool | None:
    """True when the record's signed volume depends on the origin; None if the
    cache predates the stats (recompute with --refresh-fast / a fresh ios run)."""
    if "snorm" not in rec:
        return None
    if rec["snorm"] > 1e-3 * max(rec["area"], 1e-12):
        return True
    hi = max(rec["vol_min"], rec["vol_mean"])
    return hi > 0 and abs(rec["vol_min"] - rec["vol_mean"]) / hi > 1e-3


def fast_bboxes(ifc_path: Path) -> dict:
    """ifcfast side, both modes in one open: {mode: {guid: {cls, mn, mx, vol, ntri}}}."""
    import ifcfast

    m = ifcfast.open(str(ifc_path))
    out = {"ifcfast": getattr(ifcfast, "__version__", None), "unit_scale": m.unit_scale}
    for mode in MODES:
        t = time.time()
        ms = m.iter_meshes(cut_openings=(mode == "cut"))  # MeshIter (GH #179)
        shift = np.asarray(ms.global_shift, dtype=np.float64)
        d: dict[str, dict] = {}
        for x in ms:
            if len(x.faces) == 0:
                continue
            v = x.vertices.astype(np.float64) + shift
            g = d.get(x.guid)
            if g is None:
                g = d[x.guid] = {
                    "cls": x.entity, "mn": [np.inf] * 3, "mx": [-np.inf] * 3,
                    "vol": 0.0, "ntri": 0,
                }
            g["mn"] = np.minimum(g["mn"], v.min(0)).tolist()
            g["mx"] = np.maximum(g["mx"], v.max(0)).tolist()
            g["vol"] += _tet_volume(v, x.faces.astype(np.int64))
            g["ntri"] += int(len(x.faces))
            st = _shell_stats(v, x.faces.astype(np.int64))
            for k, val in st.items():  # per-item sums; exact for single-item products
                g[k] = g.get(k, 0.0) + val
        out[mode] = d
        out[f"{mode}_shift"] = shift.tolist()
        out[f"{mode}_s"] = time.time() - t
        print(f"ifcfast {mode}: {len(d)} products, {time.time() - t:.1f}s, shift={shift.tolist()}", flush=True)
    return out


def ios_bboxes(ifc_path: Path, mode: str, threads: int) -> dict:
    """ifcopenshell side: {ios, elems: {guid: {cls, mn, mx, vol, ntri}}, s, errors}."""
    import ifcopenshell
    import ifcopenshell.geom

    t0 = time.time()
    f = ifcopenshell.open(str(ifc_path))
    print(f"ifcopenshell {ifcopenshell.version} open {time.time() - t0:.1f}s", flush=True)
    settings = ifcopenshell.geom.settings()  # DEFAULT: local coords, openings applied
    if mode == "nocut":
        settings.set("disable-opening-subtractions", True)
    it = ifcopenshell.geom.iterator(settings, f, threads)
    elems: dict[str, dict] = {}
    errors: Counter = Counter()
    if it.initialize():
        while True:
            sh = it.get()
            try:
                geom = sh.geometry
                v = np.asarray(geom.verts, dtype=np.float64).reshape(-1, 3)
                fa = np.asarray(geom.faces, dtype=np.int64).reshape(-1, 3)
                if len(fa):
                    M = np.asarray(sh.transformation.matrix, dtype=np.float64).reshape(4, 4).T
                    w = v @ M[:3, :3].T + M[:3, 3]
                    cls = f.by_id(sh.id).is_a()
                    g = elems.get(sh.guid)
                    rec = {
                        "cls": cls, "mn": w.min(0).tolist(), "mx": w.max(0).tolist(),
                        "vol": _tet_volume(v, fa), "ntri": int(len(fa)),
                        # class_sweep's convention: local-frame verts about the
                        # representation origin — what --export-sweep-cache hands on
                        "vol_local": _tet_volume(v, fa, np.zeros(3)),
                        **_shell_stats(w, fa),
                    }
                    if g is None:
                        elems[sh.guid] = rec
                    else:  # same guid emitted twice (never seen; keep the union)
                        g["mn"] = np.minimum(g["mn"], rec["mn"]).tolist()
                        g["mx"] = np.maximum(g["mx"], rec["mx"]).tolist()
                        g["vol"] += rec["vol"]
                        g["ntri"] += rec["ntri"]
            except Exception as e:  # noqa: BLE001 — one bad product must not abort
                errors[type(e).__name__] += 1
            if not it.next():
                break
    print(f"ifcopenshell {mode}: {len(elems)} products, {time.time() - t0:.1f}s, errors={dict(errors)}", flush=True)
    return {"ios": ifcopenshell.version, "mode": mode, "elems": elems,
            "s": time.time() - t0, "errors": dict(errors)}


def compare(ff: dict, ios_elems: dict, mode: str, subtractive_classes: set[str] | None = None) -> dict:
    """Join on guid; per element max |corner delta| in metres."""
    F = ff[mode]
    io = ios_elems
    if mode == "cut":
        drop = {g for g, r in io.items() if r["cls"] in (subtractive_classes or set())}
    else:
        drop = set()
    spatial_only = {g for g, r in io.items() if r["cls"] in SPATIAL and g not in F}
    common = [g for g in io if g in F]
    rows = []
    for g in common:
        a = np.array(F[g]["mn"] + F[g]["mx"])
        b = np.array(io[g]["mn"] + io[g]["mx"])
        dd = a - b
        k = int(np.argmax(np.abs(dd)))
        rows.append({
            "guid": g, "cls": io[g]["cls"], "dmax_m": float(np.abs(dd).max()),
            "worst": CORNERS[k], "delta_m": dd.tolist(),
            "vol_ff": F[g]["vol"], "vol_ios": io[g]["vol"],
            "ntri_ff": F[g]["ntri"], "ntri_ios": io[g]["ntri"],
            # GH #230: None when the cache predates the shell stats
            "indeterminate_ff": is_indeterminate(F[g]),
            "indeterminate_ios": is_indeterminate(io[g]),
        })
    rows.sort(key=lambda r: -r["dmax_m"])
    ios_only = Counter(io[g]["cls"] for g in io if g not in F and g not in drop and g not in spatial_only)
    ff_only = Counter(F[g]["cls"] for g in F if g not in io)
    return {
        "rows": rows,
        "ios_only": dict(ios_only), "ff_only": dict(ff_only),
        "ios_spatial_only": len(spatial_only), "ios_subtractive_dropped": len(drop),
    }


def _pct(d: np.ndarray, q: float) -> float:
    return float(np.percentile(d, q)) if len(d) else float("nan")


def print_report(cmp: dict, model: str, mode: str, top: int) -> None:
    rows = cmp["rows"]
    d = np.array([r["dmax_m"] for r in rows]) if rows else np.zeros(0)
    print(f"\n=== {model} [mode={mode}] bbox parity: shared {len(rows)}, "
          f"ios-only {sum(cmp['ios_only'].values())}, ff-only {sum(cmp['ff_only'].values())}, "
          f"ios spatial containers {cmp['ios_spatial_only']}, ios subtractive dropped {cmp['ios_subtractive_dropped']} ===")
    if cmp["ios_only"]:
        print("ios-only by class:", dict(sorted(cmp["ios_only"].items(), key=lambda kv: -kv[1])))
    if cmp["ff_only"]:
        print("ff-only by class: ", dict(sorted(cmp["ff_only"].items(), key=lambda kv: -kv[1])))
    if not len(d):
        print("no shared elements")
        return
    ind = [r for r in rows if r["indeterminate_ios"] or r["indeterminate_ff"]]
    unknown = sum(1 for r in rows if r["indeterminate_ios"] is None)
    print(f"origin-dependent volume (GH #230): {len(ind)} elements"
          + (f" ({unknown} without stats — old cache)" if unknown else "")
          + " — their vol ff/ios ratio is NOT a kernel divergence")
    print(f"max|corner delta|: p50={np.median(d) * 1e3:.3f}mm p90={_pct(d, 90) * 1e3:.3f}mm "
          f"p99={_pct(d, 99) * 1e3:.3f}mm max={d.max() * 1e3:.2f}mm  "
          f">1mm={int((d > 1e-3).sum())} >1cm={int((d > 1e-2).sum())} >10cm={int((d > 1e-1).sum())}")
    per: dict[str, list] = defaultdict(list)
    for r in rows:
        per[r["cls"]].append(r)
    print(f"\n{'class':<30}{'n':>6}{'p50mm':>9}{'p99mm':>9}{'maxmm':>10}{'>1mm':>6}{'>1cm':>6}  worst guid")
    for cls in sorted(per, key=lambda c: -max(r["dmax_m"] for r in per[c])):
        rs = per[cls]
        dc = np.array([r["dmax_m"] for r in rs])
        print(f"{cls:<30}{len(rs):>6}{np.median(dc) * 1e3:>9.3f}{_pct(dc, 99) * 1e3:>9.3f}"
              f"{dc.max() * 1e3:>10.2f}{int((dc > 1e-3).sum()):>6}{int((dc > 1e-2).sum()):>6}  {rs[0]['guid']}")
    print(f"\ntop {top}:")
    for r in rows[:top]:
        vr = (r["vol_ff"] / r["vol_ios"] - 1) * 100 if r["vol_ios"] else float("nan")
        print(f"{r['dmax_m'] * 1e3:9.2f}mm {r['guid']} {r['cls']:<24} worst={r['worst']:<4} "
              f"d=[{','.join(f'{x * 1e3:.1f}' for x in r['delta_m'])}]mm "
              f"vol ff={r['vol_ff']:.4f} ios={r['vol_ios']:.4f} ({vr:+.2f}%) tri {r['ntri_ff']}/{r['ntri_ios']}")


def subtractive_classes(ifc_path: Path) -> set[str]:
    """Entity names of the IfcFeatureElementSubtraction hierarchy for this schema."""
    import ifcfast
    from ifcfast.classify import subtypes_of

    schema = ifcfast.header(str(ifc_path)).schema
    return set(subtypes_of("IfcFeatureElementSubtraction", schema)) | {"IfcFeatureElementSubtraction"}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("ifc", type=Path)
    ap.add_argument("--cache-dir", type=Path, required=True)
    ap.add_argument("--mode", choices=MODES, default="cut")
    ap.add_argument("--top", type=int, default=20)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--refresh-fast", action="store_true",
                    help="recompute the ifcfast side (new build), reuse cached ios")
    ap.add_argument("--dump-elements", type=Path, default=None,
                    help="write per-element rows (guid, cls, deltas, volumes) as JSON")
    ap.add_argument("--export-sweep-cache", type=Path, default=None,
                    help="write class_sweep's <stem>_sweep[_nocut].json with the ios "
                         "volumes from this pass (fast side empty; run class_sweep "
                         "--refresh-fast on it) — one oracle pass serves both axes")
    args = ap.parse_args(argv)

    args.cache_dir.mkdir(parents=True, exist_ok=True)
    stem = args.ifc.stem
    ff_cache = args.cache_dir / f"{stem}_ff_bbox.json"
    ios_cache = args.cache_dir / f"{stem}_ios_bbox_{args.mode}.json"

    if ff_cache.exists() and not args.refresh_fast:
        ff = json.load(ff_cache.open())
        print(f"loaded cached {ff_cache}")
    else:
        ff = fast_bboxes(args.ifc)
        json.dump(ff, ff_cache.open("w"))
        print(f"wrote {ff_cache}")

    if ios_cache.exists():
        ios = json.load(ios_cache.open())
        print(f"loaded cached {ios_cache} (ifcopenshell {ios.get('ios')})")
    else:
        ios = ios_bboxes(args.ifc, args.mode, args.threads)
        json.dump(ios, ios_cache.open("w"))
        print(f"wrote {ios_cache}")

    if args.export_sweep_cache:
        suffix = "" if args.mode == "cut" else "_nocut"
        p = args.export_sweep_cache / f"{stem}_sweep{suffix}.json"
        p.parent.mkdir(parents=True, exist_ok=True)
        if not p.exists():
            # class_sweep::ios_volumes integrates the local-frame vertices about
            # the representation origin; hand on that number (``vol_local``) so
            # the two tools agree. Caches written before GH #230 only carry the
            # vertex-mean value — those are labelled, not silently reused.
            legacy = any("vol_local" not in r for r in ios["elems"].values())
            json.dump({"fast": {}, "ios": {g: {"entity": r["cls"], "volume": r.get("vol_local", r["vol"]),
                                               "origin": "vertex_mean" if "vol_local" not in r else "local",
                                               "indeterminate": is_indeterminate(r)}
                                         for g, r in ios["elems"].items()}}, p.open("w"))
            print(f"wrote class_sweep ios cache {p} (run class_sweep --refresh-fast)"
                  + (" — WARNING: legacy ios cache, volumes are vertex-mean centred; "
                     "only origin-independent rows are comparable" if legacy else ""))
        else:
            print(f"class_sweep cache {p} exists; not overwritten")

    cmp = compare(ff, ios["elems"], args.mode, subtractive_classes(args.ifc))
    print_report(cmp, args.ifc.name, args.mode, args.top)
    if args.dump_elements:
        args.dump_elements.parent.mkdir(parents=True, exist_ok=True)
        json.dump({"model": args.ifc.name, "mode": args.mode, "ifcopenshell": ios.get("ios"),
                   "ifcfast": ff.get("ifcfast"), **cmp}, args.dump_elements.open("w"))
        print(f"wrote {args.dump_elements}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
