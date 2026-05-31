"""Build spatial container rows for IDS (IfcBuilding, Storey, Space, Site, Project)."""

from __future__ import annotations

from typing import TYPE_CHECKING

import pandas as pd

if TYPE_CHECKING:
    import ifcfast


def build_spatial_df(model: "ifcfast.Model") -> pd.DataFrame:
    """DataFrame indexed by guid with columns aligned to ``products_df``."""
    rows: list[dict] = []

    for storey in model.storeys:
        rows.append(
            {
                "guid": str(storey.guid),
                "entity": "IFCBUILDINGSTOREY",
                "name": storey.name,
                "description": None,
                "tag": None,
                "object_type": None,
                "predefined_type": None,
            }
        )

    for space in model.spaces:
        pt = space.predefined_type
        rows.append(
            {
                "guid": str(space.guid),
                "entity": "IFCSPACE",
                "name": space.name,
                "description": None,
                "tag": None,
                "object_type": None,
                "predefined_type": str(pt).upper() if pt else None,
            }
        )

    if not model.storey_building.empty:
        for bg in model.storey_building["building_guid"].dropna().unique():
            rows.append(
                {
                    "guid": str(bg),
                    "entity": "IFCBUILDING",
                    "name": None,
                    "description": None,
                    "tag": None,
                    "object_type": None,
                    "predefined_type": None,
                }
            )

    if not model.aggregates.empty and "parent_kind" in model.aggregates.columns:
        for parent_guid, group in model.aggregates.groupby("parent_guid"):
            kinds = set(group["parent_kind"].astype(str))
            if "building" in kinds:
                ent = "IFCBUILDING"
            elif "site" in kinds:
                ent = "IFCSITE"
            elif "project" in kinds:
                ent = "IFCPROJECT"
            else:
                continue
            rows.append(
                {
                    "guid": str(parent_guid),
                    "entity": ent,
                    "name": None,
                    "description": None,
                    "tag": None,
                    "object_type": None,
                    "predefined_type": None,
                }
            )

    raw = getattr(model, "_rust_index", None)
    if isinstance(raw, dict):
        go = raw.get("group_objects") or {}
        for guid, ent, ptype in zip(
            go.get("guid", []),
            go.get("entity", []),
            go.get("predefined_type", []),
            strict=False,
        ):
            pt = str(ptype).upper() if ptype else None
            rows.append(
                {
                    "guid": str(guid),
                    "entity": str(ent).upper(),
                    "name": None,
                    "description": None,
                    "tag": None,
                    "object_type": None,
                    "predefined_type": pt,
                }
            )

    if not rows:
        return pd.DataFrame(
            columns=[
                "guid",
                "entity",
                "name",
                "description",
                "tag",
                "object_type",
                "predefined_type",
            ]
        )

    df = pd.DataFrame(rows).drop_duplicates(subset=["guid"], keep="first")
    df["entity_upper"] = df["entity"].astype(str).str.upper()
    return df.set_index("guid", drop=False)
