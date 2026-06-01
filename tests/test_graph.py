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
SITE_ANNOTATION = Path(__file__).parent / "fixtures" / "site_annotation.ifc"
SANNERGATA = Path(
    "/home/edkjo/workspace/inbox/ifc-workbench/data/samples/_big/"
    "Sannergata_bygg_ARK_E.ifc"
)
DUPLEX = Path(__file__).parent.parent / ".local-samples" / "Duplex_A_20110907.ifc"


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
def site_annotation(fresh_cache):
    """Tiny IFC4 fixture with three containment kinds — storey (a wall),
    site (an IfcAnnotation), and building (an IfcDiscreteAccessory).
    Exercises the GH #32 fix where non-storey containment used to be
    silently dropped."""
    return ifcfast.open(str(SITE_ANNOTATION), use_cache=False, write_cache=False)


@pytest.fixture
def sannergata(fresh_cache):
    if not SANNERGATA.exists():
        pytest.skip(f"missing fixture: {SANNERGATA}")
    return ifcfast.open(str(SANNERGATA), use_cache=False, write_cache=False)


@pytest.fixture
def duplex(fresh_cache):
    if not DUPLEX.exists():
        pytest.skip(f"missing fixture: {DUPLEX}")
    return ifcfast.open(str(DUPLEX), use_cache=False, write_cache=False)


# ----------------------------------------------------------------------
# Tests
# ----------------------------------------------------------------------


def test_relationship_dataframes_have_expected_columns(minimal):
    assert list(minimal.contained_in.columns) == [
        "product_guid", "container_guid", "container_kind",
    ]
    assert list(minimal.aggregates.columns) == [
        "child_guid", "parent_guid", "parent_kind",
    ]
    assert list(minimal.storey_building.columns) == ["storey_guid", "building_guid"]


def test_contained_in_container_kinds_are_recognised(minimal):
    """Every container_kind we emit is one of the four documented values."""
    kinds = set(minimal.contained_in.container_kind.unique())
    assert kinds.issubset({"site", "building", "storey", "space"})


def test_non_storey_containment_surfaces_in_graph(site_annotation):
    """GH #32: site-, building-, and space-level containment edges must
    appear in `contained_in` and resolve via the spatial helpers — not
    silently disappear because the old indexer hard-filtered to storey
    structures only."""
    m = site_annotation
    ci = m.contained_in
    assert set(ci.container_kind.unique()) == {"site", "building", "storey"}

    ann = "SiteAnnotateGuidA0000000"      # IfcAnnotation in IfcSite
    acc = "BldgAccessoryGuidA000000"      # IfcDiscreteAccessory in IfcBuilding
    wall = "7XvctVUKr0kugbFTf53O9L"        # IfcWall in IfcBuildingStorey
    site = "1XvctVUKr0kugbFTf53O9L"
    building = "2XvctVUKr0kugbFTf53O9L"
    storey = "3XvctVUKr0kugbFTf53O9L"

    # contained_in records the right edge for each kind.
    rows = {row.product_guid: (row.container_guid, row.container_kind)
            for row in ci.itertuples(index=False)}
    assert rows[ann] == (site, "site")
    assert rows[acc] == (building, "building")
    assert rows[wall] == (storey, "storey")

    # parent() returns whatever container the element actually sits in.
    assert m.parent(ann) == site
    assert m.parent(acc) == building
    assert m.parent(wall) == storey

    # storey_of: only resolves for storey-or-below elements.
    assert m.storey_of(wall) == storey
    assert m.storey_of(acc) is None     # building has no storey above it
    assert m.storey_of(ann) is None     # site has no storey above it

    # building_of: walks via storey OR direct building containment.
    assert m.building_of(wall) == building
    assert m.building_of(acc) == building
    assert m.building_of(ann) is None   # site has no building above it


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
    ci = sannergata.contained_in
    storey_edges = ci[ci.container_kind == "storey"]
    by_storey = storey_edges.groupby("container_guid").size().to_dict()
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
    """``storey_of(p)`` returns the same guid the contained_in table records
    for products contained directly in a storey."""
    ci = sannergata.contained_in
    storey_rows = ci[ci.container_kind == "storey"]
    for product, storey in zip(
        storey_rows.product_guid.values[:50],
        storey_rows.container_guid.values[:50],
    ):
        assert sannergata.storey_of(product) == storey


def test_building_of_for_product_walks_via_storey(sannergata):
    """``building_of(wall)`` returns the building hosting the wall's storey."""
    sb = dict(zip(
        sannergata.storey_building.storey_guid,
        sannergata.storey_building.building_guid,
    ))
    ci = sannergata.contained_in
    storey_rows = ci[ci.container_kind == "storey"]
    for product, storey in zip(
        storey_rows.product_guid.values[:30],
        storey_rows.container_guid.values[:30],
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


# ----------------------------------------------------------------------
# Tier-1 spaces / voids / type linkage (added when the Rust core closed
# the IfcSpace / IfcRelVoidsElement / IfcRelDefinesByType gaps).
# ----------------------------------------------------------------------


def test_spaces_collection_present_on_empty_model(minimal):
    """``m.spaces`` is a list (possibly empty), not missing."""
    assert isinstance(minimal.spaces, list)
    df = minimal.spaces_df
    assert list(df.columns) == ["guid", "step_id"]


def test_voids_dataframe_present_on_empty_model(minimal):
    """``m.voids`` is an empty DataFrame with the documented columns even
    when no IfcRelVoidsElement is declared in the source IFC."""
    df = minimal.voids
    assert list(df.columns) == ["opening_guid", "host_guid"]
    assert len(df) == 0


def test_voids_resolves_on_real_model(sannergata):
    """Sannergata has 492 IfcRelVoidsElement relations. Each row should
    resolve both sides to product GUIDs the indexer also knows about."""
    voids = sannergata.voids
    assert len(voids) > 0
    product_guids = {p.guid for p in sannergata.products}
    assert set(voids["host_guid"].values).issubset(product_guids)
    assert set(voids["opening_guid"].values).issubset(product_guids)
    assert len(voids) == len(voids.drop_duplicates())


def test_reldefinesbytype_populates_product_type_fields(sannergata):
    """``IfcRelDefinesByType`` is the strongest signal — anything with a
    formal type link should land in ``type_source == 'ifctype'`` with a
    non-null ``type_guid`` and ``type_name``."""
    by_source: dict[str, int] = {"ifctype": 0, "objecttype": 0, "none": 0}
    for p in sannergata.products:
        by_source[p.type_source] = by_source.get(p.type_source, 0) + 1
        if p.type_source == "ifctype":
            assert p.type_guid is not None, f"ifctype product missing type_guid: {p.guid}"
            assert p.type_name, f"ifctype product missing type_name: {p.guid}"
        elif p.type_source == "objecttype":
            assert p.type_guid is None
            assert p.type_name == p.object_type
        else:  # "none"
            assert p.type_guid is None
            assert p.type_name is None
    # On a Revit-export-grade model the dominant bucket should be ifctype.
    assert by_source["ifctype"] > by_source["objecttype"]
    assert by_source["ifctype"] > by_source["none"]


def test_ifc2x3_doorstyle_windowstyle_are_classified_as_ifctype(duplex):
    """Regression for #18 — IFC2x3 IfcDoorStyle / IfcWindowStyle are
    IfcTypeProduct subtypes that don't carry the ``*Type`` suffix. The
    indexer must still treat them as the RelatingType of
    IfcRelDefinesByType, or 100% of door/window typing leaks silently on
    IFC2x3 files. Acceptance ties to the ifcopenshell walk on the
    bundled buildingSMART Duplex sample: 99/268 ifctype products."""
    df = duplex.products_df
    assert (df["type_source"] == "ifctype").sum() == 99, (
        "expected 99 ifctype products on Duplex_A_20110907.ifc — "
        "regression of #18 (IfcDoorStyle/IfcWindowStyle dropped by name suffix)"
    )
    doors = df[df["entity"] == "IfcDoor"]
    windows = df[df["entity"] == "IfcWindow"]
    assert len(doors) > 0 and (doors["type_source"] == "ifctype").all()
    assert len(windows) > 0 and (windows["type_source"] == "ifctype").all()

    catalogue = duplex.type_objects_df
    style_rows = catalogue[catalogue["entity"].isin(["IfcDoorStyle", "IfcWindowStyle"])]
    assert len(style_rows) > 0, "type_objects must surface IfcDoorStyle / IfcWindowStyle"
    door_type_guids = set(doors["type_guid"].dropna())
    window_type_guids = set(windows["type_guid"].dropna())
    assert door_type_guids.issubset(set(catalogue["guid"].values))
    assert window_type_guids.issubset(set(catalogue["guid"].values))


def test_type_objects_table_captures_ifctype_catalogue(sannergata):
    """Every ``type_guid`` referenced from a product should also appear
    in ``m.type_objects_df`` — the catalogue is round-trip consistent."""
    catalogue_guids = set(sannergata.type_objects_df["guid"].values)
    assert len(catalogue_guids) > 0
    product_type_guids = {
        p.type_guid for p in sannergata.products if p.type_source == "ifctype"
    }
    missing = product_type_guids - catalogue_guids
    assert not missing, f"products reference {len(missing)} unknown type guids"


def test_cache_roundtrip_preserves_new_tables(tmp_path, monkeypatch):
    """Spaces, voids, type_objects and per-product type_* survive a
    cache write/read cycle."""
    if not SANNERGATA.exists():
        pytest.skip(f"missing fixture: {SANNERGATA}")
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    cold = ifcfast.open(str(SANNERGATA))
    hot = ifcfast.open(str(SANNERGATA))

    assert len(cold.spaces) == len(hot.spaces)
    assert len(cold.type_objects) == len(hot.type_objects)

    import pandas as pd
    pd.testing.assert_frame_equal(
        cold.voids.reset_index(drop=True),
        hot.voids.reset_index(drop=True),
        check_dtype=False,
    )

    # Per-product type linkage survives the cache hit.
    cold_types = {
        p.guid: (p.type_guid, p.type_name, p.type_source)
        for p in cold.products if p.type_source == "ifctype"
    }
    df = hot.products_df  # cache hit: products[] is empty
    hot_typed = df[df["type_source"] == "ifctype"]
    assert len(hot_typed) == len(cold_types)
    for _, row in hot_typed.head(20).iterrows():
        assert cold_types[row["guid"]] == (
            row["type_guid"], row["type_name"], row["type_source"]
        )
