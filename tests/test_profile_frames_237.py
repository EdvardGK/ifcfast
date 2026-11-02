"""GH #237 — L / U / T / Z sections sit in the IFC spec frame.

The spec (IFC2x3 onward, every schema we read) puts the origin of each
parameterised profile at the centre of its bounding box, in the
orientation of the entity's figure. Before the fix ifcfast built L, U and
T in a corner frame (an L100x60 sat 30 mm x 50 mm off) and the Z
centred on the web with its bottom at y = 0, flanges mirrored and
``FlangeWidth`` read as the length beyond each web face.

Z ``FlangeWidth`` reading. The IfcZShapeProfileDef figure (and DIN 1027
steel tables) measure ``b`` from the web's back face to the flange toe,
so the overall width is ``2·b − tw`` and the bounding box is centred on
the web. IfcOpenShell 0.8.5 runs each flange from the web's back face to
``x = ±b`` instead (overall ``2·b``, each flange ``tw/2`` longer). We
follow the spec; ``test_bbox_matches_ifcopenshell`` pins that deviation.

Truth is analytic: each beam is a 1000 mm extrusion along +z of one
profile, element n at x = n·1000 mm, so its 2D bbox / centroid / area in
the profile plane are known in closed form.
"""

from __future__ import annotations

import math
from pathlib import Path

import numpy as np
import pytest

import ifcfast

FIX = Path(__file__).parent / "fixtures" / "gh237_lutz_frames.ifc"
Q = 1.0 - math.pi / 4.0
LENGTH_MM = 1000.0


def _rects(rs):
    """(area, centroid) of a union of disjoint axis-aligned rectangles."""
    a = sum((x1 - x0) * (y1 - y0) for x0, y0, x1, y1 in rs)
    cx = sum((x1 - x0) * (y1 - y0) * (x0 + x1) / 2 for x0, y0, x1, y1 in rs) / a
    cy = sum((x1 - x0) * (y1 - y0) * (y0 + y1) / 2 for x0, y0, x1, y1 in rs) / a
    return a, (cx, cy)


# L100x60x10 in the spec frame: corner at (−30, −50).
_L = _rects([(-30, -50, 30, -40), (-30, -40, -20, 50)])
# T: flange at +y, web toward −y.
_T = _rects([(-60, 41, 60, 50), (-5.5, -50, 5.5, 41)])

# guid -> (2D bbox (xmin, ymin, xmax, ymax) mm, area mm², centroid or None,
#          Z web thickness or None)
SPEC: dict[str, tuple] = {
    "1GH237LShapeSharp00000": ((-30, -50, 30, 50), _L[0], _L[1], None),
    "1GH237LShapeRadii00000": (
        (-45, -75, 45, 75),
        (150 + 90 - 10) * 10 + Q * (12**2 - 2 * 6**2),
        None,
        None,
    ),
    "1GH237UShapeRadii00000": (
        (-37.5, -100, 37.5, 100),
        200 * 8.5 + 2 * (75 - 8.5) * 11.5 + 2 * Q * (11.5**2 - 6**2),
        (None, 0.0),  # symmetric about the x axis
        None,
    ),
    "1GH237TShapeSharp00000": ((-60, -50, 60, 50), _T[0], _T[1], None),
    # Z: overall width 2·80 − 8 = 152, point-symmetric about the origin.
    "1GH237ZShapeSharp00000": ((-76, -100, 76, 100), 200 * 8 + 2 * (80 - 8) * 10, (0.0, 0.0), 8.0),
    "1GH237ZShapeRadii00000": (
        (-76, -100, 76, 100),
        200 * 8 + 2 * (80 - 8) * 8 + 2 * Q * (8**2 - 4**2),
        (0.0, 0.0),
        8.0,
    ),
    # The L moved by its own Position (20, −30).
    "1GH237LShapePosit00000": (
        (-10, -80, 50, 20),
        _L[0],
        (_L[1][0] + 20, _L[1][1] - 30),
        None,
    ),
}
ORDER = list(SPEC)  # element n sits at x = n·1000 mm


def _fast_meshes() -> dict:
    m = ifcfast.open(str(FIX), use_cache=False, write_cache=False)
    ms = m.iter_meshes()  # metres
    shift = np.asarray(ms.global_shift, dtype=np.float64)
    out = {}
    for x in ms:
        if len(x.faces) == 0:
            continue
        v = x.vertices.astype(np.float64) + shift
        f = x.faces.astype(np.int64).reshape(-1, 3)
        out[x.guid] = (v, f)
    return out


def _vol_centroid(v, f):
    a, b, c = v[f[:, 0]], v[f[:, 1]], v[f[:, 2]]
    six = np.einsum("ij,ij->i", a, np.cross(b, c))
    vol = six.sum() / 6.0
    cen = (six[:, None] * (a + b + c)).sum(0) / (24.0 * vol)
    return abs(vol), cen


@pytest.fixture(scope="module")
def fast():
    return _fast_meshes()


@pytest.mark.parametrize("guid", ORDER)
def test_section_in_spec_frame(fast, guid):
    bbox, area, centroid, _ = SPEC[guid]
    v, f = fast[guid]
    x0 = ORDER.index(guid) * LENGTH_MM
    mn, mx = v.min(0) * 1000.0, v.max(0) * 1000.0  # mm
    got = (mn[0] - x0, mn[1], mx[0] - x0, mx[1])
    assert got == pytest.approx(bbox, abs=1e-3), f"{guid}: 2D bbox {got} vs spec {bbox}"
    assert (mn[2], mx[2]) == pytest.approx((0.0, LENGTH_MM), abs=1e-3)
    vol, cen = _vol_centroid(v, f)
    assert vol == pytest.approx(area * LENGTH_MM * 1e-9, rel=1e-4), guid
    if centroid is not None:
        cx, cy = centroid
        if cx is not None:
            assert cen[0] * 1000.0 - x0 == pytest.approx(cx, abs=1e-2), guid
        assert cen[1] * 1000.0 == pytest.approx(cy, abs=1e-2), guid


def test_bbox_matches_ifcopenshell(fast):
    """Per-element world bbox vs IfcOpenShell 0.8.5 to 1e-6 m. Z: depth and
    length agree; the width differs by exactly tw/2 per side (IfcOpenShell
    reads FlangeWidth to x = ±b, the spec to the web's back face)."""
    ifcopenshell = pytest.importorskip("ifcopenshell")
    geom = pytest.importorskip("ifcopenshell.geom")
    f = ifcopenshell.open(str(FIX))
    settings = geom.settings()
    settings.set("use-world-coords", True)
    for guid in ORDER:
        shape = geom.create_shape(settings, f.by_guid(guid))
        iv = np.asarray(shape.geometry.verts, dtype=np.float64).reshape(-1, 3)
        v, _ = fast[guid]
        d_mn, d_mx = v.min(0) - iv.min(0), v.max(0) - iv.max(0)
        tw = SPEC[guid][3]
        if tw is None:
            assert np.abs(np.r_[d_mn, d_mx]).max() <= 1e-6, f"{guid}: {d_mn} {d_mx}"
        else:
            half = tw / 2 * 1e-3
            assert d_mn[0] == pytest.approx(half, abs=1e-6), guid
            assert d_mx[0] == pytest.approx(-half, abs=1e-6), guid
            assert np.abs(np.r_[d_mn[1:], d_mx[1:]]).max() <= 1e-6, f"{guid}: {d_mn} {d_mx}"
