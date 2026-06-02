"""Type-first extraction (``Model.by_type`` / ``type_summary`` /
``type_bank``) and the ``Model.diff()`` model-version comparison."""

from __future__ import annotations

import ifcfast


def test_by_type_returns_matching_products(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    walls = m.by_type("IfcWall")
    assert len(walls) == 1
    assert walls[0].entity == "IfcWall"
    # Unknown entity → empty list, never raises.
    assert m.by_type("IfcDoesNotExist") == []


def test_type_summary_shape(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    rows = m.type_summary()
    assert rows and isinstance(rows, list)
    for r in rows:
        for key in (
            "entity", "count", "storeys", "predefined_types",
            "object_types", "sample_guids",
        ):
            assert key in r, f"missing key: {key}"
        assert isinstance(r["count"], int)
        assert isinstance(r["sample_guids"], list)


def test_type_summary_sorted_by_count_desc(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    rows = m.type_summary()
    counts = [r["count"] for r in rows]
    assert counts == sorted(counts, reverse=True)


def test_diff_self_is_empty(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    d = m.diff(m)
    assert d["products"]["added_count"] == 0
    assert d["products"]["removed_count"] == 0
    assert d["products"]["changed_count"] == 0
    assert d["type_deltas"] == {}
    assert d["storey_deltas"] == []
    assert d["products"]["kept"] == len(m)


def test_diff_against_path_string(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    d = m.diff(str(ifcfast.example_path()))
    # Same file passed both times → no deltas.
    assert d["left"]["products"] == d["right"]["products"]
    assert d["products"]["changed_count"] == 0


def test_diff_shape_is_json_friendly(tmp_path, monkeypatch):
    import json

    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    d = m.diff(m)
    json.dumps(d, default=str)


def test_diff_nan_equal_nan(tmp_path, monkeypatch):
    """Self-diff must report zero changes even when watched fields are
    NaN on both sides (GH #40).

    Before the fix, `lv != rv` evaluated `nan != nan` → True, so every
    product with a missing `predefined_type` / `storey_name` /
    `storey_guid` (typical on IfcCurtainWall, openings, etc.) showed up
    as "changed" against itself.
    """
    from ifcfast.model import _values_equal

    nan = float("nan")
    assert _values_equal(nan, nan) is True
    assert _values_equal(None, None) is True
    assert _values_equal("Wall", "Wall") is True
    assert _values_equal(nan, "Wall") is False
    assert _values_equal("Wall", nan) is False
    assert _values_equal(None, "Wall") is False
    assert _values_equal("a", "b") is False
