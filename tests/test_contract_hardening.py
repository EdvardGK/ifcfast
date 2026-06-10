"""Regression tests for the agent-first contract-hardening batch.

Covers GH #68 (diff null-representation false changes), GH #70
(truncated-file refusal), GH #71 (loud failures on typo'd table /
entity / mode + namespace hygiene), GH #78 (products_in aggregate
completeness), GH #79 (storey_of without an IfcBuilding), GH #83 (MCP
staleness) and GH #84 (diff(Path), CLI content errors).
"""

from __future__ import annotations

import math
import os
import shutil
from pathlib import Path

import pytest

import ifcfast
from ifcfast.model import _is_missing, _values_equal

FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"


@pytest.fixture
def fresh_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    yield tmp_path / "cache"


# ----------------------------------------------------------------------
# Spatial fixture builders (GH #78 / #79): a curtain wall contained in
# the storey, with an IfcPlate part attached via IfcRelAggregates.
# ----------------------------------------------------------------------

_SPATIAL_COMMON = """\
#1=IFCPROJECT('0PROJgUIDgUIDgUIDgUID0',$,'P',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1SITEgUIDgUIDgUIDgUID0',$,'Site',$,$,#15,$,$,.ELEMENT.,$,$,0.,$,$);
#12=IFCBUILDINGSTOREY('3STORgUIDgUIDgUIDgUID0',$,'S1',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#30=IFCCURTAINWALL('7CWALLUIDgUIDgUIDgUID0',$,'CW-001',$,$,#15,$,'t1');
#31=IFCPLATE('7PLATEUIDgUIDgUIDgUID0',$,'Plate-001',$,$,#15,$,'t2');
#33=IFCRELCONTAINEDINSPATIALSTRUCTURE('8CONT1UIDgUIDgUIDgUID0',$,$,$,(#30),#12);
#34=IFCRELAGGREGATES('9AGGPLUIDgUIDgUIDgUID0',$,$,$,#30,(#31));
"""

_WITH_BUILDING = _SPATIAL_COMMON + """\
#11=IFCBUILDING('2BLDGgUIDgUIDgUIDgUID0',$,'B',$,$,#15,$,$,.ELEMENT.,$,$,$);
#20=IFCRELAGGREGATES('4AGG1gUIDgUIDgUIDgUID0',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5AGG2gUIDgUIDgUIDgUID0',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6AGG3gUIDgUIDgUIDgUID0',$,$,$,#11,(#12));
"""

# Storey aggregated directly under the SITE — legal IFC, common in
# infra / landscape exports. No IfcBuilding anywhere.
_NO_BUILDING = _SPATIAL_COMMON + """\
#20=IFCRELAGGREGATES('4AGG1gUIDgUIDgUIDgUID0',$,$,$,#1,(#10));
#22=IFCRELAGGREGATES('6AGG3gUIDgUIDgUIDgUID0',$,$,$,#10,(#12));
"""

STOREY = "3STORgUIDgUIDgUIDgUID0"
CWALL = "7CWALLUIDgUIDgUIDgUID0"
PLATE = "7PLATEUIDgUIDgUIDgUID0"
BLDG = "2BLDGgUIDgUIDgUIDgUID0"


def _write_ifc(path: Path, data_lines: str) -> Path:
    path.write_text(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('x'),'2;1');\n"
        "FILE_NAME('t.ifc','2026-06-10T00:00:00',('t'),('t'),'t','t','');\n"
        "FILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n"
        + data_lines
        + "ENDSEC;\nEND-ISO-10303-21;\n"
    )
    return path


# ----------------------------------------------------------------------
# GH #68 — diff must treat None (cold parse) and NaN (cache hit) as the
# same missing value.
# ----------------------------------------------------------------------


def test_values_equal_mixed_null_representations():
    nan = float("nan")
    assert _values_equal(nan, None)
    assert _values_equal(None, nan)
    assert _values_equal(nan, nan)
    assert _values_equal(None, None)
    assert not _values_equal(None, "x")
    assert not _values_equal(nan, "x")
    assert _values_equal("a", "a")
    assert not _values_equal("a", "b")


def test_is_missing():
    assert _is_missing(None)
    assert _is_missing(float("nan"))
    assert not _is_missing(0.0)
    assert not _is_missing("")
    assert not _is_missing("nan")


def test_diff_cold_vs_cached_reports_zero_changes(tmp_path, monkeypatch):
    """A byte-identical copy must diff clean regardless of which side
    came from the parquet cache and which from a cold parse (GH #68)."""
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    copy = tmp_path / "copy.ifc"
    shutil.copyfile(FIXTURE, copy)

    cold = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    ifcfast.open(str(copy))  # build the copy's cache
    cached = ifcfast.open(str(copy))  # cache hit -> DataFrame-backed

    d = cold.diff(cached)
    assert d["products"]["changed_count"] == 0, d["products"]["changed"]
    assert d["products"]["added_count"] == 0
    assert d["products"]["removed_count"] == 0


def test_diff_accepts_pathlike(fresh_cache):
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    d = m.diff(FIXTURE)  # pathlib.Path, not str (GH #84)
    assert d["products"]["changed_count"] == 0


# ----------------------------------------------------------------------
# GH #79 / #78 — spatial graph completeness.
# ----------------------------------------------------------------------


def test_storey_of_aggregate_part_without_building(tmp_path, fresh_cache):
    """Plate -> curtain wall (aggregates) -> storey (containment) must
    resolve even when the storey hangs directly off the site (GH #79)."""
    p = _write_ifc(tmp_path / "no_building.ifc", _NO_BUILDING)
    m = ifcfast.open(str(p), use_cache=False, write_cache=False)
    assert m.storey_of(CWALL) == STOREY  # direct containment (control)
    assert m.storey_of(PLATE) == STOREY  # via aggregate hop


def test_storey_of_aggregate_part_with_building(tmp_path, fresh_cache):
    p = _write_ifc(tmp_path / "with_building.ifc", _WITH_BUILDING)
    m = ifcfast.open(str(p), use_cache=False, write_cache=False)
    assert m.storey_of(PLATE) == STOREY


def test_products_in_storey_includes_aggregate_parts(tmp_path, fresh_cache):
    """products_in(storey) must include sub-products reached through
    IfcRelAggregates, and agree with products_in(building) (GH #78)."""
    p = _write_ifc(tmp_path / "with_building.ifc", _WITH_BUILDING)
    m = ifcfast.open(str(p), use_cache=False, write_cache=False)
    in_storey = set(m.products_in(STOREY))
    in_building = set(m.products_in(BLDG))
    assert PLATE in in_storey
    assert in_storey == {CWALL, PLATE}
    assert in_storey == in_building


# ----------------------------------------------------------------------
# GH #70 — truncated / unterminated files must fail loudly.
# ----------------------------------------------------------------------


def test_truncated_file_raises(tmp_path, fresh_cache):
    data = FIXTURE.read_bytes()
    trunc = tmp_path / "trunc.ifc"
    trunc.write_bytes(data[: len(data) // 2])
    with pytest.raises(ValueError, match="truncated|unterminated"):
        ifcfast.open(str(trunc), use_cache=False, write_cache=False)


def test_missing_trailer_raises(tmp_path, fresh_cache):
    text = FIXTURE.read_text()
    assert "END-ISO-10303-21" in text
    bad = tmp_path / "no_trailer.ifc"
    bad.write_text(text.replace("END-ISO-10303-21;", ""))
    with pytest.raises(ValueError, match="truncated|unterminated"):
        ifcfast.header(str(bad))


def test_intact_file_still_opens(fresh_cache):
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    assert len(m.products) >= 1


# ----------------------------------------------------------------------
# GH #71 — loud failures on typos; namespace hygiene.
# ----------------------------------------------------------------------


def test_preview_unknown_table_raises(fresh_cache):
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    with pytest.raises(ValueError, match="Unknown table"):
        m.preview("nope")
    # the error must teach the valid vocabulary
    with pytest.raises(ValueError, match="psets"):
        m.preview("types")  # plausible agent typo for "type_objects"


def test_preview_known_tables_do_not_raise(fresh_cache):
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    for table in (
        "products", "storeys", "spaces", "type_objects", "contained_in",
        "aggregates", "storey_building", "voids", "psets", "quantities",
        "materials", "classifications",
    ):
        assert isinstance(m.preview(table, 2), list)


def test_filter_unknown_entity_raises_at_call_time(fresh_cache):
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    with pytest.raises(ValueError, match="Unknown IFC entity"):
        m.filter(entity="IfcWal")  # not iterated — must raise eagerly
    with pytest.raises(ValueError, match="Unknown mode"):
        m.filter(mode="bogus")
    with pytest.raises(ValueError, match="Unknown IFC entity"):
        m.by_type("IfcNotAClass")


def test_filter_valid_but_absent_entity_is_empty_not_error(fresh_cache):
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    assert list(m.filter(entity="IfcPile")) == []
    # supertype-only names (values, not keys, of the schema maps) pass
    assert list(m.filter(entity="IfcElement")) == []


def test_namespace_hygiene():
    assert not hasattr(ifcfast, "Path")
    assert not hasattr(ifcfast, "annotations")


# ----------------------------------------------------------------------
# GH #83 / MCP data tools — direct function-level tests (the stdio
# round-trip lives in test_mcp_server.py). Skipped without `mcp`.
# ----------------------------------------------------------------------


def test_mcp_resolve_detects_file_change(tmp_path, monkeypatch):
    pytest.importorskip("mcp.server.fastmcp")
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    from ifcfast import mcp_server

    p = _write_ifc(tmp_path / "model.ifc", _WITH_BUILDING)
    m1 = mcp_server._resolve(str(p))
    assert mcp_server._resolve(str(p)) is m1  # unchanged -> cached

    # re-export: contents (and size) change
    _write_ifc(
        tmp_path / "model.ifc",
        _WITH_BUILDING.replace("'CW-001'", "'CW-001-RENAMED'"),
    )
    m2 = mcp_server._resolve(str(p))
    assert m2 is not m1
    assert any(r.name == "CW-001-RENAMED" for r in m2.products)


def test_mcp_psets_and_product_card(tmp_path, monkeypatch):
    pytest.importorskip("mcp.server.fastmcp")
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    from ifcfast import mcp_server

    qfix = Path(__file__).parent / "fixtures" / "quantities.ifc"
    wall = "7WALL1UIDgUIDgUIDgUID0"

    rows = mcp_server.quantities(str(qfix), guid=wall)
    assert rows and all(r["guid"] == wall for r in rows)
    names = {r["quantity_name"] for r in rows}
    assert "Length" in names

    card = mcp_server.product_card(str(qfix), wall)
    assert card is not None
    assert card["product"]["guid"] == wall
    assert {r["quantity_name"] for r in card["quantities"]} >= {"Length"}
    assert card["storey_guid"] == STOREY
    assert mcp_server.product_card(str(qfix), "NOTAREALGUID0000000000") is None
