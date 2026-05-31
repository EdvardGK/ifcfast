"""Detect facets that must use IfcTester / IfcOpenShell fallback."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from ifctester.ids import Specification


def _facet_type(facet: Any) -> str:
    return type(facet).__name__


def spec_fallback_reasons(spec: "Specification") -> list[str]:
    """Return non-empty if this specification cannot run fully on ifcfast tables."""
    reasons: list[str] = []
    for facet in list(spec.applicability) + list(spec.requirements):
        ft = _facet_type(facet)
        if ft == "Property":
            # dataType checked against psets.value_type when present
            pass
        elif ft == "PartOf":
            rel = getattr(facet, "relation", None) or ""
            rel_u = str(rel).upper()
            supported = {
                "",
                "IFCRELAGGREGATES",
                "IFCRELCONTAINEDINSPATIALSTRUCTURE",
                "IFCRELNESTS",
                "IFCRELASSIGNSTOGROUP",
                "IFCRELVOIDSELEMENT IFCRELFILLSELEMENT",
            }
            rel_norm = " ".join(rel_u.split())
            if rel_norm not in supported:
                reasons.append(f"partof:relation:{rel_norm or 'none'}")
        elif ft == "Attribute":
            name = getattr(facet, "name", None)
            if isinstance(name, str) and name not in _PRODUCT_ATTR_COLUMNS:
                if not hasattr(name, "options"):
                    reasons.append(f"attribute:column:{name}")
        elif ft not in ("Entity", "Attribute", "Property", "Classification", "Material"):
            reasons.append(f"unknown_facet:{ft}")
    return sorted(set(reasons))


_PRODUCT_ATTR_COLUMNS = frozenset(
    {
        "Name",
        "Description",
        "Tag",
        "ObjectType",
        "GlobalId",
        "PredefinedType",
    }
)
