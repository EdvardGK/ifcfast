"""GH #202 — ``has_body`` / ``body_rep_type`` on the products table.

A model-control tool counts "every IfcProduct with a 3D body, excluding
IfcFeatureElementSubtraction". That is the denominator of every
per-element check, so it must be exact and must not need a geometry pass.
The flag is read from ``IfcProductDefinitionShape.Representations``
(``crates/core/src/body_rep.rs``), the same function the mesher uses to
pick the representation it tessellates.

``has_body_202.ifc`` (schema-valid IFC4) carries one product per case.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

import ifcfast
from ifcfast.classify import subtypes_of

FIXTURE = Path(__file__).parent / "fixtures" / "has_body_202.ifc"

EXPECTED = {
    # guid: (entity, has_body, body_rep_type)
    "2HB202Wall000000000000": ("IfcWall", True, "SweptSolid"),
    "2HB202Member0000000000": ("IfcMember", False, None),  # Axis/FootPrint/Box
    "2HB202Proxy00000000000": ("IfcBuildingElementProxy", False, None),  # no rep
    "2HB202Terminal00000000": ("IfcAirTerminal", True, "MappedRepresentation"),
    "2HB202CooledBeam000000": ("IfcCooledBeam", True, "MappedRepresentation"),
    "2HB202Opening000000000": ("IfcOpeningElement", True, "SweptSolid"),
    "2HB202Port000000000000": ("IfcDistributionPort", False, None),
    "2HB202Space00000000000": ("IfcSpace", True, "SweptSolid"),
}


@pytest.fixture(scope="module", params=["cold", "cache"])
def model(request, tmp_path_factory):
    if request.param == "cold":
        return ifcfast.open(FIXTURE, use_cache=False, write_cache=False)
    # Round-trip through the parquet cache: the columns must survive it.
    cache = tmp_path_factory.mktemp("cache")
    old = os.environ.get("IFCFAST_CACHE")
    os.environ["IFCFAST_CACHE"] = str(cache)
    try:
        ifcfast.open(FIXTURE, use_cache=False, write_cache=True)
        return ifcfast.open(FIXTURE, use_cache=True, write_cache=False)
    finally:
        if old is None:
            os.environ.pop("IFCFAST_CACHE", None)
        else:
            os.environ["IFCFAST_CACHE"] = old


def test_columns_present(model):
    df = model.products_df
    assert {"has_body", "body_rep_type"} <= set(df.columns)


@pytest.mark.parametrize("guid", sorted(EXPECTED))
def test_per_product_semantics(model, guid):
    entity, has_body, rep_type = EXPECTED[guid]
    row = model.products_df.set_index("guid").loc[guid]
    assert row.entity == entity
    assert bool(row.has_body) is has_body
    if rep_type is None:
        assert row.body_rep_type is None or row.body_rep_type != row.body_rep_type
    else:
        assert row.body_rep_type == rep_type
    p = model.product(guid)
    assert p.has_body is has_body
    assert p.body_rep_type == rep_type


def test_spaces_df_carries_the_flag(model):
    sp = model.spaces_df.set_index("guid").loc["2HB202Space00000000000"]
    assert bool(sp.has_body) is True
    assert sp.body_rep_type == "SweptSolid"


def test_element_denominator_expression(model):
    """The reporter's count, as the one expression AGENTS.md documents."""
    df = model.products_df
    subtraction = subtypes_of("IfcFeatureElementSubtraction", model.schema)
    n = int((df.has_body & ~df.entity.isin(subtraction)).sum())
    # wall, air terminal, cooled beam, space — not the opening, the port,
    # the axis-only member or the representation-less proxy.
    assert n == 4
    # len(m) is a ROW count of the products table, not an element count.
    assert len(model) == len(df) == 8
