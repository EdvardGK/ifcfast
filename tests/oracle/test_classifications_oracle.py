"""Classifications oracle test (GH #59 M1).

Differential gate for the ``m.classifications`` surface: ifcfast's
``IfcRelAssociatesClassification`` walk vs ifcopenshell's
``ifcopenshell.util.classification`` walk over the same committed tiny
fixture. Greenfield adapter — there was no ad-hoc classifications test
to lift, so this is built directly on the shared oracle DSL
(:mod:`tests.oracle.normalize`) + collector (:mod:`tests.oracle.report`).

Surface shape
-------------
``m.classifications`` is a DataFrame, one row per
``(product, classification-reference)`` association, with columns:
``guid`` (the *associated product's* GlobalId, not the rel/reference
GUID), ``system_name`` / ``edition`` / ``source`` (from the
``IfcClassification`` system) and ``identification`` / ``name`` /
``location`` (from the ``IfcClassificationReference``). We project that
onto ``{product_guid: {identification: {field: value}}}`` and build the
exact same nested shape from ifcopenshell, then diff field-by-field.

``identification`` (e.g. ``"232.1"``) is the group key: it's the stable
human-facing code an element is tagged with, and lets an element carry
several references (multiple classification systems) as distinct groups.

Fixture policy
--------------
Only committed tiny fixtures are used. ``minimal.ifc`` carries an
``IfcRelAssociatesClassification`` (NS 3451 "232.1 Yttervegger" on the
wall), so the test runs against it. If a future change strips all
classifications from the committed fixtures, ``_pick_fixture`` skips
cleanly with a clear reason rather than inventing data.

Inheritance note
----------------
We compare ifcopenshell's *direct* ``get_references`` (not
``get_inherited_references``) because ifcfast's substrate keys
classifications by the directly-associated product. If a fixture ever
associates a classification at the type level and expects it inherited
onto instances, that is a *real* surface difference the oracle should
flag — it must not be papered over here.
"""

from __future__ import annotations

import pytest

from . import normalize as nz
from .report import Collector

SURFACE = "classifications"

#: Tiny committed fixtures that may carry classifications, most-likely first.
#: The first one whose ifcfast ``classifications`` surface is non-empty is
#: used; if none carry any, the test skips with a clear reason.
_CANDIDATE_FIXTURES = ("minimal.ifc", "quantities.ifc", "aggregate_part.ifc")

#: The classification-reference fields compared as the per-group payload.
#: ``identification`` is excluded — it is the group key, not a payload field.
_PAYLOAD_FIELDS = ("system_name", "edition", "source", "name", "location")


def _pick_fixture(corpus):
    """Return the first candidate fixture whose ifcfast surface is non-empty.

    Skips cleanly (clear reason) if no committed tiny fixture carries any
    classification association — we never fabricate fixture data for M1.
    """
    for name in _CANDIDATE_FIXTURES:
        fx = corpus(name)
        if len(fx.fast.classifications) > 0:
            return fx
    pytest.skip(
        "no committed tiny fixture carries an IfcRelAssociatesClassification "
        f"(checked {list(_CANDIDATE_FIXTURES)}); classifications oracle has "
        "nothing to diff — add a classification-bearing fixture to enable it"
    )


def _ours_grouped(fast) -> dict[str, dict[str, nz.NormalizedPayload]]:
    """ifcfast classifications -> {product_guid: {identification: payload}}.

    Each DataFrame row is one whole reference (many fields), so we hand
    :func:`normalize_grouped_payloads` a pre-nested map rather than the
    flat row-stream form (which is one ``(name, value)`` pair per row).
    """
    nested: dict[str, dict[str, dict[str, object]]] = {}
    for row in fast.classifications.itertuples():
        d = row._asdict()
        guid = d["guid"]
        identification = str(d["identification"])
        # Loud lookup: ``d[field]`` (not ``d.get(field)``) so a future
        # column rename on m.classifications raises KeyError here instead of
        # silently injecting ``None`` and comparing None==None (a vacuous
        # pass that tests nothing).
        nested.setdefault(guid, {})[identification] = {
            field: d[field] for field in _PAYLOAD_FIELDS
        }
    return nz.normalize_grouped_payloads(nested)


def _truth_grouped(ios) -> dict[str, dict[str, nz.NormalizedPayload]]:
    """ifcopenshell classifications -> {product_guid: {identification: payload}}.

    Walks ``IfcObjectDefinition`` (every classifiable object) and reads
    *direct* references via ``get_references`` so the key set matches
    ifcfast's directly-associated-product substrate. Projects each
    reference onto the same ``_PAYLOAD_FIELDS`` map ifcfast emits, so the
    two sides are field-comparable.

    ``ifcopenshell`` is imported *inside* this helper (not at module
    scope) so the main ``pytest -q`` run collects this file cleanly
    without the dev extra and skips via the conftest gate.
    """
    import ifcopenshell.util.classification as uc

    nested: dict[str, dict[str, dict[str, object]]] = {}
    for el in ios.by_type("IfcObjectDefinition"):
        if not hasattr(el, "GlobalId"):
            continue
        refs = uc.get_references(el)
        if not refs:
            continue
        for ref in refs:
            classification = uc.get_classification(ref)
            payload = {
                "system_name": getattr(classification, "Name", None),
                "edition": getattr(classification, "Edition", None),
                "source": getattr(classification, "Source", None),
                "name": getattr(ref, "Name", None),
                "location": getattr(ref, "Location", None),
            }
            identification = str(getattr(ref, "Identification", None))
            nested.setdefault(el.GlobalId, {})[identification] = payload
    return nz.normalize_grouped_payloads(nested)


def test_classifications_match_ifcopenshell(corpus):
    """Differential gate: ifcfast classifications == ifcopenshell ground truth.

    Collects the entire divergence surface into typed records, then
    asserts none is blocking. Any untagged disagreement is ``unknown``
    (hence blocking) — the oracle reports it rather than masking it.
    """
    fx = _pick_fixture(corpus)

    ours = _ours_grouped(fx.fast)
    truth = _truth_grouped(fx.ios)

    # --- anti-vacuous-green guard -------------------------------------------
    # _pick_fixture already guarantees a non-empty ifcfast surface, but a
    # column-name slip in _ours_grouped (now caught loudly by ``d[field]``)
    # or an empty-payload projection could still collapse the comparison to
    # zero leaves -> a green pass testing nothing. The chosen fixture
    # (minimal.ifc) carries exactly 1 classification reference (NS 3451
    # "232.1") on 1 product, projected onto 5 payload fields.
    n_refs = sum(len(groups) for groups in ours.values())
    n_leaves = sum(
        len(p.items) for groups in ours.values() for p in groups.values()
    )
    assert n_refs >= 1, f"ifcfast produced no classification refs: {ours!r}"
    assert n_leaves >= len(_PAYLOAD_FIELDS), (
        f"expected >={len(_PAYLOAD_FIELDS)} classification leaf fields "
        f"compared from {fx.name}, got {n_leaves}"
    )
    print(
        f"[oracle:{SURFACE}] compared {n_refs} reference(s) "
        f"({n_leaves} leaf field(s)) across {len(ours)} product(s) "
        f"from {fx.name}"
    )

    diffs = nz.diff_grouped(ours, truth)

    collector = Collector()
    collector.extend_from_diffs(diffs, surface=SURFACE, fixture=fx.name)
    collector.assert_clean()
