"""GH #180 — `StoreyRow.elevation` is file units, `elevation_m` is metres.

`IfcBuildingStorey.Elevation` is authored in the file's own length unit,
which is millimetres on most Revit / Archicad output. Every other
length-valued surface in the library is metres and says so with an `_m`
suffix. `elevation` was the one that silently wasn't, so a caller who
had internalised "ifcfast gives metres" got a 1000x error precisely
because they trusted the pattern.

`elevation` stays raw (round-trip); `elevation_m` is the comparable one.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import ifcfast
from ifcfast.model import StoreyRow


FIXTURES = Path(__file__).parent / "fixtures"
MM = FIXTURES / "storey_mm.ifc"          # LENGTHUNIT .MILLI. .METRE.
METRE = FIXTURES / "far_origin_box.ifc"  # LENGTHUNIT .METRE., storey at 120.
BROKEN = FIXTURES / "broken_conversion_unit.ifc"  # unit_scale is None

MM_PLAN_02 = "9XvctVUKr0kugbFTf53Omm"


@pytest.fixture
def mm_model():
    return ifcfast.open(MM, use_cache=False, write_cache=False)


def test_millimetre_storey_keeps_raw_and_adds_metres(mm_model):
    """The headline case: 3000 mm is 3.0 m, and both are readable."""
    assert mm_model.unit_scale == pytest.approx(0.001)
    assert mm_model.length_unit == "mm"

    plan02 = next(s for s in mm_model.storeys if s.guid == MM_PLAN_02)
    assert plan02.elevation == pytest.approx(3000.0)     # raw, file units
    assert plan02.elevation_m == pytest.approx(3.0)      # metres


def test_zero_elevation_is_zero_in_both(mm_model):
    """0 is the one value where the bug is invisible — pin it anyway so
    a None/0.0 mix-up in the conversion can't hide here."""
    plan01 = next(s for s in mm_model.storeys if s.guid != MM_PLAN_02)
    assert plan01.elevation == 0.0
    assert plan01.elevation_m == 0.0


def test_metre_model_the_two_fields_agree():
    m = ifcfast.open(METRE, use_cache=False, write_cache=False)
    assert m.unit_scale == pytest.approx(1.0)
    storey = m.storeys[0]
    assert storey.elevation == pytest.approx(120.0)
    assert storey.elevation_m == pytest.approx(120.0)
    assert storey.elevation_m == pytest.approx(storey.elevation)


def test_unresolvable_unit_gets_no_fabricated_metres():
    """A declared-but-unresolvable LENGTHUNIT leaves `unit_scale` None.
    Defaulting to metres there would be the same silent-wrong-number
    failure class the issue is about, so `elevation_m` stays None while
    the raw attribute survives."""
    m = ifcfast.open(BROKEN, use_cache=False, write_cache=False, strict=False)
    assert m.unit_scale is None
    storey = m.storeys[0]
    assert storey.elevation == 0.0
    assert storey.elevation_m is None


def test_elevation_m_is_on_the_declared_surfaces(mm_model):
    """`schemas` / `summary` / `preview` must agree with the dataclass —
    an agent reads the column list, not the source."""
    assert "elevation_m" in [
        f.name for f in StoreyRow.__dataclass_fields__.values()
    ]
    assert "elevation_m" in mm_model.schemas["storeys"]["columns"]
    assert "elevation_m" in mm_model.summary()["tables"]["storeys"]["columns"]

    rows = mm_model.preview("storeys", n=5)
    row = next(r for r in rows if r["guid"] == MM_PLAN_02)
    assert row["elevation"] == pytest.approx(3000.0)
    assert row["elevation_m"] == pytest.approx(3.0)


def test_cache_round_trip_preserves_elevation_m(tmp_path, monkeypatch):
    """The metres view survives the parquet index cache. (A pre-#180
    cache has no such column, which is what the CACHE_VERSION bump to 6
    invalidates.)"""
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "ifcfast-cache"))

    cold = ifcfast.open(MM)          # writes the cache
    hot = ifcfast.open(MM)           # reads it back
    cold_row = next(s for s in cold.storeys if s.guid == MM_PLAN_02)
    hot_row = next(s for s in hot.storeys if s.guid == MM_PLAN_02)

    assert hot_row.elevation == pytest.approx(cold_row.elevation) == 3000.0
    assert hot_row.elevation_m == pytest.approx(cold_row.elevation_m) == 3.0


def test_diff_storey_delta_carries_the_metres_view(tmp_path):
    """`diff()`'s storey deltas compare raw elevations, which can be two
    different units across a revision pair; the metres pair is the one a
    consumer can actually subtract."""
    right = tmp_path / "storey_mm_moved.ifc"
    text = MM.read_text(encoding="utf-8").replace(
        "'Plan 02',.ELEMENT.,3000.", "'Plan 02',.ELEMENT.,6000."
    )
    assert "6000." in text
    right.write_text(text, encoding="utf-8")

    left = ifcfast.open(MM, use_cache=False, write_cache=False)
    d = left.diff(
        ifcfast.open(right, use_cache=False, write_cache=False)
    )
    delta = next(
        x for x in d["storey_deltas"] if x["guid"] == MM_PLAN_02
    )
    assert delta["elevation"] == [3000.0, 6000.0]
    assert delta["elevation_m"] == pytest.approx([3.0, 6.0])


def test_diff_reports_a_unit_only_change(tmp_path):
    """The case the raw-only comparison misses entirely: a revision that
    re-authors the same file in metres. `elevation` is 3000. on both
    sides, so a `ls.elevation != rs.elevation` test sees nothing — while
    the storey actually moved from 3 m to 3000 m."""
    right = tmp_path / "storey_metre.ifc"
    text = MM.read_text(encoding="utf-8").replace(
        "IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.)",
        "IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.)",
    )
    assert ".MILLI." not in text
    right.write_text(text, encoding="utf-8")

    left = ifcfast.open(MM, use_cache=False, write_cache=False)
    d = left.diff(ifcfast.open(right, use_cache=False, write_cache=False))
    delta = next(x for x in d["storey_deltas"] if x["guid"] == MM_PLAN_02)
    assert delta["elevation"] == [3000.0, 3000.0]      # raw: unchanged
    assert delta["elevation_m"] == pytest.approx([3.0, 3000.0])
