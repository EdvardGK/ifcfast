"""Property facet — vectorised pset/prop checks."""



from __future__ import annotations



from typing import Any



import pandas as pd



from ifcfast.ids.context import ValidationContext

from ifcfast.ids.facets.restrictions import restriction_enumeration, value_matches_restriction





def _value_type_matches(value_type: Any, ids_datatype: str) -> bool:

    if value_type is None:

        return False

    vt = str(value_type)

    if vt.startswith("Ifc"):

        normalized = "IFC" + vt[3:].upper()

    else:

        normalized = vt.upper()

    return normalized == ids_datatype.upper()





def _pset_names(facet: Any) -> set[str] | None:

    ps = facet.propertySet

    if ps is None:

        return None

    if isinstance(ps, str):

        return {ps}

    enum = restriction_enumeration(ps)

    return set(enum) if enum else None





def _pset_matches(pset: str, facet: Any) -> bool:

    ps = facet.propertySet

    if ps is None:

        return True

    if isinstance(ps, str):

        return pset == ps

    names = _pset_names(facet)

    if names is not None:

        return pset in names

    return bool(value_matches_restriction(pset, ps))





def _prop_names(facet: Any) -> set[str] | None:

    bn = facet.baseName

    if bn is None:

        return None

    if isinstance(bn, str):

        return {bn}

    enum = restriction_enumeration(bn)

    return set(enum) if enum else None





def _prop_name_matches(prop: str, facet: Any) -> bool:

    bn = facet.baseName

    if bn is None:

        return True

    if isinstance(bn, str):

        return prop == bn

    names = _prop_names(facet)

    if names is not None:

        return prop in names

    return bool(value_matches_restriction(prop, bn))





def _value_usable(value: Any) -> bool:

    if value is None or (isinstance(value, float) and pd.isna(value)):

        return False

    s = str(value)

    return s != "" and s != "UNKNOWN"





def _source_guids(ctx: ValidationContext, guid: str) -> set[str]:

    out = {str(guid)}

    if "type_guid" in ctx.objects.columns and guid in ctx.objects.index:

        tg = ctx.objects.loc[guid, "type_guid"]

        if tg is not None and not (isinstance(tg, float) and pd.isna(tg)):

            out.add(str(tg))

    return out





def _property_table(ctx: ValidationContext) -> pd.DataFrame:

    frames: list[pd.DataFrame] = []

    if not ctx.psets.empty:

        frames.append(ctx.psets)

    if not ctx.quantities.empty:

        q = ctx.quantities.rename(

            columns={"qto_name": "pset_name", "quantity_name": "prop_name"}

        )

        frames.append(q)

    if not frames:

        return pd.DataFrame()

    return pd.concat(frames, ignore_index=True)





def property_mask(ctx: ValidationContext, facet: Any, guids: pd.Index) -> pd.Series:

    cardinality = getattr(facet, "cardinality", "required") or "required"

    combined = _property_table(ctx)



    has_row = pd.Series(False, index=guids)

    matches = pd.Series(False, index=guids)



    if not combined.empty:

        base = combined



        requires_named = facet.baseName is not None

        for g in guids:

            sources = _source_guids(ctx, str(g))

            rows = base[base["guid"].astype(str).isin(sources)]

            if len(rows) == 0:

                continue

            rows = rows[rows["pset_name"].astype(str).apply(lambda p: _pset_matches(p, facet))]

            if len(rows) == 0:

                continue

            has_row.loc[g] = True

            ok = True

            for pset_name, pset_rows in rows.groupby(rows["pset_name"].astype(str)):

                prop_rows = pset_rows

                if requires_named:

                    prop_rows = pset_rows[

                        pset_rows["prop_name"].astype(str).apply(lambda p: _prop_name_matches(p, facet))

                    ]

                    if len(prop_rows) == 0:

                        ok = False

                        break

                for _, row in prop_rows.iterrows():

                    if not _value_usable(row.get("value")):

                        ok = False

                        break

                    if getattr(facet, "dataType", None) and "value_type" in row.index:

                        if not _value_type_matches(row.get("value_type"), str(facet.dataType)):

                            ok = False

                            break

                    if facet.value is not None:

                        if not value_matches_restriction(row.get("value"), facet.value):

                            ok = False

                            break

                if not ok:

                    break

            if ok:

                matches.loc[g] = True



    if cardinality == "prohibited":

        return ~matches if facet.value is not None else ~has_row

    if cardinality == "optional":

        if facet.value is None and getattr(facet, "dataType", None) is None:

            return pd.Series(True, index=guids)

        return ~has_row | matches

    return matches


