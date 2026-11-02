"""GH #234 — ``IfcCompositeProfileDef`` swept solids mesh.

``gh234_composite_profile.ifc`` (hand-written IFC4, mm, depth 3 m):

* ``…D1a`` — triangle (0.5 m²) + rectangle placed/rotated by its own
  Position (0.4 m²), disjoint: 2.7 m³, reliable. Before the fix it was
  not meshed at all (no QTO row) while ``has_body`` was True — the
  HI90_ARK_MMI700 Archicad wall shape.
* ``…D1b`` — two overlapping squares: the mesh counts the 0.5 m² overlap
  twice (6.0 m³); flagged ``profile_invalid == "overlapping"``.
* ``…D1c`` — one member is an unsupported ``IfcCenterLineProfileDef``:
  no partial mesh, ``unhandled:IFCEXTRUDEDAREASOLID`` in the stats.
"""

from __future__ import annotations

from pathlib import Path

import pandas as pd

import ifcfast

FIX = Path(__file__).parent / "fixtures" / "gh234_composite_profile.ifc"
DISJOINT = "3Rk2cW9Nt1HfJ4sBpLgD1a"
OVERLAP = "3Rk2cW9Nt1HfJ4sBpLgD1b"
UNSUPPORTED = "3Rk2cW9Nt1HfJ4sBpLgD1c"


def _qto(cut_openings=False):
    m = ifcfast.open(FIX, use_cache=False, write_cache=False)
    df, _ = m.mesh_qto(cut_openings=cut_openings)
    return df


def test_disjoint_composite_meshes_with_analytic_volume():
    for cut in (False, True):
        df = _qto(cut).set_index("guid")
        row = df.loc[DISJOINT]
        assert abs(row.volume_m3 - 2.7) < 1e-4
        assert row.mesh_quality == "closed"
        assert row.volume_reliable
        assert row.volume_method == "mesh"
        assert pd.isna(row.profile_invalid)


def test_overlapping_members_flagged():
    df = _qto()
    row = df.set_index("guid").loc[OVERLAP]
    assert row.profile_invalid == "overlapping"
    assert not row.volume_reliable
    # The 6.0 m³ mesh exceeds the prism bound (bbox footprint 1.5 m² ×
    # 3 m), so the #60 tripwire routes it first; either way it is out of
    # trusted sums.
    assert row.volume_method in {"mesh_profile_invalid", "prism_fallback"}
    # Reveal-all: the double-counted mesh volume is still reported.
    assert abs(row.volume_mesh_m3 - 6.0) < 1e-4
    assert df.attrs["mesh_stats"]["products_profile_invalid"] == 1


def test_unsupported_member_fails_loudly():
    df = _qto()
    assert UNSUPPORTED not in set(df.guid)
    stats = df.attrs["mesh_stats"]
    assert stats["by_source"].get("unhandled:IFCEXTRUDEDAREASOLID") == 1
