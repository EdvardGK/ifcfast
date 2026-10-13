"""GH #232 / #233 — cross-product opening cut on real Archicad walls.

Fixtures are single-wall subsets of HI90_ARK_MMI700.ifc (Archicad 29,
IFC2X3), anonymised. The ifcopenshell values below are provenance, copied
from the v0.6.4 discovery sweep's ifcopenshell 0.8.5 dump
(`scratch/discovery/HI90_ARK_MMI700/HI90_ARK_MMI700_ios_bbox_cut.json`,
default settings, openings applied) — ifcopenshell is never run here.

* #232: an IfcOpeningElement whose Body has 3 touching items was fed to
  Manifold as ONE concatenated mesh → the wall came out at 1.8204 m³
  (−33.8 %). `gh232_opening_split.ifc` is the same wall with the opening
  rewritten as 3 single-item openings (same union) — the control.
* #233: a cutter face flush with a host face left a zero-thickness sheet
  (bbox metres off) or, for a wall its openings fully consume, a phantom
  "closed" residue product.
"""
from pathlib import Path

import numpy as np
import pytest

import ifcfast

FIX = Path(__file__).parent / "fixtures"

# ifcopenshell 0.8.5 (cut mode) — provenance, see module docstring.
IOS = {
    "0fv7j7e6rCzBbEyxXhX0$k": dict(
        vol=2.748479200000412,
        mn=[145.7094581360987, 124.94130747036733, 17.7],
        mx=[149.39608047741262, 130.5750588767873, 20.7],
    ),
    "1o1uaQnuP0xvnggRxTxvO0": dict(
        vol=0.3595999999999609,
        mn=[133.7788756015481, 122.76308214676251, 27.400000000000002],
        mx=[134.80943187307594, 123.47326596577349, 30.500000000000004],
    ),
    "0$1AhHhaXB7uKyZ3CA1fKl": dict(
        vol=4.373600000000013,
        mn=[176.9941974873744, 142.5173065306925, 23.650000000000002],
        mx=[181.14102255767352, 145.58211897834036, 24.950000000000003],
    ),
}


def _cut_wall(fixture: str, guid: str):
    """(volume m³, world bbox min, max, triangles) of `guid` with openings cut,
    or None when the product is not emitted."""
    m = ifcfast.open(str(FIX / fixture))
    ms = m.iter_meshes(cut_openings=True)  # metres (unit="m" default)
    shift = np.asarray(ms.global_shift, dtype=np.float64)
    for x in ms:
        if x.guid != guid or len(x.faces) == 0:
            continue
        v = x.vertices.astype(np.float64) + shift
        f = x.faces.astype(np.int64).reshape(-1, 3)
        a, b, c = v[f[:, 0]], v[f[:, 1]], v[f[:, 2]]
        vol = float(np.einsum("ij,ij->i", a, np.cross(b, c)).sum() / 6.0)
        return vol, v.min(0), v.max(0), len(f)
    return None


def _assert_bbox_mm(mn, mx, ref, tol_m=1e-3):
    d = np.abs(np.r_[mn - ref["mn"], mx - ref["mx"]]).max()
    assert d <= tol_m, f"world bbox {d * 1000:.2f} mm off ifcopenshell"


def test_three_item_opening_matches_split_opening_232():
    g = "0fv7j7e6rCzBbEyxXhX0$k"
    three = _cut_wall("gh232_opening_3item.ifc", g)
    split = _cut_wall("gh232_opening_split.ifc", g)
    assert three is not None and split is not None
    assert three[0] == pytest.approx(split[0], rel=1e-3)
    assert three[0] == pytest.approx(IOS[g]["vol"], rel=1e-3)
    _assert_bbox_mm(three[1], three[2], IOS[g])
    _assert_bbox_mm(split[1], split[2], IOS[g])


def test_flush_end_face_leaves_no_detached_sheet_233():
    # opening x-end == wall x-end: pre-fix an 8-triangle 0-volume piece
    # stretched the bbox 5.2 m.
    g = "1o1uaQnuP0xvnggRxTxvO0"
    r = _cut_wall("gh233_flush_end.ifc", g)
    assert r is not None
    assert r[0] == pytest.approx(IOS[g]["vol"], rel=1e-3)
    _assert_bbox_mm(r[1], r[2], IOS[g])
    assert r[3] == 12


def test_flush_top_three_item_opening_leaves_no_attached_sheet_233():
    # 3-item openings (#232) whose top == wall top and ends == wall ends:
    # pre-fix zmax +1.6 m from sheets attached to the kept sill block.
    g = "0$1AhHhaXB7uKyZ3CA1fKl"
    r = _cut_wall("gh233_flush_top_3item.ifc", g)
    assert r is not None
    assert r[0] == pytest.approx(IOS[g]["vol"], rel=1e-3)
    _assert_bbox_mm(r[1], r[2], IOS[g])


def test_fully_consumed_wall_is_not_emitted_233():
    # ifcopenshell returns 0 triangles and drops it; pre-fix ifcfast
    # emitted a ~4e-7 m³ "closed, reliable" residue with a 2.7 m bbox.
    g = "0hq9gTBgzDJ9yLll4OWB9V"
    assert _cut_wall("gh233_consumed.ifc", g) is None
    prod, _ = ifcfast.open(str(FIX / "gh233_consumed.ifc")).mesh_qto(cut_openings=True)
    assert g not in set(prod["guid"])
