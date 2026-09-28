"""GH #192 slice 3: `m.nests` / `m.groups` / `m.fills` edge tables and the
IDS PartOf facet, end to end through the wheel.

Fixture: `crates/core/tests/fixtures/ids/partof_relations.ifc` (the same file
the Rust `ids_eval.rs` PartOf test and its IfcTester cross-check use).
"""
from pathlib import Path

import pytest

import ifcfast

FIXTURE = (
    Path(__file__).resolve().parent.parent
    / "crates/core/tests/fixtures/ids/partof_relations.ifc"
)

NESTS_COLS = ["parent_guid", "child_guid", "position", "parent_step_id", "child_step_id"]
GROUPS_COLS = ["group_guid", "group_entity", "member_guid", "group_step_id", "member_step_id"]
FILLS_COLS = ["opening_guid", "element_guid"]


def g(n: int) -> str:
    return f"{n:022d}"


def _check_tables(m):
    assert list(m.nests.columns) == NESTS_COLS
    assert list(m.groups.columns) == GROUPS_COLS
    assert list(m.fills.columns) == FILLS_COLS
    assert m.nests.values.tolist() == [[g(8), g(9), 0, 8, 9]]
    assert m.groups.values.tolist() == [
        [g(10), "IfcDistributionSystem", g(6), 10, 6],
        [g(10), "IfcDistributionSystem", g(9), 10, 9],
        [g(12), "IfcZone", g(11), 12, 11],  # IfcRelAssignsToGroupByFactor
    ]
    assert m.fills.values.tolist() == [[g(5), g(6)]]
    # fills joins voids on the opening: door -> opening -> wall
    j = m.fills.merge(m.voids, on="opening_guid")
    assert j[["element_guid", "host_guid"]].values.tolist() == [[g(6), g(4)]]


def test_edge_tables_cold_parse():
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    _check_tables(m)
    for name in ("nests", "groups", "fills"):
        assert m.summary()["tables"][name]["rows"] == len(getattr(m, name))
        assert name in m.schemas
        assert m.preview(name, 1)


def test_edge_tables_survive_the_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path))
    m1 = ifcfast.open(str(FIXTURE))
    _check_tables(m1)
    m2 = ifcfast.open(str(FIXTURE))
    assert m2._products_df is not None, "second open must be a cache hit"
    _check_tables(m2)
    assert m2.nests["position"].dtype.kind == "i"
    assert m2.groups["member_step_id"].dtype.kind == "i"


def test_cache_missing_edge_table_is_a_miss(tmp_path, monkeypatch):
    from ifcfast import cache

    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path))
    m1 = ifcfast.open(str(FIXTURE))
    d = cache.cache_dir_for(m1.header)
    (d / cache.NESTS_FILE).unlink()
    assert cache.read_index(m1.header) is None


PART_OF_IDS = """<?xml version="1.0" encoding="utf-8"?>
<ids xmlns="http://standards.buildingsmart.org/IDS" xmlns:xs="http://www.w3.org/2001/XMLSchema">
<info><title>T</title></info><specifications>
<specification name="door in a wall" ifcVersion="IFC4"><applicability minOccurs="1" maxOccurs="unbounded"><entity><name><simpleValue>IFCDOOR</simpleValue></name></entity></applicability>
<requirements><partOf relation="IFCRELVOIDSELEMENT IFCRELFILLSELEMENT"><entity><name><simpleValue>IFCWALL</simpleValue></name></entity></partOf></requirements></specification>
<specification name="beam in a building" ifcVersion="IFC4"><applicability minOccurs="1" maxOccurs="unbounded"><entity><name><simpleValue>IFCBEAM</simpleValue></name></entity></applicability>
<requirements><partOf relation="IFCRELAGGREGATES"><entity><name><simpleValue>IFCBUILDING</simpleValue></name></entity></partOf></requirements></specification>
</specifications></ids>"""


def test_validate_ids_part_of():
    rep = ifcfast.validate_ids(PART_OF_IDS, str(FIXTURE))
    assert rep.specs["status"].tolist() == ["pass", "fail"]
    assert rep.specs["unsupported_feature"].isna().all()
    f = rep.failures
    assert f["facet_type"].tolist() == ["part_of"]
    assert f["reason_code"].tolist() == ["PARTOF_ENTITY_MISMATCH"]
    assert f["actual"].tolist() == ["['IFCELEMENTASSEMBLY']"]
    assert f["expected"].tolist() == [
        "An element must have an IFCRELAGGREGATES relationship with an IFCBUILDING"
    ]
