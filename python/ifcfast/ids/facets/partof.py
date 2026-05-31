"""PartOf facet — aggregates, containment, nest, group, void (IfcTester-aligned)."""

from __future__ import annotations

from typing import Any

import pandas as pd

from ifcfast.ids.context import ValidationContext
from ifcfast.ids.facets.restrictions import restriction_enumeration, value_matches_restriction


def _normalize_relation(rel: str | None) -> str:
    return " ".join(str(rel or "").upper().split())


def _parent_entity(ctx: ValidationContext, parent_guid: str) -> str | None:
    return ctx.guid_entity.get(str(parent_guid))


def _parent_predefined_type(ctx: ValidationContext, parent_guid: str) -> str | None:
    return ctx.guid_predefined_type.get(str(parent_guid))


def _parent_entity_matches(ctx: ValidationContext, parent_guid: str, facet_entity: Any) -> bool:
    ent = _parent_entity(ctx, parent_guid)
    if ent is None:
        return False
    if isinstance(facet_entity, str):
        return ent == facet_entity.upper()
    enum = restriction_enumeration(facet_entity)
    if enum:
        return ent in {v.upper() for v in enum}
    if hasattr(facet_entity, "options"):
        return value_matches_restriction(ent, facet_entity)
    return True


def _parent_matches(
    ctx: ValidationContext,
    parent_guid: str,
    want_name: Any,
    want_pt: Any,
) -> bool:
    if want_name is not None:
        if not _parent_entity_matches(ctx, parent_guid, want_name):
            return False
    if want_pt is not None:
        actual = _parent_predefined_type(ctx, parent_guid)
        if str(want_pt).upper() == "USERDEFINED":
            if actual is None or not str(actual).strip():
                return False
        else:
            want = str(want_pt).upper()
            if actual is None or str(actual).upper() != want:
                return False
    return True


def _aggregate_ancestor_guids(ctx: ValidationContext, child_guid: str) -> set[str]:
    out: set[str] = set()
    if not ctx.aggregates_transitive.empty:
        sub = ctx.aggregates_transitive[ctx.aggregates_transitive["child_guid"] == child_guid]
        out.update(sub["ancestor_guid"].astype(str))
    if not ctx.aggregates.empty:
        sub = ctx.aggregates[ctx.aggregates["child_guid"] == child_guid]
        out.update(sub["parent_guid"].astype(str))
    return out


def _nest_ancestor_guids(ctx: ValidationContext, child_guid: str) -> set[str]:
    if ctx.nests.empty:
        return set()
    parent_of = dict(
        zip(
            ctx.nests["child_guid"].astype(str),
            ctx.nests["parent_guid"].astype(str),
            strict=False,
        )
    )
    out: set[str] = set()
    cur = child_guid
    seen: set[str] = set()
    while cur in parent_of:
        parent = parent_of[cur]
        if parent in seen:
            break
        seen.add(parent)
        out.add(parent)
        cur = parent
    return out


def _group_parent_guids(ctx: ValidationContext, child_guid: str) -> set[str]:
    if ctx.groups.empty:
        return set()
    sub = ctx.groups[ctx.groups["child_guid"].astype(str) == child_guid]
    return set(sub["group_guid"].astype(str))


def _container_guids(ctx: ValidationContext, child_guid: str) -> set[str]:
    out: set[str] = set()
    if not ctx.contained_in.empty:
        sub = ctx.contained_in[ctx.contained_in["product_guid"].astype(str) == child_guid]
        out.update(sub["storey_guid"].astype(str))
    if not ctx.contained_in_space.empty:
        sub = ctx.contained_in_space[
            ctx.contained_in_space["product_guid"].astype(str) == child_guid
        ]
        out.update(sub["space_guid"].astype(str))
    return out


def _void_host_guids(ctx: ValidationContext, guid: str) -> set[str]:
    """IfcTester checks the voided host element (wall/slab), not the opening."""
    if ctx.voids.empty:
        return set()
    as_host = ctx.voids[ctx.voids["host_guid"].astype(str) == guid]
    if len(as_host):
        return {guid}
    as_opening = ctx.voids[ctx.voids["opening_guid"].astype(str) == guid]
    return set(as_opening["host_guid"].astype(str))


def _any_parent_chain_matches(
    ctx: ValidationContext,
    guid: str,
    want_name: str | None,
    want_pt: Any,
    *,
    sources: str,
) -> bool:
    candidates: set[str] = set()
    if sources in ("all", "aggregate"):
        candidates |= _aggregate_ancestor_guids(ctx, guid)
    if sources in ("all", "nest"):
        candidates |= _nest_ancestor_guids(ctx, guid)
    if sources in ("all", "group"):
        candidates |= _group_parent_guids(ctx, guid)
    if sources in ("all", "container"):
        candidates |= _container_guids(ctx, guid)
    if sources == "void":
        candidates |= _void_host_guids(ctx, guid)
    if not candidates and sources == "all":
        return False
    if not want_name and want_pt is None:
        return bool(candidates)
    return any(_parent_matches(ctx, p, want_name, want_pt) for p in candidates)


def partof_mask(ctx: ValidationContext, facet: Any, guids: pd.Index) -> pd.Series:
    cardinality = getattr(facet, "cardinality", "required") or "required"
    rel = _normalize_relation(getattr(facet, "relation", None))
    want_name = facet.name if facet.name is not None else None
    want_pt = facet.predefinedType

    out = pd.Series(False, index=guids)

    for g in guids.astype(str):
        if rel == "IFCRELAGGREGATES":
            out.loc[g] = _any_parent_chain_matches(
                ctx, g, want_name, want_pt, sources="aggregate"
            )
        elif rel == "IFCRELCONTAINEDINSPATIALSTRUCTURE":
            parents = _container_guids(ctx, g)
            if not parents:
                out.loc[g] = False
            elif want_name is None and want_pt is None:
                out.loc[g] = True
            else:
                out.loc[g] = any(
                    _parent_matches(ctx, p, want_name, want_pt) for p in parents
                )
        elif rel == "IFCRELNESTS":
            out.loc[g] = _any_parent_chain_matches(ctx, g, want_name, want_pt, sources="nest")
        elif rel == "IFCRELASSIGNSTOGROUP":
            out.loc[g] = _any_parent_chain_matches(ctx, g, want_name, want_pt, sources="group")
        elif rel == "IFCRELVOIDSELEMENT IFCRELFILLSELEMENT":
            hosts = _void_host_guids(ctx, g)
            if not hosts:
                out.loc[g] = False
            elif want_name is None and want_pt is None:
                out.loc[g] = True
            else:
                out.loc[g] = any(
                    _parent_matches(ctx, p, want_name, want_pt) for p in hosts
                )
        elif rel == "":
            out.loc[g] = _any_parent_chain_matches(
                ctx, g, want_name, want_pt, sources="all"
            )
        else:
            out.loc[g] = False

    if cardinality == "prohibited":
        return ~out
    if cardinality == "optional":
        return pd.Series(True, index=guids)
    return out
