"""Compare ifcfast IDS engine to IfcTester reference."""

from __future__ import annotations

from pathlib import Path


def ifctester_results(ids_path: Path, ifc_path: Path):
    from ifctester import ids as ifctester_ids
    import ifcopenshell

    doc = ifctester_ids.open(str(ids_path))
    ifc = ifcopenshell.open(str(ifc_path))
    doc.validate(ifc)
    out = []
    for spec in doc.specifications:
        failed_guids = sorted(
            {str(e.GlobalId) for e in spec.failed_entities if getattr(e, "GlobalId", None)}
        )
        out.append(
            {
                "name": spec.name,
                "status": bool(spec.status),
                "applicable": len(spec.applicable_entities),
                "passed": len(spec.passed_entities),
                "failed": len(spec.failed_entities),
                "failed_guids": failed_guids,
            }
        )
    return out


def assert_parity(ids_path: Path, ifc_path: Path, *, engine: str = "ifcfast") -> None:
    from ifcfast.ids import validate

    ref = ifctester_results(ids_path, ifc_path)
    report = validate(ids_path, ifc_path, engine=engine, use_cache=False)

    assert len(report.specifications) == len(ref), "spec count mismatch"
    for sr, r in zip(report.specifications, ref):
        assert sr.applicable_count == r["applicable"], (
            f"{sr.name}: applicable {sr.applicable_count} != {r['applicable']}"
        )
        assert sr.passed_count == r["passed"], (
            f"{sr.name}: passed {sr.passed_count} != {r['passed']}"
        )
        assert sr.failed_count == r["failed"], (
            f"{sr.name}: failed {sr.failed_count} != {r['failed']}"
        )
        assert sr.status == r["status"], f"{sr.name}: status {sr.status} != {r['status']}"
        assert set(sr.failed_guids) == set(r["failed_guids"]), (
            f"{sr.name}: failed GUID set mismatch"
        )
