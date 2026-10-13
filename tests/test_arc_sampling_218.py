"""GH #218 — large-radius profile arcs: sagitta-honouring chord count,
authored endpoints kept.

Both fixtures are extracts of KNM_ARK.ifc (Revit 2026 IFC4, mm units,
ACC v3), written by ifcopenshell with only the Body representation kept
and the IfcPerson anonymised. Every product is a plain
``IfcExtrudedAreaSolid`` over an ``IfcIndexedPolyCurve`` with
``IfcArcIndex`` segments of R = 5–98 m.

Before the fix the arc sampler capped the chord count at 32 per full turn
(2 chords for a 16.5° arc at R = 98 m, 253 mm sagitta) and scaled every
sample — authored endpoints included — by the sector-area factor, jogging
them up to 169 mm outward:

* ``gh218_roof_min.ifc`` — one IfcRoof, one R = 5.2 m arc: bbox 10.0 mm
  off ifcopenshell.
* ``gh218_arcs.ifc`` — the reporter's covering (82.6 mm), the KNM slab
  ``…Uov$SX9`` (146 mm) and the 1 mm crescent-sliver wall ``…Uwv$SXu``
  whose sampled profile self-intersected: +13.7 % volume, still
  ``volume_reliable=True``.

Expected bounds / volumes are ifcopenshell 0.8.5 (``use-world-coords``,
iterator over the full KNM_ARK.ifc, cached as ``ios085_KNM.json`` in the
GH #218 investigation; the fixtures reproduce them to 3e-11 m). Where
ifcopenshell is importable the test also differentiates against it
directly on the fixture.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import ifcfast

FIXTURES = Path(__file__).parent / "fixtures"

# guid -> (fixture, ifcopenshell world bbox [m] xmin..zmax, volume [m3])
EXPECTED = {
    "1li7i1yEj7LQoWD6FYBZK3": (  # IfcRoof, R = 5.2 m arc
        "gh218_roof_min.ifc",
        (92543.684263, 1247314.729809, 141.988499, 92553.748369, 1247321.437497, 142.589444),
        2.5129,
    ),
    "14$lUHUeL4xhmfdOYot5rA": (  # IfcCovering, R = 96.9 m arcs
        "gh218_arcs.ifc",
        (92477.101337, 1247260.789419, 126.0, 92604.652004, 1247367.374316, 126.02),
        37.6541,
    ),
    "0DzHyetFz01QAC_Uov$SX9": (  # IfcSlab, R ≈ 98 m arcs
        "gh218_arcs.ifc",
        (92577.809286, 1247242.442204, 127.1, 92603.92428, 1247265.617913, 127.5),
        88.48203,
    ),
    "0DzHyetFz01QAC_Uwv$SXu": (  # IfcWall, 1 mm crescent between two R ≈ 98 m arcs
        "gh218_arcs.ifc",
        (92577.809973, 1247248.175898, 123.6, 92599.819539, 1247265.617411, 127.1),
        28.92289,
    ),
}

BBOX_TOL_M = 1.0e-3
VOLUME_REL_TOL = 0.01


def _open(name: str):
    return ifcfast.open(FIXTURES / name, use_cache=False, write_cache=False)


def _bbox_and_volume(model, guid: str):
    np = pytest.importorskip("numpy")
    mesh = model.mesh(guid)
    assert mesh is not None, f"{guid} did not mesh"
    v = np.asarray(mesh.vertices, dtype=np.float64)
    f = np.asarray(mesh.faces, dtype=np.int64).reshape(-1, 3)
    c = v - v.mean(0)
    vol = float(np.einsum("ij,ij->i", c[f[:, 0]], np.cross(c[f[:, 1]], c[f[:, 2]])).sum() / 6.0)
    return np.r_[v.min(0), v.max(0)], vol


@pytest.mark.parametrize("guid", sorted(EXPECTED))
def test_bbox_and_volume_match_ifcopenshell(guid):
    np = pytest.importorskip("numpy")
    fixture, bbox, volume = EXPECTED[guid]
    got_bbox, got_vol = _bbox_and_volume(_open(fixture), guid)
    err = np.abs(got_bbox - np.asarray(bbox))
    assert err.max() < BBOX_TOL_M, (
        f"{guid}: bbox off ifcopenshell by {err.max() * 1e3:.2f} mm "
        f"(xmin..zmax {np.round(err * 1e3, 2).tolist()} mm)"
    )
    assert got_vol == pytest.approx(volume, rel=VOLUME_REL_TOL), (
        f"{guid}: volume {got_vol:.4f} m3 vs ifcopenshell {volume}"
    )


def test_crescent_sliver_wall_is_closed_and_volume_exact():
    """The wall whose sampled profile self-intersected: mesh_qto must
    report the analytic-scale volume from a closed shell, not +13.7 %."""
    guid = "0DzHyetFz01QAC_Uwv$SXu"
    q = _open("gh218_arcs.ifc").mesh_qto()
    q = q[0] if isinstance(q, tuple) else q
    row = q[q.guid == guid].iloc[0]
    assert row.volume_m3 == pytest.approx(EXPECTED[guid][2], rel=1e-3)
    assert row.mesh_quality == "closed"
    assert bool(row.volume_reliable)


@pytest.mark.parametrize("fixture", ["gh218_roof_min.ifc", "gh218_arcs.ifc"])
def test_differential_against_ifcopenshell(fixture):
    np = pytest.importorskip("numpy")
    ifcopenshell = pytest.importorskip("ifcopenshell")
    geom = pytest.importorskip("ifcopenshell.geom")
    f = ifcopenshell.open(str(FIXTURES / fixture))
    settings = geom.settings()
    settings.set("use-world-coords", True)
    model = _open(fixture)
    checked = 0
    for product in f.by_type("IfcProduct"):
        if not product.Representation:
            continue
        shape = geom.create_shape(settings, product)
        v = np.asarray(shape.geometry.verts, dtype=np.float64).reshape(-1, 3)
        fa = np.asarray(shape.geometry.faces, dtype=np.int64).reshape(-1, 3)
        c = v - v.mean(0)
        ios_vol = np.einsum("ij,ij->i", c[fa[:, 0]], np.cross(c[fa[:, 1]], c[fa[:, 2]])).sum() / 6.0
        got_bbox, got_vol = _bbox_and_volume(model, product.GlobalId)
        err = np.abs(got_bbox - np.r_[v.min(0), v.max(0)]).max()
        assert err < BBOX_TOL_M, f"{product.GlobalId}: bbox {err * 1e3:.2f} mm off ifcopenshell"
        assert got_vol == pytest.approx(ios_vol, rel=VOLUME_REL_TOL), product.GlobalId
        checked += 1
    assert checked == {"gh218_roof_min.ifc": 1, "gh218_arcs.ifc": 4}[fixture]
