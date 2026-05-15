"""Spatial-hierarchy and relationship-graph tests.

Uses the bundled fixture for the deterministic structural cases plus
``Sannergata_bygg_ARK_E.ifc`` for the realistic-shape assertions. Both
must produce identical results on cold parse vs cache hit.
"""

from __future__ import annotations

import os
import shutil
from pathlib import Path

import pytest

import ifcfast


FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"
SANNERGATA = Path(
    "/home/edkjo/workspace/inbox/ifc-workbench/data/samples/_big/"
    "Sannergata_bygg_ARK_E.ifc"
)


# ----------------------------------------------------------------------
# Helpers
# ----------------------------------------------------------------------


@pytest.fixture
def fresh_cache(tmp_path, monkeypatch):
    """Point the cache dir at a temp path so writes don't pollute ~."""
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    yield tmp_path / "cache"


@pytest.fixture
def minimal(fresh_cache):
    return ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)


@pytest.fixture
def sannergata(fresh_cache):
    if not SANNERGATA.exists():
        pytest.skip(f"missing fixture: {SANNERGATA}")
    return ifcfast.open(str(SANNERGATA), use_cache=False, write_cache=False)


# ----------------------------------------------------------------------
# Tests
# ----------------------------------------------------------------------


def test_relationship_dataframes_have_expected_columns(minimal):
    assert list(minimal.contained_in.columns) == ["product_guid", "storey_guid"]
    assert list(minimal.aggregates.columns) == [
        "child_guid", "parent_guid", "parent_kind",
    ]
    assert list(minimal.storey_building.columns) == ["storey_guid", "building_guid"]


def test_building_children_are_storeys(sannergata):
    """Every storey in the building shows up as a child of the building."""
    sb = sannergata.storey_building
    assert len(sb) > 0
    building_guid = sb.building_guid.iloc[0]
    expected_storeys = set(sb[sb.building_guid == building_guid].storey_guid.values)
    children = set(sannergata.children(building_guid))
    assert expected_storeys.issubset(children), (
        f"missing storeys: {expected_storeys - children}"
    )


def test_storey_products_cardinality_matches_contained_in(sannergata):
    """``products_in(storey)`` matches the contained_in row count for it."""
    by_storey = (
        sannergata.contained_in.groupby("storey_guid").size().to_dict()
    )
    for storey in sannergata.storeys:
        expected = by_storey.get(storey.guid, 0)
        actual = len(sannergata.products_in(storey.guid))
        assert actual == expected, (
            f"storey {storey.name}: expected {expected}, got {actual}"
        )


def test_ancestors_chain_reaches_project(sannergata):
    """A wall's ancestor chain ends at the project guid."""
    walls = [p for p in sannergata.products if p.entity == "IfcWallStandardCase"]
    assert walls, "no walls in fixture model"
    wall = walls[0]
    chain = sannergata.ancestors(wall.guid)
    # Storey, building, site, project (in some order; we just require the
    # chain ends at the project — i.e. the project guid is the last node).
    proj_row = sannergata.aggregates[
        sannergata.aggregates.parent_kind == "project"
    ]
    assert len(proj_row) >= 1
    project_guid = proj_row.parent_guid.iloc[0]
    assert chain, "expected non-empty ancestor chain for a wall"
    assert chain[-1] == project_guid, (
        f"chain ended at {chain[-1]!r}, expected project {project_guid!r}"
    )


def test_descendants_of_project_covers_contained_products(sannergata):
    """All products with spatial containment are reachable from the project."""
    proj = sannergata.aggregates[
        sannergata.aggregates.parent_kind == "project"
    ].parent_guid.iloc[0]
    reachable = set(sannergata.descendants(proj))
    contained = set(sannergata.contained_in.product_guid.values)
    missing = contained - reachable
    assert not missing, f"{len(missing)} contained products not in descendants"


def test_storey_of_matches_contained_in(sannergata):
    """``storey_of(p)`` returns the same guid the contained_in table records."""
    for product, storey in zip(
        sannergata.contained_in.product_guid.values[:50],
        sannergata.contained_in.storey_guid.values[:50],
    ):
        assert sannergata.storey_of(product) == storey


def test_building_of_for_product_walks_via_storey(sannergata):
    """``building_of(wall)`` returns the building hosting the wall's storey."""
    sb = dict(zip(
        sannergata.storey_building.storey_guid,
        sannergata.storey_building.building_guid,
    ))
    for product, storey in zip(
        sannergata.contained_in.product_guid.values[:30],
        sannergata.contained_in.storey_guid.values[:30],
    ):
        expected = sb.get(storey)
        if expected is None:
            continue  # storey without a registered building
        assert sannergata.building_of(product) == expected


def test_missing_guid_is_silent(minimal):
    assert minimal.parent("does-not-exist") is None
    assert minimal.children("does-not-exist") == []
    assert minimal.ancestors("does-not-exist") == []
    assert minimal.descendants("does-not-exist") == []
    assert minimal.storey_of("does-not-exist") is None
    assert minimal.building_of("does-not-exist") is None
    assert minimal.products_in("does-not-exist") == []


def test_cache_roundtrip_preserves_graph(tmp_path, monkeypatch):
    """Cold parse → cache write → re-open from cache → identical graph."""
    if not SANNERGATA.exists():
        pytest.skip(f"missing fixture: {SANNERGATA}")
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    cold = ifcfast.open(str(SANNERGATA))           # writes cache
    hot = ifcfast.open(str(SANNERGATA))            # reads cache

    # Each relationship table must round-trip byte-equal.
    import pandas as pd
    pd.testing.assert_frame_equal(
        cold.contained_in.reset_index(drop=True),
        hot.contained_in.reset_index(drop=True),
        check_dtype=False,
    )
    pd.testing.assert_frame_equal(
        cold.aggregates.reset_index(drop=True),
        hot.aggregates.reset_index(drop=True),
        check_dtype=False,
    )
    pd.testing.assert_frame_equal(
        cold.storey_building.reset_index(drop=True),
        hot.storey_building.reset_index(drop=True),
        check_dtype=False,
    )

    # And the traversal helpers agree on a real query.
    storey = cold.storeys[0]
    assert set(cold.products_in(storey.guid)) == set(hot.products_in(storey.guid))
    walls = [p.guid for p in cold.products if p.entity == "IfcWallStandardCase"][:5]
    for w in walls:
        assert cold.ancestors(w) == hot.ancestors(w)
