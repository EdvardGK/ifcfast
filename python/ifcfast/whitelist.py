"""Product-whitelist introspection and the silent-zero guard (GH #178).

The Rust tier-1 indexer only emits rows for entity types in its
``PRODUCT_TYPES`` list (``crates/core/src/indexer.rs``); that list is the
canonical membership source, and ``ifcfast.classify`` is a *policy* table
layered on top of it. The two drifted: ``classify.py`` listed
``IfcGeographicElement`` / ``IfcCivilElement`` under "Civil / geographic
catchalls" while the indexer had never heard of them, so a landscape or
terrain model — the correct IFC4 class for exactly that content — opened
as ``len(m) == 0`` with no error and no warning.

Two guards live here:

* :func:`product_types` re-exports the Rust list so
  ``tests/test_product_whitelist_parity_178.py`` can fail the build on
  the next drift instead of waiting for a user to find it. Since GH #201
  the Rust list is GENERATED from the schema tables
  (``scripts/gen_schema_supertypes.py`` →
  ``crates/core/src/schema_products.rs``): every concrete ``IfcProduct``
  subtype of IFC2X3 / IFC4 / IFC4X3 except ``IfcSite`` /
  ``IfcBuilding`` / ``IfcBuildingStorey`` (own tables) and ``IfcSpace``
  (dispatched separately, still emitted as a product row). A hand list
  missed 62 schema classes, ``IfcCooledBeam`` among them.
* :func:`note_skipped` turns the indexer's count of *"entities shaped
  like an IfcProduct that no whitelist entry claimed"* into a
  :class:`UserWarning` whenever the model came back with zero products.
  A silent zero is worse than a loud refusal: downstream a model-health
  tool reports "0 elements" as a finding about the MODEL rather than
  about ifcfast. With the whitelist derived from the schemas, what can
  still land here is a class outside IFC2X3 / IFC4 / IFC4X3 (a vendor
  extension, a newer schema), reported in its STEP spelling.
"""

from __future__ import annotations

import warnings

__all__ = ["product_types", "canonical_entity_name", "note_skipped"]


def product_types() -> frozenset[str]:
    """The Rust tier-1 product whitelist, in ifcopenshell title case.

    ``frozenset({"IfcWall", "IfcGeographicElement", ...})``. Reads the
    live list out of the compiled extension — never a Python copy of it,
    which is the drift this module exists to prevent.
    """
    from . import _core

    return frozenset(_core.product_types())


def canonical_entity_name(upper: str) -> str:
    """``"IFCTUBEBUNDLE"`` → ``"IfcTubeBundle"``.

    Resolved by the Rust core's schema entity table
    (``ifcfast_core::indexer::canonical_entity_name``,
    ``crates/core/src/schema_products.rs``), which knows every entity in
    IFC2X3 / IFC4 / IFC4X3 — including the ones the indexer skips, which
    is the whole point. Unknown tokens (a non-standard or vendor entity)
    come back unchanged, in their STEP spelling.

    Until GH #186 this carried its own copy of the schema entity list
    (:mod:`ifcfast.data.schema_supertypes` ``ALL_ENTITIES``) so the wheel
    and the wasm build (which has no Python) canonicalised the same
    ``skipped_product_types`` field two different ways.
    ``tests/test_product_whitelist_parity_178.py`` pins
    ``ALL_ENTITIES`` and the Rust table equal, entity for entity and
    spelling for spelling, so this delegating to the core cannot silently
    change an answer.
    """
    from . import _core

    key = upper.upper()
    canonical = _core.canonical_entity_name(key)
    # A miss echoes the (uppercased) key back; preserve the caller's
    # original casing on a miss, as this did before delegating.
    return upper if canonical == key else canonical


def note_skipped(model, raw_counts) -> dict[str, int]:
    """Attach ``skipped_product_types`` to *model* and warn on a silent zero.

    *raw_counts* is the ``{STEP_TOKEN: count}`` dict from
    ``_core.index_ifc`` (or the cache manifest, which stores the already
    canonicalised form). Keys are canonicalised to title case; the result
    is stashed on ``model._skipped_product_types`` and returned.

    The warning fires ONLY on the combination that is indefensible: the
    model has no products at all AND the file contained entities that
    look like products. A model with 5000 walls and three skipped
    vendor-class rows is a coverage gap, reported through
    ``summary()["skipped_product_types"]``, not a warning on every open.
    """
    counts: dict[str, int] = {}
    for key, n in (raw_counts or {}).items():
        name = canonical_entity_name(str(key))
        counts[name] = counts.get(name, 0) + int(n)

    model._skipped_product_types = counts

    if counts and len(model) == 0:
        listed = ", ".join(
            f"{name} ({n})" for name, n in sorted(counts.items(), key=lambda kv: -kv[1])
        )
        warnings.warn(
            f"{model.header.path}: indexed 0 products, but the file contains "
            f"entities with the IfcProduct attribute shape whose class is not "
            f"in ifcfast's product whitelist: {listed}. Those elements are "
            f"missing from every table on this model (products, meshes, QTO, "
            f"clash) — the model is not empty, this build cannot read those "
            f"classes. See m.summary()['skipped_product_types'] and GH #178.",
            UserWarning,
            stacklevel=3,
        )

    return counts
