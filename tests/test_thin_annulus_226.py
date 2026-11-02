"""GH #226 — a thin annular ``IfcArbitraryProfileDefWithVoids`` (circular
outer + circular void) tessellates as a valid ring with the analytic area.

``gh226_thin_annulus.ifc`` carries HI90_RIE's recessed-socket ring item
verbatim (0.2 mm wall, r 20 / 19.8 mm, two ``IfcArcIndex`` per loop,
extruded 12 mm). Sampled loop-by-loop, the void's area-preserving interior
samples crossed the outer polygon and earcut filled the whole disc:
~5233 mm³ instead of 300 mm³ (+13 % on the real product), flagged
``profile_invalid="self_intersecting"`` since #220. The void is now sampled
at the outer loop's angles.
"""

from __future__ import annotations

import math
from pathlib import Path

import pandas as pd

import ifcfast

FIX = Path(__file__).parent / "fixtures" / "gh226_thin_annulus.ifc"
RING = "3Gh226aZ7Lr0ZeH1rAqKa3"
ANALYTIC_M3 = math.pi * (20.0**2 - 19.8**2) * 11.999999999999988 * 1e-9


def test_thin_ring_volume_is_analytic_and_trusted():
    for cut in (False, True):
        m = ifcfast.open(FIX, use_cache=False, write_cache=False)
        df, _ = m.mesh_qto(cut_openings=cut)
        row = df.set_index("guid").loc[RING]
        assert pd.isna(row.profile_invalid), row.profile_invalid
        assert row.mesh_quality == "closed"
        assert row.volume_method == "mesh"
        assert row.volume_reliable
        assert abs(row.volume_m3 / ANALYTIC_M3 - 1) < 1e-3, (row.volume_m3, ANALYTIC_M3)
