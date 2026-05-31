"""Validation context: ifcfast model + indexed tables."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

import pandas as pd

if TYPE_CHECKING:
    import ifcfast
    from ifctester.ids import Ids


@dataclass
class ValidationContext:
    model: "ifcfast.Model"
    ids_doc: "Ids"
    schema: str
    products: pd.DataFrame
    """Products + spatial containers (union, deduped by guid)."""
    objects: pd.DataFrame
    spatial: pd.DataFrame
    psets: pd.DataFrame
    quantities: pd.DataFrame
    materials: pd.DataFrame
    classifications: pd.DataFrame
    contained_in: pd.DataFrame
    aggregates: pd.DataFrame
    aggregates_transitive: pd.DataFrame
    nests: pd.DataFrame
    groups: pd.DataFrame
    voids: pd.DataFrame
    contained_in_space: pd.DataFrame
    guid_entity: dict[str, str] = field(default_factory=dict)
    guid_predefined_type: dict[str, str | None] = field(default_factory=dict)

    @classmethod
    def from_model(cls, model: "ifcfast.Model", ids_doc: "Ids") -> "ValidationContext":
        from ifcfast.ids.spatial import build_spatial_df

        products = model.products_df.set_index("guid", drop=False)
        products["entity_upper"] = products["entity"].astype(str).str.upper()
        spatial = build_spatial_df(model)
        if len(spatial):
            objects = pd.concat([products, spatial.loc[~spatial.index.isin(products.index)]])
        else:
            objects = products

        if "description" not in objects.columns:
            objects["description"] = None

        # Classification rows may reference types/materials not in the product index.
        if not model.classifications.empty and "guid" in model.classifications.columns:
            cls_guids = model.classifications["guid"].astype(str).unique()
            missing = [g for g in cls_guids if g not in objects.index]
            if missing:
                extra = pd.DataFrame({"guid": missing, "entity_upper": "IFCOBJECT", "description": None})
                extra = extra.set_index("guid", drop=False)
                objects = pd.concat([objects, extra.loc[~extra.index.isin(objects.index)]])

        guid_entity: dict[str, str] = dict(
            zip(objects["guid"].astype(str), objects["entity_upper"], strict=False)
        )
        ptype_col = objects.get("predefined_type")
        if ptype_col is not None:
            guid_predefined_type = {
                str(g): (None if pd.isna(v) else str(v).upper())
                for g, v in zip(objects["guid"].astype(str), ptype_col, strict=False)
            }
        else:
            guid_predefined_type = {}

        schema = str(model.header.schema)

        return cls(
            model=model,
            ids_doc=ids_doc,
            schema=schema,
            products=products,
            objects=objects,
            spatial=spatial,
            psets=model.psets,
            quantities=model.quantities,
            materials=model.materials,
            classifications=model.classifications,
            contained_in=model.contained_in,
            aggregates=model.aggregates,
            aggregates_transitive=model.aggregates_transitive,
            nests=model.nests,
            groups=model.groups,
            voids=model.voids,
            contained_in_space=model.contained_in_space,
            guid_entity=guid_entity,
            guid_predefined_type=guid_predefined_type,
        )

    def ifc_versions_for_spec(self, spec: Any) -> list[str]:
        versions = spec.ifcVersion
        if isinstance(versions, str):
            return [versions]
        return list(versions)

    def schema_matches_spec(self, spec: Any) -> bool:
        return self.schema in self.ifc_versions_for_spec(spec)
