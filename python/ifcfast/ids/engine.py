"""IDS validation engine — ifcfast tables + optional IfcTester fallback."""

from __future__ import annotations

import time
from pathlib import Path
from typing import Any, Literal

import pandas as pd

from ifcfast.ids.context import ValidationContext
from ifcfast.ids.facets import (
    attribute_mask,
    classification_mask,
    entity_mask,
    material_mask,
    partof_mask,
    property_mask,
)
from ifcfast.ids.fallback import open_ifc, validate_spec_with_ifctester
from ifcfast.ids.loader import load_ids
from ifcfast.ids.report import FacetResultSummary, IdsValidationReport, SpecificationResult
from ifcfast.ids.support import spec_fallback_reasons

EngineMode = Literal["ifcfast", "ifctester", "auto", "rust"]


def _facet_mask(
    ctx: ValidationContext, facet: Any, guids: pd.Index, *, applicability: bool = False
) -> pd.Series:
    from ifctester.facet import Attribute, Classification, Entity, Material, PartOf, Property

    if isinstance(facet, Entity):
        return entity_mask(ctx, facet, guids, applicability=applicability)
    if isinstance(facet, Attribute):
        return attribute_mask(ctx, facet, guids)
    if isinstance(facet, Property):
        return property_mask(ctx, facet, guids)
    if isinstance(facet, Classification):
        return classification_mask(ctx, facet, guids)
    if isinstance(facet, Material):
        return material_mask(ctx, facet, guids)
    if isinstance(facet, PartOf):
        return partof_mask(ctx, facet, guids)
    return pd.Series(False, index=guids)


def _validate_spec_ifcfast(ctx: ValidationContext, spec: Any) -> SpecificationResult:
    from ifctester.facet import Entity

    if not ctx.schema_matches_spec(spec):
        return SpecificationResult(
            name=spec.name or "Unnamed",
            status=True,
            ifc_version_ok=False,
            applicable_count=0,
            passed_count=0,
            failed_count=0,
            engine="ifcfast",
        )

    guids = ctx.objects.index
    candidates = guids

    for facet in spec.applicability:
        if isinstance(facet, Entity):
            candidates = candidates[
                _facet_mask(ctx, facet, candidates, applicability=True)
            ]
        else:
            m = _facet_mask(ctx, facet, candidates, applicability=True)
            candidates = candidates[m]

    applicable = list(candidates)
    still_passing: set[str] = set(applicable)
    failed_entities: set[str] = set()
    facet_summaries: list[FacetResultSummary] = []

    if spec.maxOccurs != 0:
        for facet in spec.requirements:
            if not applicable:
                facet_summaries.append(
                    FacetResultSummary(facet_type=type(facet).__name__, passed_count=0, failed_count=0)
                )
                continue
            m = _facet_mask(ctx, facet, pd.Index(applicable), applicability=False)
            still_passing = {g for g in still_passing if bool(m.loc[g])}
            facet_summaries.append(
                FacetResultSummary(
                    facet_type=type(facet).__name__,
                    passed_count=int(m.sum()),
                    failed_count=int((~m).sum()),
                )
            )

    failed_entities = {g for g in applicable if g not in still_passing}
    passed_entities = still_passing

    status = True
    for fs in facet_summaries:
        if fs.failed_count > 0:
            status = False
    if spec.minOccurs != 0 and not applicable:
        status = False
    elif spec.maxOccurs == 0 and applicable:
        status = False
    if failed_entities:
        status = False

    return SpecificationResult(
        name=spec.name or "Unnamed",
        status=status,
        ifc_version_ok=True,
        applicable_count=len(applicable),
        passed_count=len(passed_entities),
        failed_count=len(failed_entities),
        failed_guids=sorted(failed_entities),
        engine="ifcfast",
        facets=facet_summaries,
    )


def _rust_fallback_reasons(compiled: dict) -> list[str]:
    """Specs that cannot run on the native Rust engine yet (aligned with support.py)."""
    reasons: list[str] = []
    supported_partof = {
        "",
        "IFCRELAGGREGATES",
        "IFCRELCONTAINEDINSPATIALSTRUCTURE",
        "IFCRELNESTS",
        "IFCRELASSIGNSTOGROUP",
        "IFCRELVOIDSELEMENT IFCRELFILLSELEMENT",
    }
    for spec in compiled.get("specifications", []):
        for section in ("applicability", "requirements"):
            for facet in spec.get(section, []):
                kind = facet.get("kind")
                if kind == "part_of":
                    rel = " ".join(str(facet.get("partof_relation") or "").upper().split())
                    if rel not in supported_partof:
                        reasons.append(f"partof:relation:{rel or 'none'}")
                elif kind == "attribute":
                    name = facet.get("attribute_name") or "Name"
                    from ifcfast.ids.support import _PRODUCT_ATTR_COLUMNS

                    if name not in _PRODUCT_ATTR_COLUMNS:
                        reasons.append(f"attribute:column:{name}")
    return sorted(set(reasons))


def _validate_rust(
    ifc_path: Path,
    ids_doc: Any,
    *,
    ids_path: Path | None = None,
    compiled: dict | None = None,
    model: Any | None = None,
    substrate: Any | None = None,
    session: Any | None = None,
) -> IdsValidationReport:
    import json

    from ifcfast import _core
    from ifcfast.ids.compile import compile_ids_doc

    if compiled is None:
        compiled = compile_ids_doc(ids_doc, ids_path=str(ids_path) if ids_path else None)

    payload = json.dumps(compiled)
    if session is not None:
        raw = session.validate(payload)
    elif substrate is not None:
        raw = substrate.validate(payload)
    elif model is not None:
        if getattr(model, "_ids_session", None) is None:
            model.prepare_ids_session(compiled=compiled)
        raw = model.ids_session.validate(payload)
    else:
        raw = _core.validate_ids_native(str(ifc_path), payload)

    validate_s = float(raw["validate_ms"]) / 1000.0
    cached = bool(raw.get("cached", False))
    open_s = 0.0 if cached else float(raw.get("open_ms", 0.0)) / 1000.0

    specs = []
    for s in raw["specifications"]:
        specs.append(
            SpecificationResult(
                name=s["name"],
                status=bool(s["status"]),
                ifc_version_ok=bool(s["ifc_version_ok"]),
                applicable_count=int(s["applicable_count"]),
                passed_count=int(s["passed_count"]),
                failed_count=int(s["failed_count"]),
                failed_guids=list(s["failed_guids"]),
                engine="rust",
            )
        )
    return IdsValidationReport(
        ids_path=str(ids_path or getattr(ids_doc, "filepath", "") or ""),
        ifc_path=str(ifc_path),
        engine="rust",
        schema=str(raw.get("schema", "")),
        parse_seconds=None,
        open_seconds=open_s,
        validate_seconds=validate_s,
        specifications=specs,
    )


def validate_loaded(
    model: Any,
    ids_doc: Any,
    *,
    ifc_path: str | Path | None = None,
    engine: EngineMode = "auto",
    ifc_file: Any | None = None,
) -> tuple[IdsValidationReport, float]:
    """
    Run validation on an already-opened ifcfast ``Model``.

    Returns ``(report, validate_seconds)`` — open time is excluded.
    """
    ifc_path = Path(ifc_path) if ifc_path is not None else Path(model.header.path)

    if engine == "rust":
        from ifcfast.ids.compile import compile_ids_doc

        compiled = compile_ids_doc(ids_doc, ids_path=str(ifc_path))
        reasons = _rust_fallback_reasons(compiled)
        if reasons:
            raise ValueError(f"engine=rust cannot run IDS with: {reasons}")
        report = _validate_rust(ifc_path, ids_doc, compiled=compiled, model=model)
        return report, report.validate_seconds

    ctx = ValidationContext.from_model(model, ids_doc)

    if engine in ("ifctester", "auto") and ifc_file is None:
        ifc_file = open_ifc(str(ifc_path))

    t1 = time.perf_counter()
    spec_results = _run_specs(ctx, ids_doc, engine=engine, ifc_file=ifc_file)
    validate_s = time.perf_counter() - t1

    parse_s = getattr(model, "parse_seconds", None)
    if parse_s is not None:
        parse_s = float(parse_s)

    report = IdsValidationReport(
        ids_path=str(getattr(ids_doc, "filepath", None) or ""),
        ifc_path=str(ifc_path),
        engine=engine,
        schema=ctx.schema,
        parse_seconds=parse_s,
        open_seconds=0.0,
        validate_seconds=validate_s,
        specifications=spec_results,
    )
    return report, validate_s


def _run_specs(
    ctx: ValidationContext,
    ids_doc: Any,
    *,
    engine: EngineMode,
    ifc_file: Any | None,
) -> list[SpecificationResult]:
    spec_results: list[SpecificationResult] = []
    for spec in ids_doc.specifications:
        reasons = spec_fallback_reasons(spec)
        use_fallback = engine == "ifctester" or (engine == "auto" and reasons)

        if use_fallback:
            if ifc_file is None:
                ifc_file = open_ifc(str(ctx.model.header.path))
            sr = validate_spec_with_ifctester(ctx, spec, ifc_file)
            if reasons:
                sr.fallback_reasons = reasons + sr.fallback_reasons
            spec_results.append(sr)
        else:
            if engine == "ifcfast" and reasons:
                spec_results.append(
                    SpecificationResult(
                        name=spec.name or "Unnamed",
                        status=False,
                        ifc_version_ok=ctx.schema_matches_spec(spec),
                        applicable_count=0,
                        passed_count=0,
                        failed_count=0,
                        engine="ifcfast",
                        fallback_reasons=reasons,
                    )
                )
                continue
            spec_results.append(_validate_spec_ifcfast(ctx, spec))
    return spec_results


def validate(
    ids_path: str | Path | None = None,
    ifc_path: str | Path | None = None,
    *,
    ids_doc: Any | None = None,
    engine: EngineMode = "auto",
    use_cache: bool = True,
) -> IdsValidationReport:
    """
    Validate an IFC file against an IDS document.

    Parameters
    ----------
    ids_path:
        Path to ``.ids`` file (parsed with IfcTester).
    ifc_path:
        Path to ``.ifc`` file (indexed with ifcfast).
    engine:
        ``ifcfast`` — columnar engine only (raises if unsupported facets).
        ``ifctester`` — IfcOpenShell + IfcTester only.
        ``auto`` — ifcfast when possible, else per-spec IfcTester fallback.
        ``rust`` — native Rust engine (Entity/Attribute/Property/Classification/Material/PartOf).
  """
    import ifcfast

    if ifc_path is None:
        raise ValueError("ifc_path is required")
    ifc_path = Path(ifc_path)
    if ids_doc is None:
        if ids_path is None:
            raise ValueError("ids_path or ids_doc is required")
        ids_path = Path(ids_path)
        ids_doc = load_ids(ids_path)
        ids_doc.filepath = str(ids_path)
    elif ids_path is not None:
        ids_path = Path(ids_path)
        ids_doc.filepath = str(ids_path)

    if engine == "rust":
        if ids_doc is None:
            if ids_path is None:
                raise ValueError("ids_path or ids_doc is required")
            ids_path = Path(ids_path)
            ids_doc = load_ids(ids_path)
            ids_doc.filepath = str(ids_path)
        t0 = time.perf_counter()
        model = ifcfast.open(str(ifc_path), use_cache=use_cache)
        open_s = time.perf_counter() - t0
        report = _validate_rust(
            ifc_path,
            ids_doc,
            ids_path=Path(ids_path) if ids_path else None,
            model=model,
        )
        report.open_seconds = open_s
        return report

    t0 = time.perf_counter()
    model = ifcfast.open(str(ifc_path), use_cache=use_cache)
    open_s = time.perf_counter() - t0

    ifc_file = None
    if engine in ("ifctester", "auto"):
        ifc_file = open_ifc(str(ifc_path))

    report, validate_s = validate_loaded(
        model,
        ids_doc,
        ifc_path=ifc_path,
        engine=engine,
        ifc_file=ifc_file,
    )
    if ids_path is not None:
        report.ids_path = str(ids_path)
    elif getattr(ids_doc, "filepath", None):
        report.ids_path = str(ids_doc.filepath)
    report.open_seconds = open_s
    return report
