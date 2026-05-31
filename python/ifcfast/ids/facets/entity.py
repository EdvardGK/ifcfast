"""Entity facet — filter products_df by IFC class (and optional predefined type)."""

from __future__ import annotations

from typing import Any

import pandas as pd

from ifcfast.classify import _ancestors
from ifcfast.ids.context import ValidationContext
from ifcfast.ids.facets.restrictions import restriction_enumeration, value_matches_restriction


def _entity_names(facet: Any) -> set[str]:
    name = facet.name
    if isinstance(name, str):
        return {name.upper()}
    enum = restriction_enumeration(name)
    if enum:
        return {v.upper() for v in enum}
    return set()


def _entity_name_matches(actual: str, facet: Any, *, allow_subtypes: bool) -> bool:
    names = _entity_names(facet)
    if names:
        if allow_subtypes:
            chain = {a.upper() for a in _ancestors(actual, "IFC4")}
            return any(n in chain for n in names)
        return actual.upper() in names
    if hasattr(facet, "name") and facet.name is not None and hasattr(facet.name, "options"):
        return value_matches_restriction(actual.upper(), facet.name)
    return True


def entity_mask(
    ctx: ValidationContext,
    facet: Any,
    guids: pd.Index,
    *,
    applicability: bool = False,
) -> pd.Series:
    """Boolean mask (indexed by guid) for entity applicability/requirement."""
    sub = ctx.objects.loc[guids.intersection(ctx.objects.index)]
    if sub.empty:
        return pd.Series(False, index=guids)

    mask = pd.Series(
        [
            _entity_name_matches(str(row["entity_upper"]), facet, allow_subtypes=applicability)
            for _, row in sub.iterrows()
        ],
        index=sub.index,
    )

    if facet.predefinedType:
        ptype = sub["predefined_type"].astype(str)
        want = str(facet.predefinedType)
        if want == "USERDEFINED":
            mask &= ptype.str.len() > 0
            mask &= ~ptype.isin(["", "nan", "None"])
        else:
            mask &= ptype.str.upper() == want.upper()

    return mask.reindex(guids, fill_value=False)
