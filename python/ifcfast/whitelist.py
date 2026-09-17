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
  the next drift instead of waiting for a user to find it.
* :func:`note_skipped` turns the indexer's count of *"entities shaped
  like an IfcProduct that no whitelist entry claimed"* into a
  :class:`UserWarning` whenever the model came back with zero products.
  A silent zero is worse than a loud refusal: downstream a model-health
  tool reports "0 elements" as a finding about the MODEL rather than
  about ifcfast.
"""

from __future__ import annotations

import warnings
from typing import Optional

__all__ = ["product_types", "canonical_entity_name", "note_skipped"]


_CANONICAL_BY_UPPER: Optional[dict[str, str]] = None


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

    Resolved against the generated schema entity list that ships with the
    wheel (:mod:`ifcfast.data.schema_supertypes`), which knows every
    entity in IFC2X3 / IFC4 / IFC4X3 — including the ones the indexer
    skips, which is the whole point. Unknown tokens (a non-standard or
    vendor entity) come back unchanged, in their STEP spelling.
    """
    global _CANONICAL_BY_UPPER
    if _CANONICAL_BY_UPPER is None:
        from .data.schema_supertypes import ALL_ENTITIES

        _CANONICAL_BY_UPPER = {e.upper(): e for e in ALL_ENTITIES}
    return _CANONICAL_BY_UPPER.get(upper.upper(), upper)


def note_skipped(model, raw_counts) -> dict[str, int]:
    """Attach ``skipped_product_types`` to *model* and warn on a silent zero.

    *raw_counts* is the ``{STEP_TOKEN: count}`` dict from
    ``_core.index_ifc`` (or the cache manifest, which stores the already
    canonicalised form). Keys are canonicalised to title case; the result
    is stashed on ``model._skipped_product_types`` and returned.

    The warning fires ONLY on the combination that is indefensible: the
    model has no products at all AND the file contained entities that
    look like products. A model with 5000 walls and three skipped
    ``IfcTubeBundle`` rows is a coverage gap, reported through
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
