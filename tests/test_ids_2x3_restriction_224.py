"""GH #224 (ambiguity A6): entity-facet names inside an ``xs:restriction``
go through the IFC2X3 occurrence/type mapping exactly as the same names do
one at a time, in applicability and requirement; the requirement audit
accepts a name whose mapping-table occurrence entity is the applicability
entity (``IFCAIRTERMINAL`` under ``IFCFLOWTERMINAL``).

Not copied from IfcTester 0.9: a name that IS an IFC2X3 class
(``IFCCOLUMN``, ``IFCDOOR``) never maps through its type object, so a proxy
typed by ``IfcColumnType`` / ``IfcDoorStyle`` is not a column / door.

No buildingSMART suite case covers this; the fixtures are the GH #214
probe (``crates/core/tests/fixtures/ids_own/gh224``). Every ``app_*``
spec's requirement fails on the element, so ``fail`` means "applicable".
"""
from pathlib import Path

import pytest

import ifcfast

D = Path(__file__).resolve().parent.parent / "crates/core/tests/fixtures/ids_own/gh224"

# (ids, ifc) -> (status, applicable classes) | "invalid"
CASES = {
    # A6: a restriction member maps like the single name.
    ("app_airterminal_restriction", "flowterminal_airterminaltype"): ("fail", ["IfcFlowTerminal"]),
    ("app_airterminal_pattern", "flowterminal_airterminaltype"): ("fail", ["IfcFlowTerminal"]),
    ("app_airterminal_restriction", "flowterminal_lamptype"): ("pass", []),
    ("req_airterminal_restriction", "flowterminal_airterminaltype"): ("pass", ["IfcFlowTerminal"]),
    ("req_airterminal_restriction", "flowterminal_lamptype"): ("fail", ["IfcFlowTerminal"]),
    ("req_mixed_enumeration", "flowterminal_airterminaltype"): ("pass", ["IfcFlowTerminal"]),
    ("req_mixed_enumeration", "flowterminal_lamptype"): ("pass", ["IfcFlowTerminal"]),
    # Same answer as the single name (the occurrence entity is not checked,
    # like IfcTester 0.8.5 / 0.9 for a single name).
    ("app_airterminal_on_proxy", "proxy_airterminaltype"): ("fail", ["IfcBuildingElementProxy"]),
    ("app_airterminal_restriction_on_proxy", "proxy_airterminaltype"): (
        "fail",
        ["IfcBuildingElementProxy"],
    ),
    # IFC2X3 classes never map through a type object (0.9's over-match).
    ("app_column", "proxy_columntype"): ("pass", []),
    ("app_door", "proxy_doorstyle"): ("pass", []),
    ("app_column_door_enumeration", "proxy_columntype"): ("pass", []),
    ("app_column_door_enumeration", "proxy_doorstyle"): ("pass", []),
    ("req_door", "proxy_doorstyle"): "invalid",
    ("req_column", "proxy_columntype"): "invalid",
}


@pytest.mark.parametrize("ids_name,ifc_name", sorted(CASES))
def test_ifc2x3_restriction_names_map_like_single_names(ids_name, ifc_name):
    want = CASES[(ids_name, ifc_name)]
    ids_path, ifc_path = str(D / f"{ids_name}.ids"), str(D / f"{ifc_name}.ifc")
    if want == "invalid":
        with pytest.raises(ifcfast.IdsInvalidError, match="can never match"):
            ifcfast.validate_ids(ids_path, ifc_path)
        return
    spec = ifcfast.validate_ids(ids_path, ifc_path).to_ifctester_json()["specifications"][0]
    status, classes = want
    assert ("pass" if spec["status"] else "fail") == status
    assert [e["class"] for e in spec["applicable_entities"]] == classes
