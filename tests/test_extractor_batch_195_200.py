"""Extractor batch GH #195-#200 (found during the GH #192 slice-2 refactor).

- #195 ``psets.value_type`` is the canonical CamelCase of the wrapper
  (``IfcThermalTransmittanceMeasure``, not ``IfcThermaltransmittancemeasure``).
- #196 a type object's own property / quantity sets are listed under the
  type's guid with ``source="instance"``; product rows are unchanged.
- #197 ``unit_scale`` resolves nested conversion-based length units
  (yard -> foot -> metre).
- #198 ``quantities.unit_step_id`` falls back to the project unit of the
  first non-empty ``IfcUnitAssignment``, conversion-based feet included.
- #199 IFC4X3 ``IfcQuantityNumber`` is readable by the IDS property facet;
  its ``quantities`` row stays the ``unhandled:`` marker.
- #200 IDS ``PROP_DATATYPE_MISMATCH`` ``actual`` is CamelCase (``IfcNumericMeasure``).

Fixtures are written to ``tmp_path`` on purpose: the nested-unit file must
not sit under ``tests/fixtures`` (the Rust legacy length-scale equality test
walks that tree and pins the pre-#197 answer there).
"""

from __future__ import annotations

import pytest

import ifcfast

HEADER = """ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('{name}','2026-09-27T00:00:00',('ifcfast tests'),('Skiplum'),'ifcfast','ifcfast-tests','');
FILE_SCHEMA(('{schema}'));
ENDSEC;
DATA;
"""
FOOTER = "ENDSEC;\nEND-ISO-10303-21;\n"

SPATIAL = """#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#9=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);
#10=IFCSITE('1SITEgUIDgUIDgUIDgUID0',$,'Site',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#11=IFCBUILDING('2BLDGgUIDgUIDgUIDgUID0',$,'Building',$,$,#15,$,$,.ELEMENT.,$,$,$);
#12=IFCBUILDINGSTOREY('3STORgUIDgUIDgUIDgUID0',$,'Plan 01',$,$,#15,$,'Plan 01',.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4AGG1gUIDgUIDgUIDgUID0',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5AGG2gUIDgUIDgUIDgUID0',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6AGG3gUIDgUIDgUIDgUID0',$,$,$,#11,(#12));
#30=IFCWALL('7WALL1UIDgUIDgUIDgUID0',$,'Wall-001',$,$,#16,$,'tag-001',.STANDARD.);
#31=IFCRELCONTAINEDINSPATIALSTRUCTURE('8CONT1UIDgUIDgUIDgUID0',$,$,$,(#30),#12);
"""

WALL = "7WALL1UIDgUIDgUIDgUID0"
WALL_TYPE = "FWTYPEgUIDgUIDgUIDgUI0"

# Feet project (conversion-based LENGTHUNIT) + a wall type carrying its own
# pset and quantity set.
FEET = (
    HEADER.format(name="feet_types.ifc", schema="IFC4")
    + """#1=IFCPROJECT('0PROJgUIDgUIDgUIDgUID0',$,'Feet',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#8,#4));
#3=IFCCONVERSIONBASEDUNIT(#9,.LENGTHUNIT.,'FOOT',#80);
#80=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.3048),#81);
#81=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#8=IFCSIUNIT(*,.AREAUNIT.,$,.SQUARE_METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
"""
    + SPATIAL
    + """#40=IFCELEMENTQUANTITY('BQTOWALLUIDgUIDgUIDgUI',$,'Qto_WallBaseQuantities',$,$,(#41,#42));
#41=IFCQUANTITYLENGTH('Length',$,$,10.,$);
#42=IFCQUANTITYAREA('NetSideArea',$,$,7.5,$);
#47=IFCRELDEFINESBYPROPERTIES('CRELDFWUIDgUIDgUIDgUI0',$,$,$,(#30),#40);
#60=IFCWALLTYPE('FWTYPEgUIDgUIDgUIDgUI0',$,'WT',$,$,(#61,#64),$,$,$,.STANDARD.);
#61=IFCPROPERTYSET('GPSETTgUIDgUIDgUIDgUI0',$,'Pset_WallCommon',$,(#62,#63));
#62=IFCPROPERTYSINGLEVALUE('FireRating',$,IFCLABEL('EI60'),$);
#63=IFCPROPERTYSINGLEVALUE('ThermalTransmittance',$,IFCTHERMALTRANSMITTANCEMEASURE(0.2),$);
#64=IFCELEMENTQUANTITY('HQTOTYgUIDgUIDgUIDgUI0',$,'Qto_WallBaseQuantities',$,$,(#65));
#65=IFCQUANTITYLENGTH('Width',$,$,0.75,$);
#66=IFCRELDEFINESBYTYPE('IRELTYgUIDgUIDgUIDgUI0',$,$,$,(#30),#60);
"""
    + FOOTER
)

YARD = (
    HEADER.format(name="yard_nested.ifc", schema="IFC4")
    + """#1=IFCPROJECT('0PROJgUIDgUIDgUIDgUID0',$,'Yard',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCCONVERSIONBASEDUNIT(#9,.LENGTHUNIT.,'yard',#82);
#82=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(3.),#83);
#83=IFCCONVERSIONBASEDUNIT(#9,.LENGTHUNIT.,'foot',#80);
#80=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.3048),#81);
#81=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
"""
    + SPATIAL
    + FOOTER
)

# Zero-offset IfcConversionBasedUnitWithOffset LENGTHUNIT (#197): a plain
# factor; the indexer must route it to the unit collector.
WITH_OFFSET = (
    HEADER.format(name="foot_with_offset.ifc", schema="IFC4")
    + """#1=IFCPROJECT('0PROJgUIDgUIDgUIDgUID0',$,'Offset',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCCONVERSIONBASEDUNITWITHOFFSET(#9,.LENGTHUNIT.,'FOOT',#80,0.);
#80=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.3048),#81);
#81=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
"""
    + SPATIAL
    + FOOTER
)

QNUM = (
    HEADER.format(name="quantity_number.ifc", schema="IFC4X3_ADD2")
    + """#1=IFCPROJECT('0PROJgUIDgUIDgUIDgUID0',$,'QNum',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
"""
    + SPATIAL
    + """#40=IFCELEMENTQUANTITY('BQTOWALLUIDgUIDgUIDgUI',$,'Qto_Custom',$,$,(#41,#42));
#41=IFCQUANTITYNUMBER('Panels',$,$,4.,$);
#42=IFCQUANTITYLENGTH('Length',$,$,3.,$);
#47=IFCRELDEFINESBYPROPERTIES('CRELDFWUIDgUIDgUIDgUI0',$,$,$,(#30),#40);
"""
    + FOOTER
)


def _open(tmp_path, name, text, **kw):
    p = tmp_path / name
    p.write_text(text, encoding="ascii")
    return ifcfast.open(p, use_cache=False, write_cache=False, **kw)


def _rows(df, guid):
    return df[df["guid"] == guid].reset_index(drop=True)


def test_value_type_is_canonical_camel_case_195(tmp_path):
    m = _open(tmp_path, "feet_types.ifc", FEET)
    ps = m.psets
    vt = dict(zip(ps["prop_name"], ps["value_type"]))
    assert vt["ThermalTransmittance"] == "IfcThermalTransmittanceMeasure"
    assert vt["FireRating"] == "IfcLabel"


def test_type_own_psets_and_quantities_196(tmp_path):
    m = _open(tmp_path, "feet_types.ifc", FEET)
    ps = m.psets
    # Product rows: inherited from the type, unchanged semantics.
    wall = _rows(ps, WALL)
    assert sorted(zip(wall["prop_name"], wall["source"])) == [
        ("FireRating", "type"),
        ("ThermalTransmittance", "type"),
    ]
    # The type's own sets under the type's guid, declared on it -> "instance".
    own = _rows(ps, WALL_TYPE)
    assert list(own["pset_name"]) == ["Pset_WallCommon", "Pset_WallCommon"]
    assert list(own["prop_name"]) == ["FireRating", "ThermalTransmittance"]
    assert list(own["value"]) == ["EI60", "0.2"]
    assert set(own["source"]) == {"instance"}
    # Type rows come after every product row.
    assert list(ps["guid"]).index(WALL_TYPE) > max(i for i, g in enumerate(ps["guid"]) if g == WALL)

    q = m.quantities
    wq = _rows(q, WALL)
    assert list(zip(wq["quantity_name"], wq["source"])) == [
        ("Length", "instance"),
        ("NetSideArea", "instance"),
        ("Width", "type"),
    ]
    tq = _rows(q, WALL_TYPE)
    assert list(tq["quantity_name"]) == ["Width"]
    assert list(tq["source"]) == ["instance"]
    assert list(tq["qto_name"]) == ["Qto_WallBaseQuantities"]


def test_quantity_unit_fallback_includes_conversion_based_feet_198(tmp_path):
    m = _open(tmp_path, "feet_types.ifc", FEET)
    assert m.unit_scale == pytest.approx(0.3048)
    q = m.quantities
    unit = dict(zip(zip(q["guid"], q["quantity_name"]), q["unit_step_id"]))
    # LENGTHUNIT is IfcConversionBasedUnit #3 (was None: SI-only fallback).
    assert unit[(WALL, "Length")] == 3
    assert unit[(WALL, "Width")] == 3
    assert unit[(WALL_TYPE, "Width")] == 3
    # AREAUNIT is the IfcSIUnit #8, as before.
    assert unit[(WALL, "NetSideArea")] == 8


def test_nested_conversion_length_unit_resolves_197(tmp_path):
    # Was: unit_scale None, and strict=True raised ValueError.
    m = _open(tmp_path, "yard_nested.ifc", YARD)
    assert m.unit_scale == pytest.approx(0.9144, rel=1e-12)


def test_zero_offset_conversion_length_unit_resolves_197(tmp_path):
    # Was: the indexer never saw IfcConversionBasedUnitWithOffset, so
    # unit_scale was None.
    m = _open(tmp_path, "foot_with_offset.ifc", WITH_OFFSET)
    assert m.unit_scale == pytest.approx(0.3048, rel=1e-12)


def test_quantity_number_marker_row_unchanged_199(tmp_path):
    m = _open(tmp_path, "quantity_number.ifc", QNUM)
    q = m.quantities
    row = q[q["quantity_name"] == "Panels"].iloc[0]
    assert row["quantity_type"] == "unhandled:IFCQUANTITYNUMBER"
    assert row["value"] is None or row["value"] != row["value"]  # null
    assert row["unit_step_id"] is None or row["unit_step_id"] != row["unit_step_id"]


IDS = """<?xml version="1.0" encoding="utf-8"?>
<ids xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="http://standards.buildingsmart.org/IDS http://standards.buildingsmart.org/IDS/1.0/ids.xsd" xmlns="http://standards.buildingsmart.org/IDS">
  <info><title>batch 199/200</title></info>
  <specifications>
{specs}
  </specifications>
</ids>"""

SPEC = """    <specification name="{name}" ifcVersion="IFC4X3_ADD2">
      <applicability maxOccurs="unbounded">
        <entity><name><simpleValue>IFCWALL</simpleValue></name></entity>
      </applicability>
      <requirements>
        <property dataType="{dt}">
          <propertySet><simpleValue>Qto_Custom</simpleValue></propertySet>
          <baseName><simpleValue>Panels</simpleValue></baseName>
          <value><simpleValue>{value}</simpleValue></value>
        </property>
      </requirements>
    </specification>"""


def test_ids_reads_quantity_number_and_camel_case_actual_199_200(tmp_path):
    ifc = tmp_path / "quantity_number.ifc"
    ifc.write_text(QNUM, encoding="ascii")
    specs = "\n".join(
        [
            SPEC.format(name="ok", dt="IFCNUMERICMEASURE", value="4"),
            SPEC.format(name="bad_value", dt="IFCNUMERICMEASURE", value="5"),
            SPEC.format(name="bad_type", dt="IFCCOUNTMEASURE", value="4"),
        ]
    )
    ids = tmp_path / "q.ids"
    ids.write_text(IDS.format(specs=specs), encoding="utf-8")
    r = ifcfast.validate_ids(str(ids), str(ifc), on_unsupported="raise")
    status = dict(zip(r.specs["name"], r.specs["status"]))
    # Was PROP_UNSUPPORTED (fail) on all three.
    assert status == {"ok": "pass", "bad_value": "fail", "bad_type": "fail"}
    f = r.failures.merge(r.specs[["spec_index", "name"]], on="spec_index")
    got = {
        n: (c, a) for n, c, a in zip(f["name"], f["reason_code"], f["actual"])
    }
    assert got["bad_value"] == ("PROP_VALUE_MISMATCH", "4.0")
    # IfcTester prints the schema spelling (facet.py:752-757).
    assert got["bad_type"] == ("PROP_DATATYPE_MISMATCH", "IfcNumericMeasure")
