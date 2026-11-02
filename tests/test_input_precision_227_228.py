"""GH #227 / #228 — two input-side mesh bugs found on KNM_LARK.

* ``far_origin_extrusion_position_mm.ifc`` (GH #227): the georeference
  sits on ``IfcExtrudedAreaSolid.Position`` under identity placements.
  Baked into f32 local vertices it put every corner on the 8 mm / 128 mm
  f32 grid at NTM magnitude — up to 62 mm off. The translation now rides
  on ``rep_origin`` in f64, so ``m.mesh()`` must hit the analytic corners
  to well under a millimetre.
* ``faceset_orphan_points.ifc`` (GH #228): ``IfcCartesianPointList3D``
  points no face references (Revit toposolids) widened the bbox, the
  QTO AABB / prism bound and the substrate bbox columns by metres. They
  are now dropped at the source.
"""

from __future__ import annotations

from pathlib import Path

import numpy as np

import ifcfast

FIX = Path(__file__).parent / "fixtures"
FAR_EXT = FIX / "far_origin_extrusion_position_mm.ifc"
ORPHANS = FIX / "faceset_orphan_points.ifc"
RAMP = "3gvBAx9cjCzg2jOt2$eqfj"
TRI = "2abeJIBM57sAgviUE16td7"
POLY = "1OrphanPts0Fixture0007"


def _open(path):
    return ifcfast.open(path, use_cache=False, write_cache=False)


def _ramp_corners_m():
    loc = np.array([92636623.58515, 1247354037.73415, 121000.0])
    x = np.array([0.79607, 0.60521, 0.0])
    x /= np.linalg.norm(x)
    z = np.array([0.0, 0.0, 1.0])
    y = np.cross(z, x)
    hx, hy = 37454.59 / 2, 2085.0 / 2
    out = [
        loc + sx * hx * x + sy * hy * y + h * z
        for sx in (-1, 1)
        for sy in (-1, 1)
        for h in (0.0, 200.0)
    ]
    return np.array(out) / 1000.0


def test_far_extrusion_position_corners_mm_accurate():
    m = _open(FAR_EXT)
    g = m.mesh(RAMP)
    v = np.asarray(g.vertices, dtype=np.float64)
    corners = _ramp_corners_m()
    d = np.linalg.norm(v[:, None, :] - corners[None, :, :], axis=2)
    # every corner hit, every vertex a corner — sub-millimetre (pre-fix: 62 mm)
    assert d.min(axis=0).max() < 1e-4
    assert d.min(axis=1).max() < 1e-4
    lo, hi = corners.min(axis=0), corners.max(axis=0)
    assert np.allclose(v.min(axis=0), lo, atol=1e-4)
    assert np.allclose(v.max(axis=0), hi, atol=1e-4)


def test_far_extrusion_position_qto_volume():
    df, _ = _open(FAR_EXT).mesh_qto(cut_openings=False)
    row = df.set_index("guid").loc[RAMP]
    want = 37.45459 * 2.085 * 0.2
    assert row.mesh_quality == "closed"
    assert abs(row.volume_m3 - want) / want < 1e-4


def test_orphan_points_excluded_from_mesh_bbox():
    m = _open(ORPHANS)
    for guid, x0 in ((TRI, 0.0), (POLY, 3.0)):
        g = m.mesh(guid)
        v = np.asarray(g.vertices)
        f = np.asarray(g.faces)
        assert len(v) == 8, f"{guid}: orphan vertex kept"
        assert np.unique(f).size == len(v)
        assert np.allclose(v.min(axis=0), [x0, 0, 0], atol=1e-6)
        assert np.allclose(v.max(axis=0), [x0 + 1, 1, 1], atol=1e-6)


def test_orphan_points_excluded_from_qto():
    df, _ = _open(ORPHANS).mesh_qto(cut_openings=False)
    df = df.set_index("guid")
    for guid in (TRI, POLY):
        row = df.loc[guid]
        assert row.mesh_quality == "closed"
        assert abs(row.volume_m3 - 1.0) < 1e-6
        # AABB was 1 x 1 x 10.2 m (A) / 1 x 1 x 10 m (B) with the orphan
        assert abs(row.aabb_volume_m3 - 1.0) < 1e-6
