"""IFC4X3 built-element classification (GH #82).

Two stacked causes had IFC4X3 built elements classify as ``mode='skip'``:

1. IFC4X3 renamed the bulk-element supertype ``IfcBuildingElement`` →
   ``IfcBuiltElement``; the inheritance walk only checked the IFC4 name.
2. Addendum / TC schema suffixes (``IFC4X3_ADD2``, ``IFC4_ADD2``,
   ``IFC4X3_TC1``) and ``"UNKNOWN"`` weren't resolved to a known schema
   key, killing the inheritance fallback for every non-hardcoded entity.

These assertions walk the static per-schema supertype map shipped with
the wheel — no runtime ``ifcopenshell`` dependency, no live model.
"""

from __future__ import annotations

import pytest

from ifcfast.classify import (
    ElementMode,
    _resolve_schema,
    classify_by_name,
)


# --- Cause 1: IfcBuiltElement rename --------------------------------------

@pytest.mark.parametrize(
    "entity",
    ["IfcBuiltElement", "IfcKerb", "IfcPavement", "IfcCourse"],
)
def test_ifc4x3_built_elements_measure(entity):
    """IFC4X3-only built elements chain through IfcBuiltElement and must
    classify as MEASURE, not SKIP."""
    assert classify_by_name(entity, "IFC4X3") is ElementMode.MEASURE


def test_ifc4x3_wall_still_measure():
    """Hardcoded MEASURE entity unaffected under IFC4X3."""
    assert classify_by_name("IfcWall", "IFC4X3") is ElementMode.MEASURE


# --- Cause 2: schema-suffix / UNKNOWN resolution --------------------------

@pytest.mark.parametrize("schema", ["IFC4X3_ADD2", "IFC4X3_TC1", "ifc4x3_add2"])
def test_suffixed_ifc4x3_resolves(schema):
    """Addendum / TC headers must resolve to IFC4X3 so the inheritance
    fallback stays alive for IFC4X3-only built elements."""
    assert _resolve_schema(schema) == "IFC4X3"
    assert classify_by_name("IfcKerb", schema) is ElementMode.MEASURE


def test_suffix_prefix_is_most_specific():
    """Longest-prefix match: IFC4X3_ADD2 -> IFC4X3, IFC4_ADD2 -> IFC4."""
    assert _resolve_schema("IFC4X3_ADD2") == "IFC4X3"
    assert _resolve_schema("IFC4_ADD2") == "IFC4"


@pytest.mark.parametrize("schema", ["UNKNOWN", "unknown", ""])
def test_unknown_schema_is_unset(schema):
    """'UNKNOWN' / empty resolve to None (unset) rather than a real schema."""
    assert _resolve_schema(schema) is None


# --- Regression: IFC4 / IFC2X3 unchanged ----------------------------------

@pytest.mark.parametrize(
    "entity,expected",
    [
        ("IfcWall", ElementMode.MEASURE),
        ("IfcWallElementedCase", ElementMode.MEASURE),  # inheritance path
        ("IfcSlab", ElementMode.MEASURE),
        ("IfcDoor", ElementMode.COUNT),
        ("IfcWindow", ElementMode.COUNT),
        ("IfcValve", ElementMode.COUNT),
        ("IfcPipeSegment", ElementMode.LINEAR),
        ("IfcSpace", ElementMode.SKIP),
        ("IfcOpeningElement", ElementMode.SKIP),
    ],
)
def test_ifc4_classification_unchanged(entity, expected):
    assert classify_by_name(entity, "IFC4") is expected


@pytest.mark.parametrize(
    "entity,expected",
    [
        ("IfcWall", ElementMode.MEASURE),
        ("IfcDoor", ElementMode.COUNT),
        ("IfcSpace", ElementMode.SKIP),
    ],
)
def test_ifc2x3_classification_unchanged(entity, expected):
    assert classify_by_name(entity, "IFC2X3") is expected


def test_ifc4_buildingelement_still_routes_measure():
    """The IFC4 IfcBuildingElement supertype must still drive MEASURE
    (the rename fix is additive, not a replacement)."""
    assert _resolve_schema("IFC4") == "IFC4"
    # IfcBuildingElement is the IFC4 supertype name itself.
    assert classify_by_name("IfcBuildingElement", "IFC4") is ElementMode.MEASURE
