#!/usr/bin/env python3
"""GH #65 diagnostic — per-wall GH #60 column breakdown vs ifcopenshell."""
import ifcfast

ARK = ("/home/edkjo/workspace/skiplum/client-projects/10008-sannergata-2/"
       "acc-tree_Sannergata 2/1. IFC/prosjektering/Sannergata_bygg_ARK_E.ifc")

CASES = [
    ("2tg5mWEtP44Aw5N0_z0wea", 0.702, "IfcHalfSpaceSolid x2  (was +517%)"),
    ("2Nf9lR2yz4n8M7t01TygaQ", 1.250, "IfcHalfSpaceSolid x13 (#39 headline, was +136%)"),
    ("3qxjXn6GP6svyMMSugQ6io", 0.002, "IfcHalfSpaceSolid x3  (was +50%)"),
    ("1c4SzqA0bFqxLl2uTAFrxY", 1.197, "IfcHalfSpaceSolid x3  (was +38%)"),
    ("0LHLBziIr9HRkxlSlK0Pw6", 8.980, "IfcHalfSpaceSolid x2  (was +6.4%)"),
    ("0LHLBziIr9HRkxlSlK0Pe9", 4.549, "IfcHalfSpaceSolid x2  (was +6.2%)"),
    ("3_6AbaPP55CwroXTQjRwPB", 1.885, "IfcPolygonalBoundedHalfSpace CONTROL (+0.0%)"),
]
WANT = {g for g, *_ in CASES}


def col(r, name, default=float("nan")):
    return getattr(r, name, default)


print(f"ifcfast {ifcfast.__version__}")
m = ifcfast.open(ARK)
prod, _surf = m.mesh_qto(cut_openings=True)
rows = {r.guid: r for r in prod.itertuples() if r.guid in WANT}

hdr = f"{'guid':22} {'ios':>8} {'vol_m3':>8} {'mesh_m3':>8} {'%diff':>8} {'method':>14} {'quality':>10}"
print(hdr)
print("-" * len(hdr))
for guid, ios, label in CASES:
    r = rows.get(guid)
    if r is None:
        print(f"{guid:22} {ios:8.3f}  <NOT MESHED>   {label}")
        continue
    vol = col(r, "volume_m3")
    mesh = col(r, "volume_mesh_m3", vol)
    pct = (vol - ios) / ios * 100.0 if ios > 1e-9 else float("nan")
    method = col(r, "volume_method", "n/a")
    qual = col(r, "mesh_quality", "?")
    print(f"{guid:22} {ios:8.3f} {vol:8.3f} {mesh:8.3f} {pct:7.1f}% "
          f"{str(method):>14} {str(qual):>10}   {label}")
