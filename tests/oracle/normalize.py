"""Reusable tolerance / ordering DSL for oracle diffs (GH #59 M1).

The oracle compares two independently-derived views of the same IFC
surface — ``ifcfast``'s extractor output vs ``ifcopenshell``'s. Raw
equality is the wrong gate: floats carry kernel-dependent rounding,
row order is non-deterministic, and ``ifcopenshell`` injects internal
bookkeeping keys (notably ``"id"``) that ifcfast never surfaces. This
module centralises all three concerns so every adapter
(quantities / psets / materials) compares values the same way.

Public API
----------
``floats_close(a, b, *, rel_tol, abs_tol) -> bool``
    Scalar float compare with combined absolute + relative tolerance.

``values_equal(a, b, *, rel_tol, abs_tol) -> bool``
    Type-aware compare: floats via :func:`floats_close`, everything
    else via ``==`` (with int/float cross-type promotion).

``drop_internal_keys(d, *, internal=DEFAULT_INTERNAL_KEYS) -> dict``
    Return a copy of ``d`` without ifcopenshell internal keys (``id``
    by default). Mirrors the ad-hoc ``k != "id"`` filter that lived in
    ``tests/test_quantities.py``.

``normalize_payload(payload, *, rel_tol, abs_tol, internal=...) -> NormalizedPayload``
    Normalise a single name->value mapping (one pset/qto) into a
    canonical, hashable, order-independent form suitable for set
    comparison. Drops internal keys; coerces numeric values to a
    tolerance-bucketed representation.

``normalize_grouped(rows, *, guid_key, group_key, name_key, value_key, ...) ->``
``    dict[str, dict[str, NormalizedPayload]]``
    Normalise a flat list-of-rows (or list of mappings / namedtuples)
    into ``{guid: {group_name: NormalizedPayload}}`` — the canonical
    nested shape both libraries are projected onto before diffing.
    ``group_key`` is the qto_name / pset_name dimension.

``diff_grouped(ours, truth, *, rel_tol, abs_tol) -> list[GroupDiff]``
    Compare two ``{guid: {group: payload}}`` structures and return a
    list of structured per-(guid, group) disagreements. Empty list ==
    full agreement. Never raises on mismatch — collects everything.

Tolerances default to ``DEFAULT_REL_TOL`` / ``DEFAULT_ABS_TOL`` and are
always keyword-overridable per call so a noisy surface (geometry) can
loosen them without touching a tight one (counts).

Usage
-----
>>> from tests.oracle import normalize as nz
>>> ours = nz.normalize_grouped(
...     m.quantities.itertuples(),
...     guid_key="guid", group_key="qto_name",
...     name_key="quantity_name", value_key="value",
... )
>>> truth = nz.normalize_grouped(
...     truth_rows, guid_key="guid", group_key="pset_name",
...     name_key="name", value_key="value",
... )
>>> diffs = nz.diff_grouped(ours, truth)
>>> assert not diffs, diffs
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Any, Callable, Iterable, Mapping

# --- tolerance defaults ------------------------------------------------------

#: Default relative tolerance for float compares. Loose enough to absorb
#: kernel rounding on authored QTO values, tight enough to catch a real
#: extractor bug (a dropped factor, a unit-scale slip).
DEFAULT_REL_TOL: float = 1e-6

#: Default absolute tolerance — dominates near zero where rel_tol is useless.
DEFAULT_ABS_TOL: float = 1e-9

#: ifcopenshell injects these bookkeeping keys into ``get_psets`` payloads;
#: ifcfast never surfaces them, so they're dropped before comparison.
DEFAULT_INTERNAL_KEYS: frozenset[str] = frozenset({"id"})


# --- scalar compares ---------------------------------------------------------

def floats_close(
    a: float,
    b: float,
    *,
    rel_tol: float = DEFAULT_REL_TOL,
    abs_tol: float = DEFAULT_ABS_TOL,
) -> bool:
    """Return True if ``a`` and ``b`` agree within abs+rel tolerance.

    NaN compares equal to NaN here (both-NaN is "agreement" for a diff
    tool); inf compares by sign. Otherwise delegates to
    :func:`math.isclose` with both tolerances active.
    """
    if math.isnan(a) and math.isnan(b):
        return True
    if math.isinf(a) or math.isinf(b):
        return a == b
    return math.isclose(a, b, rel_tol=rel_tol, abs_tol=abs_tol)


def _is_number(x: Any) -> bool:
    return isinstance(x, (int, float)) and not isinstance(x, bool)


def values_equal(
    a: Any,
    b: Any,
    *,
    rel_tol: float = DEFAULT_REL_TOL,
    abs_tol: float = DEFAULT_ABS_TOL,
) -> bool:
    """Type-aware equality used by the diff engine.

    Numbers (int/float, not bool) compare via :func:`floats_close`, so
    ``3`` and ``3.0`` agree. Everything else falls back to ``==``.
    """
    if _is_number(a) and _is_number(b):
        return floats_close(float(a), float(b), rel_tol=rel_tol, abs_tol=abs_tol)
    return a == b


# --- payload normalization ---------------------------------------------------

def drop_internal_keys(
    d: Mapping[str, Any],
    *,
    internal: Iterable[str] = DEFAULT_INTERNAL_KEYS,
) -> dict[str, Any]:
    """Copy ``d`` without ifcopenshell internal bookkeeping keys."""
    internal_set = set(internal)
    return {k: v for k, v in d.items() if k not in internal_set}


def _coerce(value: Any, value_coerce: "ValueCoerce | None" = None) -> Any:
    """Canonicalise a leaf value for storage in a NormalizedPayload.

    Numbers become ``float`` so int/float authored vs derived compare;
    everything else is stored as-is by default (the diff engine handles
    equality, not this coercion — coercion only normalises
    representation).

    ``value_coerce`` is an optional per-surface hook applied *first*.
    It exists because some ifcfast surfaces type a semantically-numeric
    column as a string (notably ``quantities.value``, which is ``str``
    even though every value is numeric). Passing ``numeric_or_passthrough``
    there lets the quantities adapter compare ``'3'`` against ``3.0`` as
    numbers, while psets/materials leave genuine strings untouched by
    *not* supplying the hook.
    """
    if value_coerce is not None:
        value = value_coerce(value)
    if _is_number(value):
        return float(value)
    return value


#: Per-surface leaf coercion hook (see :func:`_coerce`).
ValueCoerce = Callable[[Any], Any]


def numeric_or_passthrough(value: Any) -> Any:
    """Coerce a numeric-looking ``str`` to ``float``; pass everything else.

    Use as the ``value_coerce`` hook for surfaces whose value column is
    string-typed but semantically numeric (``quantities.value``). A
    non-numeric string (``"Concrete"``) is returned unchanged.
    """
    if isinstance(value, str):
        try:
            return float(value)
        except ValueError:
            return value
    return value


#: IFC value-type names whose underlying carrier is a real number. ifcfast's
#: ``psets.value_type`` column names the IFC measure/primitive; these map onto
#: a Python ``float`` so the string-typed ``value`` column compares against
#: ifcopenshell's native numeric. Boolean and integer are handled separately
#: (a bool is not a float here, an integer stays exact).
_FLOAT_VALUE_TYPES: frozenset[str] = frozenset(
    {
        "IfcReal",
        "IfcLengthMeasure",
        "IfcAreaMeasure",
        "IfcVolumeMeasure",
        "IfcPositiveLengthMeasure",
        "IfcPlaneAngleMeasure",
        "IfcRatioMeasure",
        "IfcNormalisedRatioMeasure",
        "IfcPositiveRatioMeasure",
        "IfcThermalTransmittanceMeasure",
        "IfcMassMeasure",
        "IfcMassDensityMeasure",
        "IfcCountMeasure",
        "IfcMonetaryMeasure",
        "IfcNumericMeasure",
    }
)


def coerce_by_value_type(value: Any, value_type: Any) -> Any:
    """Coerce an ifcfast string ``value`` to the type named by ``value_type``.

    ifcfast's ``m.psets`` carries a uniform-STRING ``value`` column plus a
    companion ``value_type`` column (``'IfcBoolean'`` / ``'IfcInteger'`` /
    ``'IfcReal'`` / ``'IfcLabel'`` / measure types …) — this is the
    documented contract, not a defect. To diff faithfully against
    ifcopenshell's *natively-typed* ``get_psets`` values, coerce the string
    back to the type its ``value_type`` names *before* comparison:

    - ``IfcBoolean``  : ``'True'``/``'False'`` -> ``bool``.
    - ``IfcLogical``  : ``'True'``/``'False'`` -> ``bool``; ``'Unknown'`` /
      ``None`` -> ``None`` (IFC three-valued logic; ifcopenshell yields
      ``None`` for UNKNOWN).
    - ``IfcInteger``  : -> ``int``.
    - real measures (:data:`_FLOAT_VALUE_TYPES`) : -> ``float``.
    - everything else (``IfcLabel``/``IfcText``/``IfcIdentifier``/unknown
      type) : returned as-is (a genuine string stays a string).

    This is *representation* canonicalisation — it changes typing, never
    value — so it cannot mask a real value divergence the way stringifying
    both sides (which hides ``'1'`` vs ``'1.0'`` vs ``True``) would. A value
    that cannot be parsed as its declared type is returned unchanged so the
    diff surfaces it loudly rather than swallowing it.
    """
    # Non-strings (already native, or a NaN/None) pass straight through.
    if not isinstance(value, str):
        return value

    vt = value_type if isinstance(value_type, str) else ""

    if vt == "IfcBoolean":
        if value == "True":
            return True
        if value == "False":
            return False
        return value  # unexpected -> surface it
    if vt == "IfcLogical":
        if value == "True":
            return True
        if value == "False":
            return False
        if value in ("Unknown", "UNKNOWN", "None", ""):
            return None
        return value
    if vt == "IfcInteger":
        try:
            return int(value)
        except ValueError:
            return value
    if vt in _FLOAT_VALUE_TYPES:
        try:
            return float(value)
        except ValueError:
            return value
    # IfcLabel / IfcText / IfcIdentifier / unrecognised type: keep the string.
    return value


@dataclass(frozen=True)
class NormalizedPayload:
    """Canonical, order-independent form of one name->value mapping.

    A single qto (e.g. ``Qto_WallBaseQuantities``) or pset becomes one
    of these. ``items`` is a frozenset of ``(name, coerced_value)`` so
    two payloads compare structurally regardless of dict order, after
    internal keys are dropped and numbers coerced to float.
    """

    items: frozenset[tuple[str, Any]]

    @classmethod
    def from_mapping(
        cls,
        mapping: Mapping[str, Any],
        *,
        internal: Iterable[str] = DEFAULT_INTERNAL_KEYS,
        value_coerce: ValueCoerce | None = None,
    ) -> "NormalizedPayload":
        cleaned = drop_internal_keys(mapping, internal=internal)
        return cls(
            frozenset(
                (k, _coerce(v, value_coerce)) for k, v in cleaned.items()
            )
        )

    def names(self) -> set[str]:
        return {k for k, _ in self.items}

    def as_dict(self) -> dict[str, Any]:
        return {k: v for k, v in self.items}


def normalize_payload(
    payload: Mapping[str, Any],
    *,
    internal: Iterable[str] = DEFAULT_INTERNAL_KEYS,
    value_coerce: ValueCoerce | None = None,
) -> NormalizedPayload:
    """Normalise one name->value mapping into a :class:`NormalizedPayload`."""
    return NormalizedPayload.from_mapping(
        payload, internal=internal, value_coerce=value_coerce
    )


def _row_get(row: Any, key: str) -> Any:
    """Read ``key`` from a mapping, a pandas namedtuple, or an object."""
    if isinstance(row, Mapping):
        return row[key]
    return getattr(row, key)


def normalize_grouped(
    rows: Iterable[Any],
    *,
    guid_key: str,
    group_key: str,
    name_key: str,
    value_key: str,
    internal: Iterable[str] = DEFAULT_INTERNAL_KEYS,
    value_coerce: ValueCoerce | None = None,
) -> dict[str, dict[str, NormalizedPayload]]:
    """Project a flat row stream onto ``{guid: {group: NormalizedPayload}}``.

    ``rows`` may be dicts, ``df.itertuples()`` namedtuples, or any
    attribute-bearing objects. Each row contributes one
    ``(name, value)`` pair under its ``(guid, group)`` bucket. The
    ``group_key`` dimension is the qto_name / pset_name — pass the
    column that names the container.

    ``value_coerce`` is an optional per-surface leaf hook (e.g.
    :func:`numeric_or_passthrough` for the string-typed
    ``quantities.value`` column).
    """
    nested: dict[str, dict[str, dict[str, Any]]] = {}
    for row in rows:
        guid = _row_get(row, guid_key)
        group = _row_get(row, group_key)
        name = _row_get(row, name_key)
        value = _row_get(row, value_key)
        nested.setdefault(guid, {}).setdefault(group, {})[name] = value

    return {
        guid: {
            group: normalize_payload(
                payload, internal=internal, value_coerce=value_coerce
            )
            for group, payload in groups.items()
        }
        for guid, groups in nested.items()
    }


def normalize_grouped_payloads(
    grouped: Mapping[str, Mapping[str, Mapping[str, Any]]],
    *,
    internal: Iterable[str] = DEFAULT_INTERNAL_KEYS,
    value_coerce: ValueCoerce | None = None,
) -> dict[str, dict[str, NormalizedPayload]]:
    """Normalise an already-nested ``{guid: {group: {name: value}}}`` map.

    Convenience for the ifcopenshell side, where ``get_psets`` already
    yields nested dicts (one level of which we drop internal keys from).
    """
    return {
        guid: {
            group: normalize_payload(
                payload, internal=internal, value_coerce=value_coerce
            )
            for group, payload in groups.items()
        }
        for guid, groups in grouped.items()
    }


# --- structured diff ---------------------------------------------------------

@dataclass
class GroupDiff:
    """One per-(guid, group) disagreement between two grouped surfaces.

    ``kind`` is a coarse machine label; the richer human classification
    lives on :class:`tests.oracle.report.DisagreementRecord`, which this
    feeds. ``kind`` is one of:

    - ``"missing_in_ours"``   — ifcopenshell has the (guid, group), we don't.
    - ``"missing_in_truth"``  — we surface a (guid, group) the oracle lacks.
    - ``"value_mismatch"``    — same keys, a value diverges past tolerance.
    - ``"key_mismatch"``      — the name sets differ within the group.
    """

    guid: str
    group: str
    kind: str
    detail: str
    ours: Any = None
    truth: Any = None
    diverging_keys: tuple[str, ...] = field(default_factory=tuple)


def _payload_diff(
    guid: str,
    group: str,
    ours: NormalizedPayload,
    truth: NormalizedPayload,
    *,
    rel_tol: float,
    abs_tol: float,
) -> GroupDiff | None:
    our_names = ours.names()
    truth_names = truth.names()
    if our_names != truth_names:
        return GroupDiff(
            guid=guid,
            group=group,
            kind="key_mismatch",
            detail=(
                f"name sets differ: only_ours={sorted(our_names - truth_names)} "
                f"only_truth={sorted(truth_names - our_names)}"
            ),
            ours=ours.as_dict(),
            truth=truth.as_dict(),
            diverging_keys=tuple(
                sorted(our_names ^ truth_names)
            ),
        )

    our_map = ours.as_dict()
    truth_map = truth.as_dict()
    diverging = tuple(
        sorted(
            k
            for k in our_names
            if not values_equal(
                our_map[k], truth_map[k], rel_tol=rel_tol, abs_tol=abs_tol
            )
        )
    )
    if diverging:
        return GroupDiff(
            guid=guid,
            group=group,
            kind="value_mismatch",
            detail="; ".join(
                f"{k}: ours={our_map[k]!r} truth={truth_map[k]!r}"
                for k in diverging
            ),
            ours=our_map,
            truth=truth_map,
            diverging_keys=diverging,
        )
    return None


def diff_grouped(
    ours: Mapping[str, Mapping[str, NormalizedPayload]],
    truth: Mapping[str, Mapping[str, NormalizedPayload]],
    *,
    rel_tol: float = DEFAULT_REL_TOL,
    abs_tol: float = DEFAULT_ABS_TOL,
) -> list[GroupDiff]:
    """Diff two normalized ``{guid: {group: NormalizedPayload}}`` maps.

    Returns a list of every disagreement found — empty list means the
    two surfaces fully agree. Collect-all (never first-failure) so a
    single CI run reports the whole divergence surface.
    """
    diffs: list[GroupDiff] = []

    all_guids = set(ours) | set(truth)
    for guid in sorted(all_guids, key=str):
        our_groups = ours.get(guid, {})
        truth_groups = truth.get(guid, {})
        all_groups = set(our_groups) | set(truth_groups)
        for group in sorted(all_groups, key=str):
            if group not in our_groups:
                diffs.append(
                    GroupDiff(
                        guid=guid,
                        group=group,
                        kind="missing_in_ours",
                        detail="ifcopenshell has this group; ifcfast does not",
                        truth=truth_groups[group].as_dict(),
                    )
                )
                continue
            if group not in truth_groups:
                diffs.append(
                    GroupDiff(
                        guid=guid,
                        group=group,
                        kind="missing_in_truth",
                        detail="ifcfast surfaces this group; ifcopenshell does not",
                        ours=our_groups[group].as_dict(),
                    )
                )
                continue
            d = _payload_diff(
                guid,
                group,
                our_groups[group],
                truth_groups[group],
                rel_tol=rel_tol,
                abs_tol=abs_tol,
            )
            if d is not None:
                diffs.append(d)

    return diffs
