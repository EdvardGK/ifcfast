"""GH #217: ``''`` and ``$`` stay distinct in every string column.

Two walls carry the same structure: ``Wall-Empty`` authors every optional
string attribute as ``''``, ``Wall-Null`` as ``$``. Every string column of
the products / storeys / psets / materials / classifications layers must
come back ``""`` for the first and null for the second — fresh parse AND
after a parquet cache round-trip (the cache must not coerce either way).
Where ifcopenshell is importable, its value is asserted to be the same
fact, so the expectation is the oracle's, not ours.
"""

from __future__ import annotations

import math

import pytest

import ifcfast

IFC = """ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('empty_217.ifc','2026-10-12T00:00:00',('ifcfast tests'),('Skiplum'),'ifcfast','ifcfast-tests','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0YvctVUKr0kugbFTf53O9L',$,'P',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1XvctVUKr0kugbFTf53O9L',$,'Site',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#11=IFCBUILDING('2XvctVUKr0kugbFTf53O9L',$,'B',$,$,#15,$,$,.ELEMENT.,$,$,$);
#12=IFCBUILDINGSTOREY('3XvctVUKr0kugbFTf53O9L',$,'',$,$,#15,$,$,.ELEMENT.,0.0);
#13=IFCBUILDINGSTOREY('3XvctVUKr0kugbFTf53O9M',$,$,$,$,#15,$,$,.ELEMENT.,3.0);
#15=IFCLOCALPLACEMENT($,#6);
#20=IFCRELAGGREGATES('4XvctVUKr0kugbFTf53O9L',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5XvctVUKr0kugbFTf53O9L',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6XvctVUKr0kugbFTf53O9L',$,$,$,#11,(#12,#13));
#30=IFCWALL('7XvctVUKr0kugbFTf53O9L',$,'','','',#15,$,'',.STANDARD.);
#31=IFCWALL('7XvctVUKr0kugbFTf53O9M',$,$,$,$,#15,$,$,.STANDARD.);
#32=IFCRELCONTAINEDINSPATIALSTRUCTURE('8XvctVUKr0kugbFTf53O9L',$,$,$,(#30,#31),#12);
#40=IFCPROPERTYSET('9XvctVUKr0kugbFTf53O9L',$,'Pset_E',$,(#41));
#41=IFCPROPERTYSINGLEVALUE('Label',$,IFCLABEL(''),$);
#42=IFCRELDEFINESBYPROPERTIES('AXvctVUKr0kugbFTf53O9L',$,$,$,(#30),#40);
#43=IFCPROPERTYSET('9XvctVUKr0kugbFTf53O9M',$,'Pset_E',$,(#44));
#44=IFCPROPERTYSINGLEVALUE('Label',$,$,$);
#45=IFCRELDEFINESBYPROPERTIES('AXvctVUKr0kugbFTf53O9M',$,$,$,(#31),#43);
#60=IFCMATERIAL('',$,'');
#61=IFCMATERIALCONSTITUENT('',$,#60,1.,'');
#62=IFCMATERIALCONSTITUENTSET('',$,(#61),$);
#63=IFCRELASSOCIATESMATERIAL('DXvctVUKr0kugbFTf53O9L',$,$,$,(#30),#62);
#64=IFCMATERIAL($,$,$);
#65=IFCMATERIALCONSTITUENT($,$,#64,1.,$);
#66=IFCMATERIALCONSTITUENTSET($,$,(#65),$);
#67=IFCRELASSOCIATESMATERIAL('DXvctVUKr0kugbFTf53O9M',$,$,$,(#31),#66);
#70=IFCCLASSIFICATION('','',$,'',$,$,$);
#71=IFCCLASSIFICATIONREFERENCE('','','',#70,$,$);
#72=IFCRELASSOCIATESCLASSIFICATION('EXvctVUKr0kugbFTf53O9L',$,$,$,(#30),#71);
#73=IFCCLASSIFICATION($,$,$,$,$,$,$);
#74=IFCCLASSIFICATIONREFERENCE($,$,$,#73,$,$);
#75=IFCRELASSOCIATESCLASSIFICATION('EXvctVUKr0kugbFTf53O9M',$,$,$,(#31),#74);
ENDSEC;
END-ISO-10303-21;
"""

EMPTY = "7XvctVUKr0kugbFTf53O9L"
NULL = "7XvctVUKr0kugbFTf53O9M"


def _isnull(v) -> bool:
    return v is None or (isinstance(v, float) and math.isnan(v))


def _row(df, guid):
    rows = df[df.guid == guid]
    assert len(rows) == 1, (guid, rows)
    return rows.iloc[0]


# (layer, columns) — every optional string column fed by the fixture.
CHECKS = {
    "products_df": ("name", "object_type", "tag"),
    "materials": ("material_name", "category", "item_name", "set_name"),
    "classifications": ("system_name", "edition", "identification", "name", "location", "source"),
    "psets": ("value",),
}


def _assert_model(m):
    for layer, cols in CHECKS.items():
        df = getattr(m, layer)
        e, n = _row(df, EMPTY), _row(df, NULL)
        for c in cols:
            assert e[c] == "", f"{layer}.{c}: '' must stay '' (got {e[c]!r})"
            assert _isnull(n[c]), f"{layer}.{c}: $ must stay null (got {n[c]!r})"
    names = {s.guid: s.name for s in m.storeys}
    assert names["3XvctVUKr0kugbFTf53O9L"] == ""
    assert names["3XvctVUKr0kugbFTf53O9M"] is None


def test_empty_string_distinct_from_null_fresh(tmp_path):
    p = tmp_path / "empty_217.ifc"
    p.write_text(IFC, encoding="utf-8")
    _assert_model(ifcfast.open(p, use_cache=False, write_cache=False))


def test_empty_string_distinct_from_null_through_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    p = tmp_path / "empty_217.ifc"
    p.write_text(IFC, encoding="utf-8")
    first = ifcfast.open(p)
    for layer in CHECKS:  # force every layer through extract + cache write
        getattr(first, layer)
    _assert_model(ifcfast.open(p))


def test_ifcopenshell_agrees_on_the_fixture(tmp_path):
    ifcopenshell = pytest.importorskip("ifcopenshell")
    p = tmp_path / "empty_217.ifc"
    p.write_text(IFC, encoding="utf-8")
    f = ifcopenshell.open(str(p))
    ref_e, ref_n = f.by_id(71), f.by_id(74)
    assert (ref_e.Identification, ref_e.Name, ref_e.Location) == ("", "", "")
    assert (ref_n.Identification, ref_n.Name, ref_n.Location) == (None, None, None)
    w_e, w_n = f.by_id(30), f.by_id(31)
    assert (w_e.Name, w_e.ObjectType, w_e.Tag) == ("", "", "")
    assert (w_n.Name, w_n.ObjectType, w_n.Tag) == (None, None, None)
    assert f.by_id(60).Name == "" and f.by_id(64).Name is None
