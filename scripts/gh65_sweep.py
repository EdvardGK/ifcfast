#!/usr/bin/env python3
"""GH #65 full sweep — ifcfast mesh_qto vs ifcopenshell ground truth on
Sannergata ARK_E, focused on half-space-cut walls.

ios ground truth = volume of the shape AFTER booleans are applied.
Reports per-GUID % diff, worst offenders, and how many over-reporters
remain. Usage: gh65_sweep.py [n_worst]
"""
import sys
import ifcfast
import ifcopenshell
import ifcopenshell.geom as geom
import ifcopenshell.util.shape as ushape

ARK = ("/home/edkjo/workspace/skiplum/client-projects/10008-sannergata-2/"
       "acc-tree_Sannergata 2/1. IFC/prosjektering/Sannergata_bygg_ARK_E.ifc")
N_WORST = int(sys.argv[1]) if len(sys.argv) > 1 else 20

print("ios: extracting wall shapes...", file=sys.stderr)
f = ifcopenshell.open(ARK)
settings = geom.settings()
settings.set("use-world-coords", True)
ios_vol = {}
walls = f.by_type("IfcWall") + f.by_type("IfcWallStandardCase")
it = geom.iterator(settings, f, num_threads=8, include=walls)
if it.initialize():
    while True:
        sh = it.get()
        try:
            ios_vol[sh.guid] = ushape.get_volume(sh.geometry)
        except Exception:
            pass
        if not it.next():
            break
print(f"ios: {len(ios_vol)} wall volumes", file=sys.stderr)

print("ifcfast: mesh_qto...", file=sys.stderr)
m = ifcfast.open(ARK)
prod, _ = m.mesh_qto(cut_openings=True)
iff = {r.guid: r for r in prod.itertuples()}

recs = []
for guid, ios_v in ios_vol.items():
    r = iff.get(guid)
    if r is None or ios_v is None or ios_v <= 1e-9:
        continue
    pct = (r.volume_m3 - ios_v) / ios_v * 100.0
    recs.append((pct, guid, ios_v, r.volume_m3,
                 getattr(r, "volume_method", "?"), getattr(r, "mesh_quality", "?")))

recs.sort(key=lambda x: abs(x[0]), reverse=True)
ok = sum(1 for x in recs if abs(x[0]) <= 1.0)
print(f"\nifcfast {ifcfast.__version__} | {len(recs)} matched walls | "
      f"{ok}/{len(recs)} within +/-1% ({ok/len(recs)*100:.1f}%)")
print(f"\nWorst {N_WORST} by |%diff|:")
hdr = f"{'%diff':>9} {'guid':22} {'ios':>9} {'vol_m3':>9} {'method':>14} {'quality':>10}"
print(hdr); print("-" * len(hdr))
for pct, guid, ios_v, vol, method, qual in recs[:N_WORST]:
    print(f"{pct:8.1f}% {guid:22} {ios_v:9.3f} {vol:9.3f} {method:>14} {qual:>10}")

over = [x for x in recs if x[0] > 1.0]
print(f"\nover-reporters (>+1%): {len(over)} | within +/-1%: {ok} | "
      f"under (<-1%): {len(recs)-ok-len(over)}")
