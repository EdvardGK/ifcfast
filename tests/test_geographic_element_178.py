"""GH #178 — IfcGeographicElement parses, and an unknown class is loud.

Two halves, and the second matters more than the first:

1. ``geographic_element.ifc`` is ``geom_box.ifc`` with the product entity
   swapped for ``IFCGEOGRAPHICELEMENT(.TERRAIN.)``. Before the fix that
   file indexed to zero products, zero types and zero containment edges —
   no error, no warning. The type object was still listed, so the model
   looked half-parsed rather than unsupported.

2. ``unlisted_product.ifc`` is the same file with ``IFCTUBEBUNDLE``, a
   real IFC4 product that is deliberately NOT whitelisted. It stands in
   for every class ifcfast does not yet index: the model still comes back
   empty, but it now SAYS SO — a warning at open and a
   ``skipped_product_types`` entry in ``summary()``.
"""

from __future__ import annotations

import warnings
from pathlib import Path

import pytest

import ifcfast


FIXTURES = Path(__file__).parent / "fixtures"
GEO = FIXTURES / "geographic_element.ifc"
UNLISTED = FIXTURES / "unlisted_product.ifc"

GEO_GUID = "2Geo178Elem00000000000"
STOREY_GUID = "2Geo178Storey000000000"


@pytest.fixture(scope="module")
def geo_model():
    return ifcfast.open(GEO, use_cache=False, write_cache=False)


# ---------------------------------------------------------------- 1. parses


def test_geographic_element_is_indexed(geo_model):
    assert len(geo_model) == 1
    assert geo_model.types() == {"IfcGeographicElement": 1}

    row = geo_model.products[0]
    assert row.guid == GEO_GUID
    assert row.entity == "IfcGeographicElement"
    assert row.predefined_type == "TERRAIN"
    assert row.name == "Terrain mound"


def test_containment_survives(geo_model):
    row = geo_model.products[0]
    assert row.storey_guid == STOREY_GUID
    df = geo_model.contained_in
    assert len(df) == 1
    assert df.iloc[0]["container_kind"] == "storey"


def test_mesh_qto_has_one_row_with_volume(geo_model):
    products_df, _surfaces_df = geo_model.mesh_qto()
    assert len(products_df) == 1
    row = products_df.iloc[0]
    assert row["entity"] == "IfcGeographicElement"
    # 2 x 3 m profile extruded 4 m.
    assert row["volume_m3"] == pytest.approx(24.0, rel=1e-6)
    assert row["volume_m3"] > 0


def test_no_silent_zero_warning_on_a_covered_file():
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        m = ifcfast.open(GEO, use_cache=False, write_cache=False)
    assert m.skipped_product_types == {}
    assert not [w for w in caught if "product whitelist" in str(w.message)]


# ------------------------------------------------------- 2. loud on unknown


def test_unlisted_product_class_warns_instead_of_returning_a_silent_zero():
    with pytest.warns(UserWarning, match="IfcTubeBundle"):
        m = ifcfast.open(UNLISTED, use_cache=False, write_cache=False)
    assert len(m) == 0
    assert m.skipped_product_types == {"IfcTubeBundle": 1}


def test_unlisted_product_class_shows_up_in_summary():
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        m = ifcfast.open(UNLISTED, use_cache=False, write_cache=False)
    s = m.summary()
    assert s["products"] == 0
    assert s["skipped_product_types"] == {"IfcTubeBundle": 1}


def test_relationship_entities_are_not_mistaken_for_products():
    """The counter keys on the IfcProduct attribute shape, not on "starts
    with IFC" — IfcRelAggregates, IfcUnitAssignment and friends are in
    every file and must never be reported as skipped products."""
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        m = ifcfast.open(UNLISTED, use_cache=False, write_cache=False)
    assert set(m.skipped_product_types) == {"IfcTubeBundle"}


def test_cache_hit_reports_the_same_gap(tmp_path, monkeypatch):
    """A cache hit must be as loud as a cold parse — otherwise the second
    open of the same file goes back to being a silent zero."""
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path))
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        ifcfast.open(UNLISTED, use_cache=False, write_cache=True)
    with pytest.warns(UserWarning, match="IfcTubeBundle"):
        m = ifcfast.open(UNLISTED, use_cache=True, write_cache=False)
    assert m.summary()["skipped_product_types"] == {"IfcTubeBundle": 1}
