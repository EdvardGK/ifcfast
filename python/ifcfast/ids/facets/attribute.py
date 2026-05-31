"""Attribute facet on products_df columns."""



from __future__ import annotations



from typing import Any



import pandas as pd



from ifcfast.ids.context import ValidationContext

from ifcfast.ids.facets.restrictions import restriction_enumeration, value_matches_restriction

from ifcfast.ids.support import _PRODUCT_ATTR_COLUMNS



_COLUMN_MAP = {

    "Name": "name",

    "Description": "description",

    "Tag": "tag",

    "ObjectType": "object_type",

    "GlobalId": "guid",

    "PredefinedType": "predefined_type",

}





def _series_for_attribute(ctx: ValidationContext, attr_name: str, guids: pd.Index) -> pd.Series:

    col = _COLUMN_MAP.get(attr_name)

    if col and col in ctx.objects.columns:

        return ctx.objects.loc[guids, col]

    return pd.Series(index=guids, dtype=object)





def _is_falsey(value: Any) -> bool:

    if value is None or (isinstance(value, float) and pd.isna(value)):

        return True

    if isinstance(value, str) and value == "":

        return True

    if isinstance(value, str):

        if value == "" or value == "UNKNOWN":

            return True

    return False





def attribute_mask(ctx: ValidationContext, facet: Any, guids: pd.Index) -> pd.Series:

    """Mask of guids passing attribute facet (applicability or requirement)."""

    cardinality = getattr(facet, "cardinality", "required") or "required"

    attr_name = facet.name

    if not isinstance(attr_name, str):

        from ifcfast.ids.facets.restrictions import restriction_enumeration



        allowed = restriction_enumeration(attr_name) or []

        cols = [_COLUMN_MAP[a] for a in allowed if a in _COLUMN_MAP]

        if not cols:

            return pd.Series(False, index=guids)

        present = pd.Series(False, index=guids)

        for col in cols:

            s = ctx.objects.loc[guids, col]

            present |= s.notna() & ~s.apply(_is_falsey)

        attr_name = allowed[0] if len(allowed) == 1 else "Name"



    values = _series_for_attribute(ctx, attr_name if isinstance(attr_name, str) else "Name", guids)



    col = _COLUMN_MAP.get(attr_name if isinstance(attr_name, str) else "Name")

    if col and col in ctx.objects.columns:

        absent = pd.Series(False, index=guids)

    else:

        absent = values.isna()

    falsey = values.apply(_is_falsey)

    has_value = ~falsey



    if facet.value is None:

        if cardinality == "prohibited":

            return ~has_value

        if cardinality == "optional":

            return absent | has_value

        return has_value



    matches = values.apply(lambda v: value_matches_restriction(v, facet.value))

    if cardinality == "prohibited":

        return ~matches

    if cardinality == "optional":

        return absent | (has_value & matches)

    return has_value & matches


