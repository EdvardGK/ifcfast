"""Whole-model per-entity-class volume sweep: ifcfast mesh_qto vs ifcopenshell.

The corpus-differential workhorse (see the ``oracle-gate`` project skill).
Runs both kernels over one IFC, aggregates ``SUM(volume_m3)`` per entity
class keyed by GlobalId, prints a ratio table, and — when a baseline file
is given — flags per-class drift so a geometry change can be gated on
"which classes moved, and toward or away from the oracle".

Usage (from the repo root, venv active)::

    python -m tests.oracle.class_sweep path/to/MODEL.ifc \
        [--cache-dir DIR]            # JSON cache; skip re-tessellation
        [--baseline FILE]            # prior sweep JSON to diff against
        [--write-baseline FILE]      # save this sweep as a new baseline
        [--tolerance 0.005]          # flag classes whose ratio moved more

Baselines for client corpora (e.g. G55) live OUTSIDE the repo — they are
client data. Convention: ``scratch/<corpus>/baselines/<MODEL>.json``.

Kernel contract mirrors :mod:`tests.oracle._geom_adapter`: DEFAULT
ifcopenshell settings (openings applied, local coords — volume is
placement-invariant for closed shells and world-coords segfaults on some
corpora), one ``ifcopenshell.geom.iterator`` pass, signed-tetra
``abs()/6`` volume, per-element try/except so one bad product never
aborts the sweep. ifcfast side is ``mesh_qto(cut_openings=True)`` and
uses ``volume_m3`` (the routed best estimate — the number agents consume).
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path


def fast_volumes(ifc_path: Path) -> dict[str, dict]:
    """ifcfast side: {guid: {entity, volume, mesh_quality, volume_method}}."""
    import ifcfast

    m = ifcfast.open(str(ifc_path))
    prod, _surf = m.mesh_qto(cut_openings=True)
    out: dict[str, dict] = {}
    for _, r in prod.iterrows():
        v = r["volume_m3"]
        out[r["guid"]] = {
            "entity": r["entity"],
            "volume": None if v is None else float(v),
            "mesh_quality": r["mesh_quality"],
            "volume_method": r["volume_method"],
        }
    return out


def ios_volumes(ifc_path: Path) -> dict[str, dict]:
    """ifcopenshell side: {guid: {entity, volume}} — single iterator pass."""
    import ifcopenshell
    import ifcopenshell.geom

    f = ifcopenshell.open(str(ifc_path))
    entity_of = {e.GlobalId: e.is_a() for e in f.by_type("IfcProduct")}
    settings = ifcopenshell.geom.settings()  # DEFAULT — no use-world-coords
    it = ifcopenshell.geom.iterator(settings, f)
    out: dict[str, dict] = {}
    if not it.initialize():
        return out
    while True:
        shape = it.get()
        try:
            geom = shape.geometry
            verts = list(geom.verts)
            faces = geom.faces
            total = 0.0
            fit = iter(faces)
            for i, j, k in zip(fit, fit, fit):
                ax, ay, az = verts[3 * i], verts[3 * i + 1], verts[3 * i + 2]
                bx, by, bz = verts[3 * j], verts[3 * j + 1], verts[3 * j + 2]
                cx, cy, cz = verts[3 * k], verts[3 * k + 1], verts[3 * k + 2]
                total += (
                    (ay * bz - az * by) * cx
                    + (az * bx - ax * bz) * cy
                    + (ax * by - ay * bx) * cz
                )
            out[shape.guid] = {
                "entity": entity_of.get(shape.guid, "?"),
                "volume": abs(total) / 6.0,
            }
        except Exception:  # noqa: BLE001 — one bad product must not abort
            pass
        if not it.next():
            break
    return out


def per_class(fast: dict, ios: dict) -> dict[str, dict]:
    """Aggregate shared-guid volumes per entity class."""
    shared = set(fast) & set(ios)
    agg: dict[str, dict] = defaultdict(
        lambda: {"n": 0, "fast_sum": 0.0, "ios_sum": 0.0, "n_fast_none": 0}
    )
    for g in shared:
        ent = ios[g]["entity"]
        d = agg[ent]
        d["n"] += 1
        fv = fast[g]["volume"]
        if fv is None:
            d["n_fast_none"] += 1
        else:
            d["fast_sum"] += fv
        d["ios_sum"] += ios[g]["volume"]
    for d in agg.values():
        d["ratio"] = d["fast_sum"] / d["ios_sum"] if d["ios_sum"] > 1e-9 else None
    return dict(agg)


def print_table(agg: dict[str, dict], baseline: dict[str, dict] | None, tolerance: float) -> list[str]:
    """Print the sweep table; return the list of drifted class names."""
    drifted: list[str] = []
    hdr = f"{'class':<30}{'n':>6}{'fast_sum':>14}{'ios_sum':>14}{'ratio':>9}"
    if baseline:
        hdr += f"{'base':>9}{'drift':>9}"
    print(hdr)
    for ent in sorted(agg, key=lambda e: -agg[e]["ios_sum"]):
        d = agg[ent]
        ratio = d["ratio"]
        line = (
            f"{ent:<30}{d['n']:>6}{d['fast_sum']:>14.3f}{d['ios_sum']:>14.3f}"
            f"{ratio if ratio is not None else float('nan'):>9.4f}"
        )
        if baseline:
            b = baseline.get(ent, {}).get("ratio")
            if b is not None and ratio is not None:
                drift = ratio - b
                line += f"{b:>9.4f}{drift:>+9.4f}"
                if abs(drift) > tolerance:
                    drifted.append(ent)
                    line += "  <-- DRIFT"
            else:
                line += f"{'new':>9}{'':>9}"
        print(line)
    return drifted


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("ifc", type=Path)
    ap.add_argument("--cache-dir", type=Path, default=None)
    ap.add_argument("--baseline", type=Path, default=None)
    ap.add_argument("--write-baseline", type=Path, default=None)
    ap.add_argument("--tolerance", type=float, default=0.005)
    args = ap.parse_args()

    cache = (
        args.cache_dir / f"{args.ifc.stem}_sweep.json" if args.cache_dir else None
    )
    if cache and cache.exists():
        data = json.load(cache.open())
        fast, ios = data["fast"], data["ios"]
        print(f"loaded cached {cache}")
    else:
        fast = fast_volumes(args.ifc)
        print(f"ifcfast products: {len(fast)}", flush=True)
        ios = ios_volumes(args.ifc)
        print(f"ios products: {len(ios)}", flush=True)
        if cache:
            cache.parent.mkdir(parents=True, exist_ok=True)
            json.dump({"fast": fast, "ios": ios}, cache.open("w"))
            print(f"wrote {cache}")

    shared = set(fast) & set(ios)
    print(
        f"\n=== {args.ifc.name}: shared guids {len(shared)}, "
        f"fast-only {len(set(fast) - set(ios))}, ios-only {len(set(ios) - set(fast))} ==="
    )
    agg = per_class(fast, ios)
    baseline = json.load(args.baseline.open()) if args.baseline else None
    drifted = print_table(agg, baseline, args.tolerance)

    if args.write_baseline:
        args.write_baseline.parent.mkdir(parents=True, exist_ok=True)
        json.dump(agg, args.write_baseline.open("w"), indent=1)
        print(f"\nbaseline written: {args.write_baseline}")

    if drifted:
        print(f"\nDRIFTED past ±{args.tolerance}: {', '.join(drifted)}")
        return 1
    if baseline:
        print(f"\nno class drifted past ±{args.tolerance}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
