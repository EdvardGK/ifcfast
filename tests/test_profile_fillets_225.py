"""GH #225 — parameterised section profiles carry their fillets.

Before the fix every ``IfcIShapeProfileDef`` (and the L/U/T/Z siblings)
was meshed as its sharp-cornered polygon: rolled sections under-reported
volume by 3–5 % with ``volume_reliable=True`` (HI90_RIB: all 20 IShape
beams, G55_RIB: all 10). ``IfcRoundedRectangleProfileDef`` was a plain
rectangle, and ``IfcCShapeProfileDef`` / ``IfcRectangleHollowProfileDef``
/ ``IfcAsymmetricIShapeProfileDef`` did not mesh at all.

Synthetic fixtures are the right tool here: the truth is the analytic
section area (sharp area ± one fillet correction per rounded corner), so
each beam is a 1 m extrusion of one profile and its volume must equal
``area × 1 m``. ifcopenshell 0.8.5 is the second voice: it tessellates
each fillet with a few chords and no area correction, so it sits a little
off the analytic value; ifcfast must sit closer.
"""

from __future__ import annotations

import math

import pytest

import ifcfast

Q = 1.0 - math.pi / 4.0
LENGTH_MM = 1000.0


def fillet(r: float, theta: float) -> float:
    """Area between two edges meeting at ``theta`` and their tangent arc."""
    return r * r * (1.0 / math.tan(theta / 2.0) - (math.pi - theta) / 2.0)


def _ipn200() -> tuple[str, float]:
    b, h, tw, tf, r1, r2, k = 90.0, 200.0, 7.5, 11.3, 7.5, 4.5, 0.14
    reach = (b - tw) / 2.0
    sharp = 2 * b * tf + (h - 2 * tf) * tw + 2 * reach * reach * k
    theta = math.pi / 2 + math.atan(k)
    area = sharp + 4 * fillet(r1, theta) - 4 * fillet(r2, theta)
    return (
        f"IFCISHAPEPROFILEDEF(.AREA.,'IPN200',$,{b},{h},{tw},{tf},{r1},{r2},{math.atan(k)!r})",
        area,
    )


def _upn200() -> tuple[str, float]:
    h, b, tw, tf, r1, r2, k = 200.0, 75.0, 8.5, 11.5, 11.5, 6.0, 0.08

    def m(x: float) -> float:
        return (x - b / 2) ** 2 / 2

    sharp = h * tw + 2 * ((b - tw) * tf - k * (m(b) - m(tw)))
    theta = math.pi / 2 + math.atan(k)
    area = sharp + 2 * fillet(r1, theta) - 2 * fillet(r2, theta)
    return (
        f"IFCUSHAPEPROFILEDEF(.AREA.,'UPN200',$,{h},{b},{tw},{tf},{r1},{r2},{math.atan(k)!r})",
        area,
    )


# name -> (IFC4 profile, analytic area mm², compare with ifcopenshell?)
# Not compared with ifcopenshell: Z (ifcfast's FlangeWidth reading differs
# from IfcOpenShell's, GH #237 — a different section) and the asymmetric I
# (ifcopenshell 0.8.5 raises "Failed to process shape" on IFC4
# IfcAsymmetricIShapeProfileDef with or without radii / slopes).
PROFILES: dict[str, tuple[str, float, bool]] = {
    "IPE200": (
        "IFCISHAPEPROFILEDEF(.AREA.,'IPE200',$,100.,200.,5.6,8.5,12.,$,$)",
        2 * 100 * 8.5 + (200 - 17) * 5.6 + 4 * Q * 144,
        True,
    ),
    "HEA200": (
        "IFCISHAPEPROFILEDEF(.AREA.,'HEA200',$,200.,190.,6.5,10.,18.,$,$)",
        2 * 200 * 10 + (190 - 20) * 6.5 + 4 * Q * 324,
        True,
    ),
    "IPN200": (*_ipn200(), True),
    "UPN200": (*_upn200(), True),
    "L100x10": (
        "IFCLSHAPEPROFILEDEF(.AREA.,'L100x10',$,100.,100.,10.,12.,6.,$)",
        190 * 10 + Q * (144 - 2 * 36),
        True,
    ),
    "T100": (
        "IFCTSHAPEPROFILEDEF(.AREA.,'T100',$,100.,100.,11.,11.,11.,5.5,3.,$,$)",
        100 * 11 + 89 * 11 + 2 * Q * (121 - 5.5**2 - 9),
        True,
    ),
    "Z200": (
        "IFCZSHAPEPROFILEDEF(.AREA.,'Z200',$,200.,80.,8.,8.,8.,4.)",
        200 * 8 + 2 * 80 * 8 + 2 * Q * (64 - 16),
        False,
    ),
    "C200x75x20x2.5": (
        "IFCCSHAPEPROFILEDEF(.AREA.,'C200',$,200.,75.,2.5,20.,2.5)",
        2 * 75 * 2.5 + 195 * 2.5 + 2 * 17.5 * 2.5 + 4 * Q * (2.5**2 - 5.0**2),
        True,
    ),
    "RHS100x50x4": (
        "IFCRECTANGLEHOLLOWPROFILEDEF(.AREA.,'RHS100x50x4',$,100.,50.,4.,4.,8.)",
        (100 * 50 - 4 * Q * 64) - (92 * 42 - 4 * Q * 16),
        True,
    ),
    "RoundedRect300x200r20": (
        "IFCROUNDEDRECTANGLEPROFILEDEF(.AREA.,$,$,300.,200.,20.)",
        300 * 200 - 4 * Q * 400,
        True,
    ),
    "AsymI": (
        "IFCASYMMETRICISHAPEPROFILEDEF(.AREA.,$,$,200.,300.,8.,15.,12.,150.,10.,10.,$,$,$,$)",
        200 * 15 + 150 * 10 + 275 * 8 + 2 * Q * (144 + 100),
        False,  # ifcopenshell 0.8.5 fails on every IFC4 asymmetric I
    ),
}

# The corpus case of the issue, verbatim (HI90_RIB, Archicad IFC2X3).
HEA120_2X3 = (
    "IFCISHAPEPROFILEDEF(.AREA.,'HEA120',$,120.00000000000738,114.00000000000007,"
    "5.0000000000161888,7.999999999998181,11.999999999998197)",
    2 * 120 * 8 + (114 - 16) * 5 + 4 * Q * 144,
)


def _guid(i: int) -> str:
    return f"0GH225Profile{i:02d}AAAAAAA"[:22]


def _write_ifc(path, schema: str, profiles: list[str]) -> list[str]:
    lines = [
        "#1=IFCPROJECT('0GH225ProjectAAAAAAAAA',$,'p',$,$,$,$,(#10),#2);",
        "#2=IFCUNITASSIGNMENT((#3,#4));",
        "#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);",
        "#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);",
        "#5=IFCCARTESIANPOINT((0.,0.,0.));",
        "#6=IFCAXIS2PLACEMENT3D(#5,$,$);",
        "#7=IFCDIRECTION((0.,0.,1.));",
        "#10=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,#6,$);",
        "#11=IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Body','Model',*,*,*,*,#10,$,.MODEL_VIEW.,$);",
    ]
    guids = []
    for i, prof in enumerate(profiles):
        n = 100 + 10 * i
        g = _guid(i)
        guids.append(g)
        lines += [
            f"#{n}=IFCCARTESIANPOINT(({i * 1000.0},0.,0.));",
            f"#{n + 1}=IFCAXIS2PLACEMENT3D(#{n},$,$);",
            f"#{n + 2}=IFCLOCALPLACEMENT($,#{n + 1});",
            f"#{n + 3}={prof};",
            f"#{n + 4}=IFCEXTRUDEDAREASOLID(#{n + 3},#6,#7,{LENGTH_MM});",
            f"#{n + 5}=IFCSHAPEREPRESENTATION(#11,'Body','SweptSolid',(#{n + 4}));",
            f"#{n + 6}=IFCPRODUCTDEFINITIONSHAPE($,$,(#{n + 5}));",
            f"#{n + 7}=IFCBEAM('{g}',$,'b{i}',$,$,#{n + 2},#{n + 6},$"
            + (",$);" if schema == "IFC4" else ");"),
        ]
    path.write_text(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');\n"
        "FILE_NAME('gh225.ifc','2026-10-13T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');\n"
        f"FILE_SCHEMA(('{schema}'));\nENDSEC;\nDATA;\n"
        + "\n".join(lines)
        + "\nENDSEC;\nEND-ISO-10303-21;\n"
    )
    return guids


def _fast_volumes(path) -> dict:
    q = ifcfast.open(path, use_cache=False, write_cache=False).mesh_qto()
    q = q[0] if isinstance(q, tuple) else q
    return {r.guid: r for r in q.itertuples()}


@pytest.fixture(scope="module")
def ifc4_file(tmp_path_factory):
    path = tmp_path_factory.mktemp("gh225") / "profiles_ifc4.ifc"
    guids = _write_ifc(path, "IFC4", [p for p, _, _ in PROFILES.values()])
    return path, dict(zip(PROFILES, guids))


@pytest.mark.parametrize("name", sorted(PROFILES))
def test_volume_equals_analytic_section_area(ifc4_file, name):
    path, guids = ifc4_file
    row = _fast_volumes(path)[guids[name]]
    want = PROFILES[name][1] * LENGTH_MM * 1e-9
    assert row.volume_m3 == pytest.approx(want, rel=1e-4), name
    assert row.mesh_quality == "closed"
    assert bool(row.volume_reliable)


def test_hea120_ifc2x3_corpus_profile(tmp_path):
    """The issue's HI90 STEP line: was −4.88 % (the 12-gon), now exact."""
    path = tmp_path / "hea120.ifc"
    (guid,) = _write_ifc(path, "IFC2X3", [HEA120_2X3[0]])
    row = _fast_volumes(path)[guid]
    assert row.volume_m3 == pytest.approx(HEA120_2X3[1] * 1e-6, rel=1e-4)


def test_closer_to_analytic_than_ifcopenshell(ifc4_file):
    np = pytest.importorskip("numpy")
    ifcopenshell = pytest.importorskip("ifcopenshell")
    geom = pytest.importorskip("ifcopenshell.geom")
    path, guids = ifc4_file
    fast = _fast_volumes(path)
    f = ifcopenshell.open(str(path))
    settings = geom.settings()
    settings.set("use-world-coords", True)
    rows = []
    for name, (_, area, compare) in PROFILES.items():
        if not compare:
            continue
        shape = geom.create_shape(settings, f.by_guid(guids[name]))
        v = np.asarray(shape.geometry.verts, dtype=np.float64).reshape(-1, 3)
        fa = np.asarray(shape.geometry.faces, dtype=np.int64).reshape(-1, 3)
        c = v - v.mean(0)
        ios = abs(np.einsum("ij,ij->i", c[fa[:, 0]], np.cross(c[fa[:, 1]], c[fa[:, 2]])).sum()) / 6.0
        want = area * LENGTH_MM * 1e-9
        got = fast[guids[name]].volume_m3
        rows.append((name, (got / want - 1) * 100, (ios / want - 1) * 100))
        # Same section on both sides (not a convention mismatch) …
        assert abs(ios / want - 1) < 0.01, f"{name}: ifcopenshell {ios} vs analytic {want}"
        # … and ifcfast is the closer one.
        assert abs(got - want) < abs(ios - want), f"{name}: fast {got} ios {ios} analytic {want}"
    print("\nprofile                 fast-vs-analytic %   ios-vs-analytic %")
    for name, a, b in rows:
        print(f"{name:24s} {a:+18.5f} {b:+18.5f}")
    assert len(rows) == sum(1 for *_, c in PROFILES.values() if c)
