"""Analytic oracle for circular extrusions: ifcfast volume_mesh_m3 vs
pi * (r_o^2 - r_i^2) * depth per element (GH #170).

Unlike the ifcopenshell class sweep this has no tessellation on the
reference side, so it proves *exactness*, not parity: with the
area-preserving sampling the ratio must be 1.0000 whatever the segment
count. Covers products whose Body is one IfcExtrudedAreaSolid
(directly or through one IfcMappedItem) over an
IfcCircleProfileDef, or an IfcArbitraryClosedProfileDef /
IfcArbitraryProfileDefWithVoids whose loops are two IfcTrimmedCurve
circle arcs or two IfcArcIndex arcs (Revit pipes, hollow or solid).

Usage (repo root, venv with ifcopenshell)::

    python -m tests.oracle.pipe_analytic MODEL.ifc [IfcPipeSegment]

Evidence on record (2026-09-07): Clinic_Plumbing 2 882 IfcFlowSegment,
median 1.00000; G55_RIV 10 704 hollow IfcPipeSegment, 10 700 within
0.5 %, p1..p99 = 1.0000 (the four outliers are centimetre stubs, GH #173).
"""
import sys, re, math, ifcopenshell, ifcfast
path = sys.argv[1]; want_entity = sys.argv[2] if len(sys.argv) > 2 else "IfcPipeSegment"
f = ifcopenshell.open(path)
m = ifcfast.open(path, use_cache=False, write_cache=False)
prod, _ = m.mesh_qto(cut_openings=False)
vol = dict(zip(prod.guid, prod.volume_mesh_m3))
qual = dict(zip(prod.guid, zip(prod.mesh_quality, prod.volume_method, prod.volume_reliable)))
scale = float(m.unit_scale or 1.0)
rows = []
for p in f.by_type(want_entity):
    rep = p.Representation
    if not rep: continue
    bodies = [r for r in rep.Representations if r.RepresentationIdentifier == "Body"]
    if len(bodies) != 1 or len(bodies[0].Items) != 1: continue
    it = bodies[0].Items[0]
    if it.is_a("IfcMappedItem"):
        src = it.MappingSource.MappedRepresentation
        if len(src.Items) != 1: continue
        it = src.Items[0]
    if not it.is_a("IfcExtrudedAreaSolid"): continue
    prof = it.SweptArea
    r = None
    if prof.is_a("IfcCircleProfileDef") and not prof.is_a("IfcCircleHollowProfileDef"):
        r = prof.Radius; kind = "circle"
    elif prof.is_a("IfcArbitraryClosedProfileDef"):
        c = prof.OuterCurve
        if c.is_a("IfcCompositeCurve") and len(c.Segments) == 2 and all(s.ParentCurve.is_a("IfcTrimmedCurve") and s.ParentCurve.BasisCurve.is_a("IfcCircle") for s in c.Segments):
            r = c.Segments[0].ParentCurve.BasisCurve.Radius; kind = "2 trimmed arcs"
        elif c.is_a("IfcIndexedPolyCurve") and c.Segments and all(s.is_a("IfcArcIndex") for s in c.Segments) and len(c.Segments) == 2:
            pts = c.Points.CoordList
            idx = list(c.Segments[0].wrappedValue) if hasattr(c.Segments[0], "wrappedValue") else list(c.Segments[0][0])
            (x1,y1),(x2,y2),(x3,y3) = (pts[i-1][:2] for i in idx[:3])
            d = 2*(x1*(y2-y3)+x2*(y3-y1)+x3*(y1-y2))
            if abs(d) < 1e-12: continue
            ux = ((x1*x1+y1*y1)*(y2-y3)+(x2*x2+y2*y2)*(y3-y1)+(x3*x3+y3*y3)*(y1-y2))/d
            uy = ((x1*x1+y1*y1)*(x3-x2)+(x2*x2+y2*y2)*(x1-x3)+(x3*x3+y3*y3)*(x2-x1))/d
            r = math.hypot(x1-ux, y1-uy); kind = "2 arc-index"
    if r is None: continue
    ri = 0.0
    if prof.is_a("IfcArbitraryProfileDefWithVoids"):
        if len(prof.InnerCurves) != 1: continue
        ic = prof.InnerCurves[0]
        if ic.is_a("IfcIndexedPolyCurve") and len(ic.Segments) == 2:
            pts = ic.Points.CoordList
            idx = list(ic.Segments[0].wrappedValue) if hasattr(ic.Segments[0], "wrappedValue") else list(ic.Segments[0][0])
            (x1,y1),(x2,y2),(x3,y3) = (pts[i-1][:2] for i in idx[:3])
            d_ = 2*(x1*(y2-y3)+x2*(y3-y1)+x3*(y1-y2))
            if abs(d_) < 1e-12: continue
            ux = ((x1*x1+y1*y1)*(y2-y3)+(x2*x2+y2*y2)*(y3-y1)+(x3*x3+y3*y3)*(y1-y2))/d_
            uy = ((x1*x1+y1*y1)*(x3-x2)+(x2*x2+y2*y2)*(x1-x3)+(x3*x3+y3*y3)*(x2-x1))/d_
            ri = math.hypot(x1-ux, y1-uy); kind = "2 arc-index hollow"
        else:
            continue
    d = it.Depth
    analytic = math.pi * ((r*scale)**2 - (ri*scale)**2) * (d*scale)
    v = vol.get(p.GlobalId)
    if v is None or analytic <= 0: continue
    rows.append((kind, r*scale, analytic, v, v/analytic, p.GlobalId))
import collections
by = collections.defaultdict(list)
for k, r, a, v, q, g in rows: by[k].append(q)
print(f"{want_entity}: {len(rows)} single-extrusion circular products checked")
for k, qs in by.items():
    qs.sort(); n=len(qs)
    pct = lambda f: qs[min(n-1, int(f*n))]
    print(f"  {k:20s} n={n:5d}  ratio: min {qs[0]:.5f} p1 {pct(0.01):.5f} p5 {pct(0.05):.5f} median {qs[n//2]:.5f} p95 {pct(0.95):.5f} p99 {pct(0.99):.5f} max {qs[-1]:.5f}")
    within = sum(1 for q in qs if abs(q-1) < 0.005); print(f"  {'':20s} within 0.5 %: {within}/{n} = {within/n:.1%}")
worst = sorted(rows, key=lambda t: abs(t[4]-1))[-6:]
for k, r, a, v, q, g in worst: print(f"  worst {g} r={r*1000:.1f}mm analytic={a:.7f} ifcfast={v:.7f} ratio={q:.4f} quality/method/reliable={qual.get(g)}")
