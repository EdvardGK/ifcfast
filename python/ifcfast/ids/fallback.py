"""Delegate validation to IfcTester + IfcOpenShell."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ifctester.ids import Ids, Specification

    from ifcfast.ids.context import ValidationContext
    from ifcfast.ids.report import SpecificationResult


def validate_spec_with_ifctester(
    ctx: "ValidationContext",
    spec: "Specification",
    ifc_file,
) -> "SpecificationResult":
    from ifcfast.ids.report import FacetResultSummary, SpecificationResult

    spec.reset_status()
    spec.check_ifc_version(ifc_file)
    spec.validate(ifc_file)

    failed_guids: list[str] = []
    for el in spec.failed_entities:
        gid = getattr(el, "GlobalId", None)
        if gid:
            failed_guids.append(str(gid))

    facets = []
    for facet in spec.requirements:
        facets.append(
            FacetResultSummary(
                facet_type=type(facet).__name__,
                passed_count=len(facet.passed_entities),
                failed_count=len(facet.failures),
                used_fallback=True,
            )
        )

    return SpecificationResult(
        name=spec.name or "Unnamed",
        status=bool(spec.status),
        ifc_version_ok=bool(spec.is_ifc_version),
        applicable_count=len(spec.applicable_entities),
        passed_count=len(spec.passed_entities),
        failed_count=len(spec.failed_entities),
        failed_guids=sorted(set(failed_guids)),
        engine="ifctester",
        fallback_reasons=["full_spec_ifctester"],
        facets=facets,
    )


def open_ifc(path: str):
    import ifcopenshell

    return ifcopenshell.open(path)
