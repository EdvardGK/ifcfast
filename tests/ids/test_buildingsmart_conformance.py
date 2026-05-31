"""buildingSMART IDS 1.0 TestCases — parity vs IfcTester (optional, large corpus)."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

TESTCASES_ROOT = Path(
    os.environ.get(
        "IDS_TESTCASES_ROOT",
        r"C:\code\buildingSMART-IDS\Documentation\ImplementersDocumentation\TestCases",
    )
)


def _cases_for_facet(facet: str, limit: int = 20):
    root = TESTCASES_ROOT / facet
    if not root.is_dir():
        return []
    out = []
    for ids_path in sorted(root.glob("pass-*.ids"))[: limit // 2]:
        ifc = ids_path.with_suffix(".ifc")
        if ifc.is_file():
            out.append((ids_path, ifc, "pass"))
    for ids_path in sorted(root.glob("fail-*.ids"))[: limit // 2]:
        ifc = ids_path.with_suffix(".ifc")
        if ifc.is_file():
            out.append((ids_path, ifc, "fail"))
    return out


def _assert_parity(ids_path, ifc_path, expected, *, engine: str):
    from ifctester import ids as ifctester_ids
    import ifcopenshell
    from ifcfast.ids import validate

    doc = ifctester_ids.open(str(ids_path))
    ifc = ifcopenshell.open(str(ifc_path))
    doc.validate(ifc)
    ref = bool(doc.specifications[0].status)

    report = validate(ids_path, ifc_path, engine=engine, use_cache=False)
    got = bool(report.specifications[0].status)

    assert got == ref, f"parity: expected ref={ref} got={got} ({ids_path.name}, engine={engine})"
    if expected == "pass":
        assert ref is True
    else:
        assert ref is False


@pytest.mark.skipif(not TESTCASES_ROOT.is_dir(), reason="IDS_TESTCASES_ROOT not on disk")
@pytest.mark.parametrize(
    "ids_path,ifc_path,expected",
    _cases_for_facet("entity", limit=10),
    ids=lambda p: p[0].stem if isinstance(p, tuple) else str(p),
)
def test_entity_cases_match_ifctester(ids_path, ifc_path, expected):
    _assert_parity(ids_path, ifc_path, expected, engine="ifcfast")


@pytest.mark.skipif(not TESTCASES_ROOT.is_dir(), reason="IDS_TESTCASES_ROOT not on disk")
@pytest.mark.parametrize(
    "facet",
    ["entity", "attribute", "property", "classification", "material", "partof"],
)
def test_rust_facet_smoke_matches_ifctester(facet):
    cases = _cases_for_facet(facet, limit=6)
    if not cases:
        pytest.skip(f"no {facet} cases under {TESTCASES_ROOT}")
    for ids_path, ifc_path, expected in cases:
        try:
            _assert_parity(ids_path, ifc_path, expected, engine="rust")
        except ValueError as exc:
            if "engine=rust cannot run" in str(exc):
                pytest.skip(str(exc))
            raise
