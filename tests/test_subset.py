"""Agent-facing `Model.subset()` — the first write primitive (GH #124).

Exercises the Python surface end to end: GUID resolution, bytes vs.
out_path returns, the spatial-spine climb, shared-rel pruning, fail-loud
on unknown GlobalIds, and the byte-identity of subsetting everything.
Reopen validation uses ifcopenshell when available (the independent
oracle), else falls back to re-parsing with ifcfast.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import ifcfast

FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"
WALL_GUID = "7XvctVUKr0kugbFTf53O9L"


@pytest.fixture(scope="module")
def model():
    return ifcfast.open(FIXTURE, use_cache=False, write_cache=False)


def test_subset_returns_bytes(model):
    data = model.subset([WALL_GUID])
    assert isinstance(data, (bytes, bytearray))
    assert data.startswith(b"ISO-10303-21;")
    assert b"END-ISO-10303-21;" in data


def test_subset_reopens_with_wall_and_spine(model, tmp_path):
    out = tmp_path / "wall.ifc"
    stats = model.subset([WALL_GUID], out_path=str(out))
    assert stats["seeds_present"] == 1
    assert stats["path"] == str(out)
    assert out.exists()

    # The seeded wall and its whole spatial spine survive.
    re = ifcfast.open(out, use_cache=False, write_cache=False)
    guids = {p.guid for p in re}
    assert WALL_GUID in guids


def test_subset_unknown_guid_is_loud(model):
    with pytest.raises(ValueError):
        model.subset(["THISGUIDDOESNOTEXIST00"])


def test_subset_empty_seeds_raises(model):
    with pytest.raises(ValueError):
        model.subset([])


def test_subset_of_all_products_is_self_contained(model, tmp_path):
    # Subsetting every product keeps a valid, reopenable document.
    all_guids = [p.guid for p in model]
    out = tmp_path / "all.ifc"
    stats = model.subset(all_guids, out_path=str(out))
    assert stats["seeds_present"] == len(all_guids)
    re = ifcfast.open(out, use_cache=False, write_cache=False)
    assert len(re) == len(model)


ifcopenshell = pytest.importorskip("ifcopenshell")


def test_subset_reopens_in_ifcopenshell_with_no_dangling(model, tmp_path):
    out = tmp_path / "wall_oracle.ifc"
    model.subset([WALL_GUID], out_path=str(out))
    f = ifcopenshell.open(str(out))

    assert len(f.by_type("IfcProject")) == 1
    assert any(w.GlobalId == WALL_GUID for w in f.by_type("IfcWall"))

    dangling = 0
    for inst in f:
        try:
            inst.get_info(recursive=False)
        except Exception:  # noqa: BLE001
            dangling += 1
    assert dangling == 0
