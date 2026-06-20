"""Property-set (pset) oracle test (GH #59 M1).

Differential gate for the agent-visible ``psets`` surface: diff
``ifcfast``'s ``m.psets`` table against
``ifcopenshell.util.element.get_psets(el, psets_only=True)`` over a
tiny committed fixture, driven through the shared
:mod:`tests.oracle.normalize` DSL + :mod:`tests.oracle.report`
collector (same machinery as ``test_quantities_oracle.py``).

Fixture choice
--------------
``minimal.ifc`` is the only committed tiny fixture that carries an
``IfcPropertySet`` (``quantities.ifc`` carries *only*
``IfcElementQuantity`` and is the quantities adapter's fixture). It
defines one wall with ``Pset_WallCommon`` ->
``{IsExternal: .T., LoadBearing: .F.}`` — two ``IfcBoolean`` props.

ifcfast column note (the value_type contract)
----------------------------------------------
ifcfast's pset table has columns
``guid, pset_name, prop_name, value, value_type, source``. The property
column is ``prop_name`` (NOT ``property_name``). The ``value`` column is
**uniformly string-typed** (``str`` dtype) and is paired with a
``value_type`` column naming the IFC primitive/measure
(``'IfcBoolean'`` / ``'IfcInteger'`` / ``'IfcReal'`` / ``'IfcLabel'`` /
length/area measures …). This is the **documented contract**, not a
defect: every pset value is a string, and ``value_type`` tells a
consumer how to read it.

So the faithful oracle comparison is **value_type-aware**: each ifcfast
string value is coerced to the type its ``value_type`` names
(:func:`normalize.coerce_by_value_type`) *before* being diffed against
ifcopenshell's natively-typed ``get_psets`` value. An ``IfcBoolean``
``'True'`` becomes ``bool`` ``True``; an ``IfcInteger`` becomes ``int``;
real measures become ``float``; an ``IfcLabel`` stays a string. This is
*representation* canonicalisation — it changes typing, never value — so
it cannot mask a real value divergence the way stringifying both sides
would (that would collapse ``'1'`` vs ``True`` vs ``1.0`` into one
bucket). It is emphatically NOT a whitelist of the boolean as a benign
disagreement.

Real disagreement surfaced (the oracle working)
-----------------------------------------------
With **no** coercion the oracle reports a genuine representational
drift on every ``IfcBoolean`` property:

    value_mismatch  IsExternal:  ours='True'  truth=True
                    LoadBearing: ours='False' truth=False

``test_psets_boolean_repr_drift_is_visible`` pins that finding in place
(it runs the bare/uncoerced diff). The primary gate
(``test_psets_match_ifcopenshell``) applies the value_type-aware
coercion, so the *semantic* comparison is exact and any *real* future
divergence (a wrong value, a dropped property, a stray pset, a
mistyped value_type) blocks.
"""

from __future__ import annotations

from typing import Any

from . import normalize as nz
from .report import Classification, Collector
from .normalize import GroupDiff

SURFACE = "psets"
FIXTURE = "minimal.ifc"


def _ours_grouped(
    fast, *, value_type_aware: bool
) -> dict[str, dict[str, nz.NormalizedPayload]]:
    """ifcfast psets -> ``{guid: {pset_name: NormalizedPayload}}``.

    Reads ``value`` AND ``value_type`` per row (both are columns on
    ``m.psets``) and, when ``value_type_aware`` is set, coerces the
    string ``value`` to the native type named by ``value_type`` via
    :func:`normalize.coerce_by_value_type` — the documented value_type
    contract. With ``value_type_aware=False`` the raw string value is
    compared as-is (used by the drift-visibility test to prove the
    oracle *catches* the string-vs-native divergence).

    Note ``prop_name`` — ifcfast's pset property column is ``prop_name``,
    not ``property_name``.
    """
    nested: dict[str, dict[str, dict[str, Any]]] = {}
    for row in fast.psets.itertuples():
        guid = row.guid
        pset = row.pset_name
        value = (
            nz.coerce_by_value_type(row.value, row.value_type)
            if value_type_aware
            else row.value
        )
        nested.setdefault(guid, {}).setdefault(pset, {})[row.prop_name] = value
    return nz.normalize_grouped_payloads(nested)


def _truth_grouped(ios) -> dict[str, dict[str, nz.NormalizedPayload]]:
    """ifcopenshell psets -> ``{guid: {pset_name: NormalizedPayload}}``.

    Uses ``get_psets(psets_only=True)`` so quantity sets don't leak in;
    the internal ``id`` key is dropped by :func:`normalize_payload`.
    ``ifcopenshell`` is imported *inside* this helper (not at module
    scope) so the main ``pytest -q`` run collects this file cleanly
    without the dev extra and skips via the conftest gate.
    """
    from ifcopenshell.util.element import get_psets

    nested: dict[str, dict[str, dict[str, object]]] = {}
    for el in ios.by_type("IfcProduct"):
        if not hasattr(el, "GlobalId"):
            continue
        psets = get_psets(el, psets_only=True)
        if not psets:
            continue
        nested[el.GlobalId] = psets
    return nz.normalize_grouped_payloads(nested)


def test_psets_match_ifcopenshell(corpus):
    """Differential gate: ifcfast psets == ifcopenshell ground truth.

    Applies value_type-aware coercion (the documented contract for the
    string-typed ``value`` column paired with ``value_type``), then
    collects the entire divergence surface and asserts no blocking
    record. With coercion the two surfaces agree exactly, so any real
    future divergence — a wrong value, a dropped or extra property, a
    missing or phantom pset, a mistyped value_type — surfaces as an
    ``unknown`` (blocking) record.
    """
    fx = corpus(FIXTURE)

    ours = _ours_grouped(fx.fast, value_type_aware=True)
    truth = _truth_grouped(fx.ios)

    # --- anti-vacuous-green guard -------------------------------------------
    # Grouping on a mistyped column yields empty dicts -> zero diffs -> a
    # green pass that tested NOTHING. The minimal.ifc fixture authors exactly
    # 2 IfcBoolean properties (IsExternal, LoadBearing) in 1 Pset_WallCommon
    # on 1 wall. A column rename or empty fixture must FAIL here.
    n_psets = sum(len(groups) for groups in ours.values())
    n_props = sum(
        len(p.items) for groups in ours.values() for p in groups.values()
    )
    assert n_psets >= 1, f"ifcfast produced no psets: {ours!r}"
    assert n_props >= 2, (
        f"expected >=2 properties compared from {FIXTURE}, got {n_props}"
    )
    print(
        f"[oracle:{SURFACE}] compared {n_props} property/-ies "
        f"in {n_psets} pset(s) across {len(ours)} product(s)"
    )

    diffs = nz.diff_grouped(ours, truth)

    collector = Collector()
    collector.extend_from_diffs(diffs, surface=SURFACE, fixture=FIXTURE)
    collector.assert_clean()


def _is_boolean_repr_drift(diff: GroupDiff) -> Classification | None:
    """Classify the known string-``"True"`` vs bool-``True`` drift as benign.

    The disagreement is benign iff it is a ``value_mismatch`` and every
    diverging key is a pure string/bool-repr pair: ifcfast ``"True"``/
    ``"False"`` against ifcopenshell ``True``/``False``. Anything else
    (a numeric value off, a property only one side has) returns
    ``None`` and stays ``unknown`` -> blocking.
    """
    if diff.kind != "value_mismatch":
        return None
    ours = diff.ours or {}
    truth = diff.truth or {}
    for key in diff.diverging_keys:
        ov = ours.get(key)
        tv = truth.get(key)
        bool_repr_pair = (
            isinstance(ov, str)
            and ov in ("True", "False")
            and isinstance(tv, bool)
            and ov == str(tv)
        )
        if not bool_repr_pair:
            return None
    return Classification.expected_drift


def test_psets_boolean_repr_drift_is_visible(corpus):
    """Pin the real finding: bare diff surfaces the bool-repr drift.

    Runs the diff with **no** ``value_coerce``, so ifcfast's
    string-typed ``IfcBoolean`` values (``"True"``/``"False"``) collide
    with ifcopenshell's native ``bool``. This documents that:

    1. the oracle *does* catch the representation drift (it is not
       silently swallowed), and
    2. the drift is exactly the ``IfcBoolean`` repr mismatch and
       nothing more — every diverging key is a ``"True"``/``True`` style
       pair, classified ``expected_drift`` (benign) by
       :func:`_is_boolean_repr_drift`.

    If ifcfast ever changed its boolean serialisation (e.g. emitted
    native bools, or a different string), this test's expectations
    would shift and flag the change for re-triage.
    """
    fx = corpus(FIXTURE)

    # Bare comparison: no value_type coercion -> the string/bool drift shows.
    ours = _ours_grouped(fx.fast, value_type_aware=False)
    truth = _truth_grouped(fx.ios)

    diffs = nz.diff_grouped(ours, truth)

    # The oracle must actually see the drift (guards against a future
    # change that makes the surfaces accidentally agree raw and renders
    # this finding-test vacuous).
    assert diffs, (
        "expected the bare (uncoerced) psets diff to surface the "
        "IfcBoolean string-vs-bool representation drift, but found none"
    )
    assert all(d.kind == "value_mismatch" for d in diffs), (
        f"expected only value_mismatch drift, got kinds "
        f"{sorted({d.kind for d in diffs})}"
    )

    # Classify the known drift benign; anything else stays blocking.
    collector = Collector()
    collector.extend_from_diffs(
        diffs,
        surface=SURFACE,
        fixture=FIXTURE,
        classify=_is_boolean_repr_drift,
    )

    # Every collected record is the benign bool-repr drift -> clean.
    collector.assert_clean()
    assert collector.summary()[Classification.expected_drift.value] == len(diffs)


def test_psets_collector_blocks_untagged_drift():
    """Guard the report contract: an untagged psets diff blocks CI.

    Mirrors the quantities adapter's contract guard — a synthetic
    value_mismatch with no classifier stays ``unknown`` (blocking),
    proving the gate fails closed.
    """
    diff = nz.GroupDiff(
        guid="X",
        group="Pset_WallCommon",
        kind="value_mismatch",
        detail="synthetic",
    )
    col = Collector()
    col.extend_from_diffs([diff], surface=SURFACE, fixture=FIXTURE)
    assert not col.is_clean()
    assert col.blocking()[0].classification is Classification.unknown
