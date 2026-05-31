"""Spatial container entity applicability (IfcSpace, IfcBuildingStorey, …)."""

from __future__ import annotations

import tempfile
from pathlib import Path

import pytest


def test_spatial_df_includes_space_and_storey(example_ifc):
    import ifcfast
    from ifcfast.ids.spatial import build_spatial_df

    model = ifcfast.open(str(example_ifc), use_cache=False)
    spatial = build_spatial_df(model)
    entities = set(spatial["entity_upper"].unique()) if len(spatial) else set()
    # minimal.ifc may or may not have spaces; storeys are common
    assert "IFCBUILDINGSTOREY" in entities or len(model.storeys) == 0


def test_entity_applicability_ifcspace_no_fallback():
    """Specs targeting IFCSPACE should not require IfcTester for entity facet alone."""
    from ifcfast.ids.support import spec_fallback_reasons
    from ifctester import ids as ifctester_ids

    xml = """<?xml version="1.0"?>
<ids xmlns="http://standards.buildingsmart.org/IDS">
  <info><title>Spatial test</title></info>
  <specifications>
    <specification name="Spaces" ifcVersion="IFC4">
      <applicability>
        <entity><name><simpleValue>IFCSPACE</simpleValue></name></entity>
      </applicability>
      <requirements>
        <attribute cardinality="optional"><name><simpleValue>Name</simpleValue></name></attribute>
      </requirements>
    </specification>
  </specifications>
</ids>"""
    with tempfile.NamedTemporaryFile("w", suffix=".ids", delete=False, encoding="utf-8") as f:
        f.write(xml)
        path = Path(f.name)
    try:
        doc = ifctester_ids.open(str(path))
        reasons = spec_fallback_reasons(doc.specifications[0])
        assert "entity:spatial_container" not in reasons
    finally:
        path.unlink(missing_ok=True)
