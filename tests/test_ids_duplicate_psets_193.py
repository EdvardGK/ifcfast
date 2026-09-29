"""GH #193 (real-model sweep): one element carrying several IfcPropertySets
with the same Name. ifcfast evaluates a Property requirement against every
same-named set (docs/ids/ambiguities.md A47); stock IfcTester keeps one.

Walls: A = FireRating only in the second set (EI60), B = only in the first
(EI30), C = in both (EI30 first, EI60 second).
"""
import json
from pathlib import Path

import pytest

import ifcfast

D = Path(__file__).resolve().parent.parent / "crates/core/tests/fixtures/ids"
IFC = D / "duplicate_pset_names.ifc"
IDS = D.parent / "ids_own" / "duplicate_pset_names.ids"

# spec -> {wall: verdict}; the ifcfast rule.
IFCFAST = {
    "required present": {"A": "pass", "B": "pass", "C": "pass"},
    "required EI60": {"A": "pass", "B": "FAIL", "C": "pass"},
    "prohibited": {"A": "FAIL", "B": "FAIL", "C": "FAIL"},
    "optional EI60": {"A": "pass", "B": "FAIL", "C": "pass"},
}
# IfcTester 0.8.5 (`get_psets()` keeps the first set on this fixture).
IFCTESTER = {
    "required present": {"A": "FAIL", "B": "pass", "C": "pass"},
    "required EI60": {"A": "FAIL", "B": "FAIL", "C": "FAIL"},
    "prohibited": {"A": "pass", "B": "FAIL", "C": "FAIL"},
    "optional EI60": {"A": "pass", "B": "FAIL", "C": "FAIL"},
}


def _table(j):
    out = {}
    for s in j["specifications"]:
        row = {}
        for req in s["requirements"]:
            for e in req.get("passed_entities", []):
                row[e["name"]] = "pass"
            for e in req.get("failed_entities", []):
                row[e["name"]] = "FAIL"
        out[s["name"]] = row
    return out


def test_ifcfast_evaluates_every_same_named_set():
    rep = ifcfast.validate_ids(str(IDS), str(IFC))
    assert _table(rep.to_ifctester_json()) == IFCFAST


def test_ifctester_reads_one_set_and_differs():
    ifctester = pytest.importorskip("ifctester")
    ifcopenshell = pytest.importorskip("ifcopenshell")
    import ifctester.ids
    import ifctester.reporter

    ids = ifctester.ids.open(str(IDS))
    ids.validate(ifcopenshell.open(str(IFC)))
    r = ifctester.reporter.Json(ids)
    r.report()
    raw = r.to_string()
    got = _table(raw if isinstance(raw, dict) else json.loads(raw))
    assert got == IFCTESTER
    # The divergence is exactly the walls whose answer depends on which set is read.
    diff = {(s, w) for s in IFCFAST for w in "ABC" if IFCFAST[s][w] != IFCTESTER[s][w]}
    assert diff == {
        ("required present", "A"),
        ("required EI60", "A"),
        ("required EI60", "C"),
        ("prohibited", "A"),
        ("optional EI60", "C"),
    }
