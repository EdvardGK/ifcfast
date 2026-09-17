"""GH #178 — the Rust product whitelist must cover the Python policy table.

``crates/core/src/indexer.rs::PRODUCT_TYPES`` decides which entities get a
row at all; ``ifcfast.classify`` decides what a row *means* for a take-off.
The second is layered on the first, so anything classify calls a COUNT /
MEASURE / LINEAR product and the indexer has never heard of is not
"classified as skip" — it is **absent**, with no row, no mesh, no QTO and
no warning.

That is how a model whose products are all ``IfcGeographicElement`` — the
correct IFC4 class for terrain, survey markers and landscape objects, and
what the Skiplum convention uses — opened as ``len(m) == 0``.

This test is the drift guard: it reads the whitelist out of the compiled
extension, so it fails when someone adds an entity to ``classify.py``
without adding it to the indexer.
"""

from __future__ import annotations

import pytest

from ifcfast import _core
from ifcfast.classify import COUNT_ENTITIES, LINEAR_ENTITIES, MEASURE_ENTITIES
from ifcfast.whitelist import product_types


CLASSIFIED_PRODUCTS = COUNT_ENTITIES | MEASURE_ENTITIES | LINEAR_ENTITIES

# `IfcFlowValve` is not an entity in IFC2X3, IFC4 or IFC4X3 (the valve
# entity is `IfcValve`) — a pre-existing dead token in the whitelist that
# can never match a record. Named here so the schema check below is about
# NEW drift rather than failing on a known no-op; drop it from both places
# in the same change if the whitelist is ever cleaned up.
KNOWN_NON_SCHEMA = frozenset({"IfcFlowValve"})


def test_core_exposes_the_whitelist():
    names = _core.product_types()
    assert isinstance(names, list)
    # Title case, ifcopenshell spelling — not the STEP token.
    assert "IfcWall" in names
    assert "IFCWALL" not in names
    # No duplicates: the Rust list is a set in disguise.
    assert len(names) == len(set(names))


def test_whitelist_covers_every_classified_product():
    whitelist = product_types()
    missing = sorted(CLASSIFIED_PRODUCTS - whitelist)
    assert not missing, (
        "classify.py calls these entities take-off products but the Rust "
        "tier-1 indexer does not index them at all — a file made of them "
        "opens as an EMPTY model (GH #178). Add them to PRODUCT_TYPES and "
        "ENTITY_NAME_PAIRS in crates/core/src/indexer.rs: " + ", ".join(missing)
    )


@pytest.mark.parametrize("entity", ["IfcGeographicElement", "IfcCivilElement"])
def test_the_two_classes_that_started_it(entity):
    """Named explicitly so a future whitelist rewrite can't lose them
    quietly by also dropping them from classify.py (which would keep the
    set-inclusion test above green)."""
    assert entity in product_types()


def test_whitelist_names_are_real_schema_entities():
    """Every whitelist name must exist in the generated schema entity set.

    Catches two failure modes at once, both invisible in the field:
    a typo in the Rust list (`IfcGeographicElment`), and a whitelist entry
    with no `ENTITY_NAME_PAIRS` spelling — the fallback caser turns
    `IFCELECTRICFLOWSTORAGEDEVICE` into `IfcElectricflowstoragedevice`,
    which `classify.py` cannot match, so the element is indexed but
    silently classified SKIP. Same drift family as GH #178, one layer
    down."""
    from ifcfast.data.schema_supertypes import ALL_ENTITIES

    unknown = sorted(
        n for n in product_types() if n not in ALL_ENTITIES and n not in KNOWN_NON_SCHEMA
    )
    assert not unknown, f"not IFC schema entities: {unknown}"
