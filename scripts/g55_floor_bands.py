#!/usr/bin/env python3
"""G55_ARK floor banding — by GEOMETRY (placement Z), not relationships.

Opens the model WITHOUT the geometry kernel (no tessellation → low RAM),
reads each element's world placement matrix, and buckets ceilings / slabs
/ walls by their world-Z elevation. The Z clusters are the floors; the
plenum (routable) sits between a suspended ceiling and the slab above.
"""
import sys
import resource
import ifcopenshell
import ifcopenshell.util.placement as P

ARK = ("/home/edkjo/workspace/skiplum/client-projects/10027-grønland-55/"
       "acc-tree_Grønland 55/1. IFC/G55_ARK.ifc")

print("opening (no geometry)...", file=sys.stderr)
f = ifcopenshell.open(ARK)
mb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024
print(f"opened; peak RSS so far {mb:.0f} MB", file=sys.stderr)


def z_of(el):
    """World-Z of the element's placement origin, in mm."""
    if not el.ObjectPlacement:
        return None
    try:
        m = P.get_local_placement(el.ObjectPlacement)
        return float(m[2][3])
    except Exception:
        return None


classes = ["IfcSlab", "IfcCovering", "IfcWall", "IfcWallStandardCase"]
zs = {c: [] for c in classes}
for c in classes:
    for el in f.by_type(c):
        if c == "IfcCovering" and getattr(el, "PredefinedType", None) != "CEILING":
            continue
        z = z_of(el)
        if z is not None:
            zs[c].append(z)

# Merge wall variants.
walls = zs["IfcWall"] + zs["IfcWallStandardCase"]
ceil = zs["IfcCovering"]
slabs = zs["IfcSlab"]
print(f"\nelements with placement Z: slabs={len(slabs)} ceilings={len(ceil)} walls={len(walls)}")


def bands(vals, tol_mm=300.0):
    """Cluster Z values into bands within tol; return (z_centre, count)."""
    out = []
    for z in sorted(vals):
        if out and abs(z - out[-1][0]) <= tol_mm:
            n = out[-1][1] + 1
            # running mean
            out[-1] = ((out[-1][0] * out[-1][1] + z) / n, n)
        else:
            out.append((z, 1))
    return out


print("\n=== SLAB elevations (floor dividers) — the floor levels ===")
print(f"{'z (m)':>9} {'count':>6}")
for z, n in bands(slabs):
    print(f"{z/1000:9.2f} {n:6d}")

print("\n=== CEILING elevations (suspended ceilings) ===")
print(f"{'z (m)':>9} {'count':>6}")
for z, n in bands(ceil):
    print(f"{z/1000:9.2f} {n:6d}")

# Quick plenum read: for each ceiling band, the gap up to the next slab.
slab_bands = [z for z, _ in bands(slabs)]
print("\n=== plenum gap (ceiling band -> next slab above), metres ===")
for cz, cn in bands(ceil):
    above = [z for z in slab_bands if z > cz + 50]
    if above:
        gap = (min(above) - cz) / 1000
        print(f"  ceiling z={cz/1000:6.2f}  ->  slab z={min(above)/1000:6.2f}   plenum {gap:5.2f} m  ({cn} ceilings)")

mb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024
print(f"\npeak RSS {mb:.0f} MB", file=sys.stderr)
