"""Material facet on model.materials."""

from __future__ import annotations

from typing import Any

import pandas as pd

from ifcfast.ids.context import ValidationContext
from ifcfast.ids.facets.restrictions import value_matches_restriction


def _source_guids(ctx: ValidationContext, guid: str) -> set[str]:
    out = {str(guid)}
    if "type_guid" in ctx.objects.columns and guid in ctx.objects.index:
        tg = ctx.objects.loc[guid, "type_guid"]
        if tg is not None and not (isinstance(tg, float) and pd.isna(tg)):
            out.add(str(tg))
    return out


def _row_matches_value(row: pd.Series, facet: Any) -> bool:
    if facet.value is None:
        return bool(row.get("material_name"))
    candidates: list[Any] = []
    if "material_name" in row.index:
        candidates.append(row["material_name"])
    if "category" in row.index:
        candidates.append(row["category"])
    if "layer_set_name" in row.index:
        candidates.append(row["layer_set_name"])
    return any(value_matches_restriction(v, facet.value) for v in candidates)


def material_mask(ctx: ValidationContext, facet: Any, guids: pd.Index) -> pd.Series:
    cardinality = getattr(facet, "cardinality", "required") or "required"

    if ctx.materials.empty:
        present = pd.Series(False, index=guids)
        matching = pd.Series(False, index=guids)
    else:
        mats = ctx.materials
        present = pd.Series(False, index=guids)
        matching = pd.Series(False, index=guids)
        for g in guids:
            sources = _source_guids(ctx, str(g))
            sub = mats[mats["guid"].astype(str).isin(sources)]
            if len(sub) == 0:
                continue
            present.loc[g] = True
            if facet.value is None:
                if sub["material_name"].apply(lambda v: v is not None and str(v) != "").any():
                    matching.loc[g] = True
            elif any(_row_matches_value(row, facet) for _, row in sub.iterrows()):
                matching.loc[g] = True

    if cardinality == "prohibited":
        return ~matching if facet.value is not None else ~present
    if cardinality == "optional":
        if facet.value is None:
            return pd.Series(True, index=guids)
        return ~present | matching
    return matching
