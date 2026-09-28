"""GH #178 / #201 — the Rust product whitelist, pinned to the IFC schemas.

``crates/core/src/indexer.rs::PRODUCT_TYPES`` decides which entities get a
row at all; ``ifcfast.classify`` decides what a row *means* for a take-off.
The second is layered on the first, so an entity the indexer has never
heard of is not "classified as skip" — it is **absent**, with no row, no
mesh, no QTO and (while the file has other products) no warning.

That is how a model whose products are all ``IfcGeographicElement`` opened
as ``len(m) == 0`` (GH #178), and how 163 ``IfcCooledBeam`` elements
vanished from every table of a client model (GH #201). The second one
passed the old "whitelist covers classify.py" test, because the class was
in neither list.

Since GH #201 the list is GENERATED from the schema tables
(``scripts/gen_schema_supertypes.py`` → ``crates/core/src/schema_products.rs``).
This test pins it EQUAL to the concrete ``IfcProduct`` closure computed
here, independently, from ``ifcfast.data.schema_supertypes``.
"""

from __future__ import annotations

import pytest

from ifcfast import _core
from ifcfast.classify import (
    COUNT_ENTITIES,
    LINEAR_ENTITIES,
    MEASURE_ENTITIES,
    classify_by_name,
)
from ifcfast.data.schema_supertypes import ABSTRACT, ALL_ENTITIES, SUPERTYPE
from ifcfast.whitelist import product_types


CLASSIFIED_PRODUCTS = COUNT_ENTITIES | MEASURE_ENTITIES | LINEAR_ENTITIES

#: IfcProduct subtypes the indexer routes to their own tables instead of
#: the product whitelist (IfcSpace is dispatched separately and then also
#: emitted as a product row).
DISPATCHED_ELSEWHERE = frozenset(
    {"IfcSite", "IfcBuilding", "IfcBuildingStorey", "IfcSpace"}
)


def _schema_product_closure() -> frozenset[str]:
    """Every entity descending from IfcProduct in any supported schema and
    concrete in at least one of them, minus DISPATCHED_ELSEWHERE."""
    out: set[str] = set()
    for schema, parents in SUPERTYPE.items():
        for entity in parents:
            cur, seen = entity, set()
            while cur is not None and cur != "IfcProduct" and cur not in seen:
                seen.add(cur)
                cur = parents.get(cur)
            if cur == "IfcProduct" and entity not in ABSTRACT[schema]:
                out.add(entity)
    return frozenset(out - DISPATCHED_ELSEWHERE)


def _abstract_everywhere(entity: str) -> bool:
    declared = [
        s for s, parents in SUPERTYPE.items()
        if entity in parents or entity in parents.values()
    ]
    return bool(declared) and all(entity in ABSTRACT[s] for s in declared)


def test_core_exposes_the_whitelist():
    names = _core.product_types()
    assert isinstance(names, list)
    # Title case, ifcopenshell spelling — not the STEP token.
    assert "IfcWall" in names
    assert "IFCWALL" not in names
    # No duplicates: the Rust list is a set in disguise.
    assert len(names) == len(set(names))


def test_whitelist_equals_the_schema_product_closure():
    whitelist = product_types()
    closure = _schema_product_closure()
    missing = sorted(closure - whitelist)
    extra = sorted(whitelist - closure)
    assert not missing and not extra, (
        "crates/core/src/schema_products.rs drifted from the schema tables; "
        "re-run `.venv/bin/python scripts/gen_schema_supertypes.py`. "
        f"missing: {missing} extra: {extra}"
    )


def test_whitelist_covers_every_classified_product():
    """classify.py may name abstract supertypes (IfcReinforcingElement,
    IfcTransportationDevice) — no file can instantiate those, so they are
    exempt. Every concrete class it names must have rows."""
    whitelist = product_types()
    missing = sorted(
        e for e in CLASSIFIED_PRODUCTS - whitelist if not _abstract_everywhere(e)
    )
    assert not missing, (
        "classify.py calls these entities take-off products but the Rust "
        "tier-1 indexer does not index them at all — a file made of them "
        "opens as an EMPTY model (GH #178): " + ", ".join(missing)
    )


@pytest.mark.parametrize("entity", ["IfcGeographicElement", "IfcCivilElement"])
def test_the_two_classes_that_started_it(entity):
    assert entity in product_types()


# GH #201: every class the reporter listed, with a schema it is concrete
# in and the take-off mode it must get.
GH201_CLASSES = [
    ("IfcElectricDistributionPoint", "IFC2X3", "count"),
    ("IfcAirToAirHeatRecovery", "IFC4", "count"),
    ("IfcCooledBeam", "IFC4", "count"),
    ("IfcElectricTimeControl", "IFC4", "count"),
    ("IfcEngine", "IFC4", "count"),
    ("IfcFlowInstrument", "IFC4", "count"),
    ("IfcInterceptor", "IFC4", "count"),
    ("IfcTubeBundle", "IFC4", "count"),
    ("IfcDistributionBoard", "IFC4X3", "count"),
    ("IfcElectricFlowTreatmentDevice", "IFC4X3", "count"),
    ("IfcLiquidTerminal", "IFC4X3", "count"),
    ("IfcSignal", "IFC4X3", "count"),
    # A conveyor segment is a linear run like IfcPipeSegment: it inherits
    # LINEAR through IfcFlowSegment.
    ("IfcConveyorSegment", "IFC4X3", "linear"),
]


@pytest.mark.parametrize("entity,schema,mode", GH201_CLASSES)
def test_gh201_classes_are_indexed_and_classified(entity, schema, mode):
    assert entity in product_types()
    assert classify_by_name(entity, schema).value == mode


# Classes the schema closure newly admits that are NOT take-off products:
# they get a row (reveal-all) and must classify as skip.
@pytest.mark.parametrize(
    "entity,schema",
    [
        ("IfcStructuralCurveMember", "IFC4"),
        ("IfcStructuralSurfaceMember", "IFC2X3"),
        ("IfcStructuralPointConnection", "IFC4"),
        ("IfcStructuralPointAction", "IFC4"),
        ("IfcStructuralSurfaceReaction", "IFC4X3"),
        ("IfcAlignment", "IFC4X3"),
        ("IfcReferent", "IFC4X3"),
        ("IfcLinearElement", "IFC4X3"),
        ("IfcSpatialZone", "IFC4"),
        ("IfcExternalSpatialElement", "IFC4"),
        ("IfcFacility", "IFC4X3"),
        ("IfcOpeningStandardCase", "IFC4"),
        ("IfcProjectionElement", "IFC4"),
        ("IfcChamferEdgeFeature", "IFC2X3"),
    ],
)
def test_non_takeoff_closure_members_classify_skip(entity, schema):
    assert entity in product_types()
    assert classify_by_name(entity, schema).value == "skip"


@pytest.mark.parametrize(
    "entity",
    [
        # abstract in every schema
        "IfcProduct", "IfcElement", "IfcFeatureElementSubtraction",
        "IfcReinforcingElement",
        # own tables
        "IfcSite", "IfcBuilding", "IfcBuildingStorey", "IfcSpace",
        # not a schema entity (was a dead token in the hand list)
        "IfcFlowValve",
    ],
)
def test_not_whitelisted(entity):
    assert entity not in product_types()


def test_whitelist_names_are_real_schema_entities():
    """Every whitelist name is a schema entity in ifcopenshell spelling —
    no fallback-caser `Ifcelectricflowstoragedevice`, no dead tokens."""
    unknown = sorted(n for n in product_types() if n not in ALL_ENTITIES)
    assert not unknown, f"not IFC schema entities: {unknown}"
