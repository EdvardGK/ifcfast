"""``Model.by_type`` subtype expansion (GH #81).

``by_type`` mirrors ``ifcopenshell.file.by_type(type, include_subtypes=True)``:
it expands subtypes by default, matches case-insensitively, and exposes an
``include_subtypes=False`` escape hatch for an exact single-entity match.

Expansion is resolved against the static per-schema supertype map shipped
with the wheel — no runtime ``ifcopenshell`` dependency. The static-map
unit tests run everywhere; the parity assertions use the bundled
buildingSMART Duplex sample and skip when it's absent.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import ifcfast
from ifcfast.classify import canonical_entity_any_schema, subtypes_of

FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"
DUPLEX = Path(__file__).parent.parent / ".local-samples" / "Duplex_A_20110907.ifc"


@pytest.fixture
def fresh_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    yield tmp_path / "cache"


# ----------------------------------------------------------------------
# Static map — no model needed.
# ----------------------------------------------------------------------


def test_subtypes_of_expands_wall_family():
    for schema in ("IFC2X3", "IFC4"):
        s = subtypes_of("IfcWall", schema)
        assert "IfcWall" in s
        assert "IfcWallStandardCase" in s, schema


def test_subtypes_of_is_case_insensitive():
    assert subtypes_of("ifcwall", "IFC4") == subtypes_of("IfcWall", "IFC4")


def test_subtypes_of_element_is_broad():
    # IfcElement is an abstract supertype of the entire physical product
    # tree — expansion must reach concrete leaves like IfcWall.
    s = subtypes_of("IfcElement", "IFC4")
    assert len(s) > 30
    assert {"IfcWall", "IfcDoor", "IfcWindow", "IfcSlab"} <= s


def test_subtypes_of_unknown_entity_is_self_only():
    assert subtypes_of("IfcNotAClass", "IFC4") == frozenset({"IfcNotAClass"})


def test_subtypes_of_unknown_schema_degrades_to_exact():
    assert subtypes_of("IfcWall", "IFC9X9") == frozenset({"IfcWall"})


def test_canonical_entity_any_schema_normalises_case():
    assert canonical_entity_any_schema("ifcwall") == "IfcWall"
    assert canonical_entity_any_schema("IFCWALL") == "IfcWall"
    assert canonical_entity_any_schema("IfcWall") == "IfcWall"
    # Unknown names pass through untouched (caller validates separately).
    assert canonical_entity_any_schema("IfcWal") == "IfcWal"


# ----------------------------------------------------------------------
# Model.by_type — minimal fixture (IFC4, one IfcWall).
# ----------------------------------------------------------------------


def test_by_type_supertype_returns_subtype_instances(fresh_cache):
    """The core GH #81 promise on a tiny fixture: an abstract supertype
    query must reach the concrete product."""
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    assert len(m.by_type("IfcWall")) == 1
    # Every supertype of IfcWall must now return the wall too.
    for sup in ("IfcElement", "IfcProduct", "IfcRoot"):
        assert len(m.by_type(sup)) == 1, sup


def test_by_type_is_case_insensitive(fresh_cache):
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    assert len(m.by_type("ifcwall")) == len(m.by_type("IfcWall")) == 1


def test_by_type_exact_match_opt_out(fresh_cache):
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    # Exact match on a supertype the fixture has no direct instance of.
    assert m.by_type("IfcElement", include_subtypes=False) == []
    # Exact match still finds the concrete entity, case-insensitively.
    assert len(m.by_type("ifcwall", include_subtypes=False)) == 1


def test_by_type_unknown_entity_still_raises(fresh_cache):
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    with pytest.raises(ValueError, match="Unknown IFC entity"):
        m.by_type("IfcWal")


# ----------------------------------------------------------------------
# Parity — buildingSMART Duplex (IFC2x3, Revit export).
# ----------------------------------------------------------------------


@pytest.fixture
def duplex(fresh_cache):
    if not DUPLEX.exists():
        pytest.skip(f"missing fixture: {DUPLEX}")
    return ifcfast.open(str(DUPLEX), use_cache=False, write_cache=False)


def test_duplex_wall_query_includes_standard_case(duplex):
    """On a typical Revit IFC2x3 export, walls are authored as
    IfcWallStandardCase. The bare ``by_type("IfcWall")`` query must
    include them (the whole point of the ifcopenshell contract)."""
    walls = duplex.by_type("IfcWall")
    standard = duplex.by_type("IfcWallStandardCase")
    assert len(standard) > 0, "fixture should have IfcWallStandardCase walls"
    assert len(walls) > len(standard), (
        "by_type('IfcWall') must expand to include IfcWallStandardCase"
    )
    entities = {p.entity for p in walls}
    assert "IfcWallStandardCase" in entities
    # Exact opt-out returns only the bare IfcWall instances.
    exact = duplex.by_type("IfcWall", include_subtypes=False)
    assert len(exact) < len(walls)
    assert all(p.entity == "IfcWall" for p in exact)


def test_duplex_element_query_is_non_empty(duplex):
    """The extremely common ``by_type("IfcElement")`` idiom returned []
    before GH #81; it must now return every element subtype present."""
    elements = duplex.by_type("IfcElement")
    assert len(elements) > 0
    # Walls are IfcElement subtypes — they must be in the result.
    wall_guids = {p.guid for p in duplex.by_type("IfcWall")}
    element_guids = {p.guid for p in elements}
    assert wall_guids <= element_guids
