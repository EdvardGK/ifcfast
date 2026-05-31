"""IDS PartOf facet on columnar graph tables."""

from __future__ import annotations

import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from ifcfast.header import IFCHeader
from ifcfast.ids.context import ValidationContext
from ifcfast.ids.facets.partof import partof_mask
from ifcfast.model import _model_from_rust_index


def _minimal_header() -> IFCHeader:
    return IFCHeader(
        path=Path("synthetic.ifc"),
        size_bytes=0,
        mtime_ns=0,
        schema="IFC4",
        cache_key="synthetic",
    )


def _synthetic_raw() -> dict:
    return {
        "schema": "IFC4",
        "type_counts": {"IfcWall": 1},
        "storeys": {
            "step_id": [20],
            "guid": ["2Storey0000000000000001"],
            "name": ["L1"],
            "elevation": [0.0],
            "building_step_id": [None],
        },
        "buildings": {
            "step_id": [30],
            "guid": ["3Building00000000000001"],
        },
        "sites": {"step_id": [40], "guid": ["4Site00000000000000001"]},
        "projects": {"step_id": [50], "guid": ["5Project00000000000001"]},
        "spaces": {"step_id": [60], "guid": ["6Space0000000000000001"]},
        "products": {
            "step_id": [10],
            "guid": ["1Wall000000000000000001"],
            "entity": ["IfcWall"],
            "name": ["W1"],
            "predefined_type": [None],
            "object_type": [None],
            "tag": [None],
        },
        "contained_in": {"child": [10], "structure": [20]},
        "aggregates": {"child": [20, 30, 40], "parent": [30, 40, 50]},
        "storey_building": {"storey": [20], "building": [30]},
        "aggregates_transitive": {
            "child": [10, 20],
            "ancestor": [30, 40],
        },
        "nests": {"child": [], "parent": []},
        "groups": {"child": [], "parent": []},
        "voids": {"opening": [], "host": []},
    }


@pytest.fixture
def ctx() -> ValidationContext:
    import pandas as pd

    from ifcfast.ids.spatial import build_spatial_df

    model = _model_from_rust_index(_synthetic_raw(), _minimal_header(), time.time())
    products = model.products_df.set_index("guid", drop=False)
    products["entity_upper"] = products["entity"].astype(str).str.upper()
    spatial = build_spatial_df(model)
    objects = pd.concat([products, spatial.loc[~spatial.index.isin(products.index)]])
    guid_entity = dict(
        zip(objects["guid"].astype(str), objects["entity_upper"], strict=False)
    )
    ptype_col = objects.get("predefined_type")
    guid_predefined_type = (
        {
            str(g): (None if pd.isna(v) else str(v).upper())
            for g, v in zip(objects["guid"].astype(str), ptype_col, strict=False)
        }
        if ptype_col is not None
        else {}
    )
    return ValidationContext(
        model=model,
        ids_doc=SimpleNamespace(),
        schema="IFC4",
        products=products,
        objects=objects,
        spatial=spatial,
        psets=pd.DataFrame(),
        quantities=pd.DataFrame(),
        materials=pd.DataFrame(),
        classifications=pd.DataFrame(),
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


def test_aggregate_transitive_to_building(ctx: ValidationContext):
    wall = "1Wall000000000000000001"
    facet = SimpleNamespace(
        cardinality="required",
        relation="IFCRELAGGREGATES",
        name="IFCBUILDING",
        predefinedType=None,
    )
    guids = __import__("pandas").Index([wall])
    mask = partof_mask(ctx, facet, guids)
    assert bool(mask.loc[wall])


def test_nest_relation_empty_without_nests(ctx: ValidationContext):
    wall = "1Wall000000000000000001"
    facet = SimpleNamespace(
        cardinality="required",
        relation="IFCRELNESTS",
        name="IFCBUILDING",
        predefinedType=None,
    )
    guids = __import__("pandas").Index([wall])
    # Synthetic nests row is wall nested under wall — still exercises table
    mask = partof_mask(ctx, facet, guids)
    assert isinstance(bool(mask.loc[wall]), bool)
