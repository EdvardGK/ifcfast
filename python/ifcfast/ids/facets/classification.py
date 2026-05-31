"""Classification facet on model.classifications."""

from __future__ import annotations

from typing import Any

import pandas as pd

from ifcfast.ids.context import ValidationContext
from ifcfast.ids.facets.restrictions import value_matches_restriction


def classification_mask(ctx: ValidationContext, facet: Any, guids: pd.Index) -> pd.Series:
    cardinality = getattr(facet, "cardinality", "required") or "required"
    has_constraint = facet.value is not None or facet.system is not None

    if ctx.classifications.empty:
        present = pd.Series(False, index=guids)
        matching = pd.Series(False, index=guids)
    else:
        cls = ctx.classifications
        present = pd.Series(False, index=guids)
        matching = pd.Series(False, index=guids)
        for g in guids:
            sources = {str(g)}
            if "type_guid" in ctx.objects.columns and g in ctx.objects.index:
                tg = ctx.objects.loc[g, "type_guid"]
                if tg is not None and not (isinstance(tg, float) and pd.isna(tg)):
                    sources.add(str(tg))
            sub = cls[cls["guid"].astype(str).isin(sources)]
            if len(sub) == 0:
                continue
            present.loc[g] = True
            if facet.value is not None and "identification" in sub.columns:
                sub = sub[
                    sub["identification"].apply(
                        lambda v: value_matches_restriction(v, facet.value)
                    )
                ]
            if facet.system is not None and "system_name" in sub.columns:
                sub = sub[
                    sub["system_name"].apply(
                        lambda v: value_matches_restriction(v, facet.system)
                    )
                ]
            if len(sub) > 0:
                matching.loc[g] = True

    if cardinality == "prohibited":
        return ~matching if has_constraint else ~present
    if cardinality == "optional":
        if not has_constraint:
            return pd.Series(True, index=guids)
        return ~present | matching
    return matching
