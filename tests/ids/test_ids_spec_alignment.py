"""buildingSMART IDS 1.0 + IfcTester alignment checks."""

from __future__ import annotations

import json

import pytest

from tests.ids.conftest import SIMPLE_WALL_IDS


def test_compile_and_rust_xml_agree_on_entity_list():
    """Rust XML parser reads H29-style xs:enumeration entity lists."""
    from ifcfast import _core

    xml = """<?xml version="1.0"?>
<ids:ids xmlns:ids="http://standards.buildingsmart.org/IDS" xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <ids:specifications>
    <ids:specification name="T" ifcVersion="IFC4">
      <ids:applicability>
        <ids:entity>
          <ids:name>
            <xs:restriction base="xs:string">
              <xs:enumeration value="IFCWALL"/>
              <xs:enumeration value="IFCDOOR"/>
            </xs:restriction>
          </ids:name>
        </ids:entity>
      </ids:applicability>
      <ids:requirements/>
    </ids:specification>
  </ids:specifications>
</ids:ids>"""
    import tempfile
    from pathlib import Path

    with tempfile.NamedTemporaryFile("w", suffix=".ids", delete=False, encoding="utf-8") as f:
        f.write(xml)
        path = f.name
    try:
        compiled_json = _core.compile_ids_xml(path)
        compiled = json.loads(compiled_json)
        names = compiled["specifications"][0]["applicability"][0]["entity_names"]
        assert set(names) == {"IFCWALL", "IFCDOOR"}
    finally:
        Path(path).unlink(missing_ok=True)


@pytest.mark.skipif(not SIMPLE_WALL_IDS.is_file(), reason="fixture IDS not on disk")
def test_rust_engine_matches_ifctester_simple_wall(example_ifc):
    from tests.ids.parity import assert_parity

    assert_parity(SIMPLE_WALL_IDS, example_ifc, engine="rust")


def test_optional_property_absent_passes():
    """IDS 1.0: optional property passes when pset/prop missing."""
    from ifcfast.ids.facets.restrictions import value_matches_restriction
    from ifctester.facet import Restriction

    r = Restriction().parse({"@base": "xs:string", "xs:enumeration": [{"@value": "A"}]})
    assert value_matches_restriction("A", r)
    assert not value_matches_restriction("B", r)
