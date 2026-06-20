"""Quantities oracle test (GH #59 M1).

Lifts ``test_matches_ifcopenshell_ground_truth`` out of
``tests/test_quantities.py`` and drives it through the reusable
:mod:`tests.oracle.normalize` DSL + :mod:`tests.oracle.report`
collector, so a sweep yields a *structured* diff list (not just the
first failing assert) and so the psets/materials adapters can reuse
the exact same comparison machinery.

The original test in ``tests/test_quantities.py`` is intentionally
left in place — this is a parallel, richer gate, not a replacement, so
the existing suite keeps passing unchanged.
"""

from __future__ import annotations

from . import normalize as nz
from .report import Classification, Collector

SURFACE = "quantities"
FIXTURE = "quantities.ifc"


def _ours_grouped(fast) -> dict[str, dict[str, nz.NormalizedPayload]]:
    """ifcfast quantities -> {guid: {qto_name: NormalizedPayload}}.

    ``quantities.value`` is a string-typed column whose values are all
    semantically numeric, so the ``numeric_or_passthrough`` hook coerces
    ``'3'`` to ``3.0`` before tolerance comparison against the oracle's
    floats (the original ad-hoc test did ``float(row.value)`` inline).
    """
    return nz.normalize_grouped(
        fast.quantities.itertuples(),
        guid_key="guid",
        group_key="qto_name",
        name_key="quantity_name",
        value_key="value",
        value_coerce=nz.numeric_or_passthrough,
    )


def _truth_grouped(ios) -> dict[str, dict[str, nz.NormalizedPayload]]:
    """ifcopenshell quantities -> {guid: {qto_name: NormalizedPayload}}.

    Uses ``get_psets(qtos_only=True)``; internal ``id`` keys are dropped
    by :func:`normalize_payload`. ``ifcopenshell`` is imported *inside*
    this helper (not at module scope) so the main ``pytest -q`` run —
    which collects this file without the dev extra installed — skips
    cleanly via the conftest gate rather than erroring at collection.
    """
    from ifcopenshell.util.element import get_psets

    nested: dict[str, dict[str, dict[str, object]]] = {}
    for el in ios.by_type("IfcProduct"):
        if not hasattr(el, "GlobalId"):
            continue
        qtos = get_psets(el, qtos_only=True)
        if not qtos:
            continue
        nested[el.GlobalId] = qtos
    return nz.normalize_grouped_payloads(nested)


def test_quantities_match_ifcopenshell(corpus):
    """Differential gate: ifcfast quantities == ifcopenshell ground truth.

    Collects the entire divergence surface, then asserts no blocking
    record. Any disagreement is ``unknown`` (hence blocking) until a
    classifier deliberately tags it benign.
    """
    fx = corpus(FIXTURE)

    ours = _ours_grouped(fx.fast)
    truth = _truth_grouped(fx.ios)

    # --- anti-vacuous-green guard -------------------------------------------
    # An oracle that groups on a mistyped column yields empty dicts -> zero
    # diffs -> a green pass that tested NOTHING. Assert the comparison was
    # actually populated: the quantities.ifc fixture authors 8 quantity rows
    # across 1 IfcElementQuantity group on 1 product. A column rename or an
    # empty fixture must FAIL here, not pass.
    n_groups = sum(len(groups) for groups in ours.values())
    n_quantities = sum(
        len(p.items) for groups in ours.values() for p in groups.values()
    )
    assert n_groups >= 1, f"ifcfast produced no quantity groups: {ours!r}"
    assert n_quantities >= 8, (
        f"expected >=8 quantities compared from {FIXTURE}, got {n_quantities}"
    )
    print(
        f"[oracle:{SURFACE}] compared {n_quantities} quantities "
        f"in {n_groups} group(s) across {len(ours)} product(s)"
    )

    diffs = nz.diff_grouped(ours, truth)

    collector = Collector()
    collector.extend_from_diffs(diffs, surface=SURFACE, fixture=FIXTURE)
    collector.assert_clean()


def test_collector_classification_is_blocking_by_default():
    """Guard the report contract: an untagged diff blocks CI."""
    diff = nz.GroupDiff(
        guid="X",
        group="Qto_Test",
        kind="value_mismatch",
        detail="synthetic",
    )
    col = Collector()
    col.extend_from_diffs([diff], surface=SURFACE, fixture=FIXTURE)
    assert not col.is_clean()
    assert col.blocking()[0].classification is Classification.unknown
