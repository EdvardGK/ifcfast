"""Report JSON shape."""

from __future__ import annotations

import pytest

from tests.ids.conftest import SIMPLE_WALL_IDS


@pytest.mark.skipif(not SIMPLE_WALL_IDS.is_file(), reason="fixture IDS not on disk")
def test_report_to_dict_keys(example_ifc):
    from ifcfast.ids import validate

    report = validate(SIMPLE_WALL_IDS, example_ifc, engine="ifcfast", use_cache=False)
    d = report.to_dict()
    assert "ids_path" in d
    assert "ifc_path" in d
    assert "specifications" in d
    assert d["total_specifications"] == len(d["specifications"])
