"""JSON-serialisable IDS validation report."""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any, Literal


@dataclass
class FacetResultSummary:
    facet_type: str
    passed_count: int
    failed_count: int
    used_fallback: bool = False


@dataclass
class SpecificationResult:
    name: str
    status: bool
    ifc_version_ok: bool
    applicable_count: int
    passed_count: int
    failed_count: int
    failed_guids: list[str] = field(default_factory=list)
    engine: Literal["ifcfast", "ifctester"] = "ifcfast"
    fallback_reasons: list[str] = field(default_factory=list)
    facets: list[FacetResultSummary] = field(default_factory=list)


@dataclass
class IdsValidationReport:
    ids_path: str
    ifc_path: str
    engine: str
    schema: str
    parse_seconds: float | None
    open_seconds: float
    validate_seconds: float
    specifications: list[SpecificationResult] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)

    @property
    def total_specifications(self) -> int:
        return len(self.specifications)

    @property
    def passed_specifications(self) -> int:
        return sum(1 for s in self.specifications if s.status)

    @property
    def failed_specifications(self) -> int:
        return sum(1 for s in self.specifications if not s.status)

    @property
    def applicable_total(self) -> int:
        return sum(s.applicable_count for s in self.specifications)

    def to_dict(self) -> dict[str, Any]:
        d = asdict(self)
        d["total_specifications"] = self.total_specifications
        d["passed_specifications"] = self.passed_specifications
        d["failed_specifications"] = self.failed_specifications
        d["total_seconds"] = self.open_seconds + self.validate_seconds
        return d
