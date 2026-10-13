"""GH #220 — an invalid swept profile polygon routes the QTO row out of
trusted volume sums.

``gh220_crossing_sliver.ifc`` (hand-written IFC4, mm):

* wall ``…C9d`` — an ``IfcIndexedPolyCurve`` profile: an authored arc
  (0,0)→(3000,300)→(6000,0) closed by a polyline whose middle edge
  crosses the arc. Self-intersecting as authored. Before GH #220 it came
  back ``mesh_quality="open_shell"`` / ``volume_method="mesh_open"`` /
  ``volume_reliable=True`` — the over-covering earcut mesh fits inside its
  own prism and AABB bounds, the shape class of the KNM_ARK slivers.
* wall ``…C9e`` — the same arc closed by its chord: a valid segment.
"""

from __future__ import annotations

from pathlib import Path

import pandas as pd

import ifcfast

FIX = Path(__file__).parent / "fixtures"
SLIVER = FIX / "gh220_crossing_sliver.ifc"
CROSSING = "2Qm8aZ7Lr0ZeH1rAqKfC9d"
VALID = "2Qm8aZ7Lr0ZeH1rAqKfC9e"
REASONS = {"self_intersecting", "degenerate", "hole_outside"}


def _qto(cut_openings=False):
    m = ifcfast.open(SLIVER, use_cache=False, write_cache=False)
    df, _ = m.mesh_qto(cut_openings=cut_openings)
    return df.set_index("guid")


def test_crossing_profile_is_unreliable_with_typed_reason():
    for cut in (False, True):
        df = _qto(cut)
        row = df.loc[CROSSING]
        assert row.profile_invalid == "self_intersecting"
        assert not row.volume_reliable
        assert row.volume_method == "mesh_profile_invalid"
        # Reveal-all: the mesh value is still reported, not dropped.
        assert row.volume_mesh_m3 > 0
        assert row.volume_m3 == row.volume_mesh_m3
        assert df.attrs["mesh_stats"]["products_profile_invalid"] == 1


def test_valid_profile_untouched():
    row = _qto().loc[VALID]
    assert pd.isna(row.profile_invalid)
    assert row.volume_reliable
    assert row.volume_method == "mesh"
    # Circular segment, chord 6 m, sagitta 0.3 m, 3 m high ≈ 3.607 m³.
    assert abs(row.volume_m3 - 3.607) < 0.01


def test_reason_vocabulary_and_reliability_invariant():
    df = _qto()
    flagged = df[df.profile_invalid.notna()]
    assert set(flagged.profile_invalid) <= REASONS
    assert not flagged.volume_reliable.any()


def test_substrate_carries_profile_invalid(tmp_path):
    import pyarrow.parquet as pq

    out = ifcfast.bundle(SLIVER, tmp_path / "b")
    inst = pq.read_table(out["instances_parquet"]).to_pandas().set_index("guid")
    assert inst.loc[CROSSING, "profile_invalid"] == "self_intersecting"
    assert not inst.loc[CROSSING, "volume_reliable"]
    assert inst.loc[CROSSING, "volume_method"] == "mesh_profile_invalid"
    assert pd.isna(inst.loc[VALID, "profile_invalid"])
    assert inst.loc[VALID, "volume_reliable"]
