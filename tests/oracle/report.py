"""Structured disagreement reporting for the oracle suite (GH #59 M1).

A differential test that stops at the first mismatch tells you almost
nothing about the *shape* of the divergence. The oracle instead
collects every disagreement a sweep finds into a list of typed
:class:`DisagreementRecord`s, each carrying a human-meaningful
:class:`Classification`. CI fails on any record not explicitly tagged
as benign (``expected_drift`` / ``ifcopenshell_quirk``), but the full
list is always available for triage.

Public API
----------
``Classification`` — enum:
    ``expected_drift`` | ``ifcfast_bug`` | ``ifcopenshell_quirk`` |
    ``tolerance`` | ``schema_drift`` | ``unknown``.

``DisagreementRecord`` — frozen dataclass:
    fields ``surface``, ``fixture``, ``guid``, ``group``, ``kind``,
    ``detail``, ``classification`` (default ``unknown``),
    ``ours``, ``truth``, ``diverging_keys``.

``DisagreementRecord.from_group_diff(diff, *, surface, fixture,``
``    classification=Classification.unknown) -> DisagreementRecord``
    Adapter from :class:`tests.oracle.normalize.GroupDiff`.

``Collector`` — accumulates records across surfaces/fixtures:
    ``.record(rec)`` / ``.extend_from_diffs(diffs, *, surface, fixture,``
    ``classify=None)`` / ``.records`` / ``.blocking()`` /
    ``.is_clean()`` / ``.summary()`` / ``.assert_clean()``.

``BENIGN`` — frozenset of classifications that do NOT fail CI.

Usage
-----
>>> from tests.oracle.report import Collector, Classification
>>> col = Collector()
>>> col.extend_from_diffs(diffs, surface="quantities", fixture="quantities.ifc")
>>> col.assert_clean()   # raises AssertionError listing blocking records
"""

from __future__ import annotations

import enum
from dataclasses import dataclass, field
from typing import Any, Callable, Iterable

from .normalize import GroupDiff


class Classification(enum.Enum):
    """Triage label for a single disagreement.

    - ``expected_drift``     — known, accepted difference (benign).
    - ``ifcfast_bug``        — our extractor is wrong (blocking).
    - ``ifcopenshell_quirk`` — the oracle is wrong / quirky (benign).
    - ``tolerance``          — within-spirit but past current tol (blocking
                               until tol is widened deliberately).
    - ``schema_drift``       — IFC schema-version shape change (blocking;
                               needs a deliberate mapping update).
    - ``unknown``            — not yet triaged (blocking by default).
    """

    expected_drift = "expected_drift"
    ifcfast_bug = "ifcfast_bug"
    ifcopenshell_quirk = "ifcopenshell_quirk"
    tolerance = "tolerance"
    schema_drift = "schema_drift"
    unknown = "unknown"


#: Classifications that are accepted and do NOT fail CI.
BENIGN: frozenset[Classification] = frozenset(
    {Classification.expected_drift, Classification.ifcopenshell_quirk}
)


@dataclass(frozen=True)
class DisagreementRecord:
    """One classified disagreement between ifcfast and the oracle.

    ``surface`` names the agent-visible surface under test
    (``"quantities"`` / ``"psets"`` / ``"materials"`` / ``"geometry"``).
    ``fixture`` is the tiny IFC file it came from. ``kind`` is the
    coarse machine label carried over from
    :class:`tests.oracle.normalize.GroupDiff`.
    """

    surface: str
    fixture: str
    guid: str
    group: str
    kind: str
    detail: str
    classification: Classification = Classification.unknown
    ours: Any = None
    truth: Any = None
    diverging_keys: tuple[str, ...] = field(default_factory=tuple)

    @classmethod
    def from_group_diff(
        cls,
        diff: GroupDiff,
        *,
        surface: str,
        fixture: str,
        classification: Classification = Classification.unknown,
    ) -> "DisagreementRecord":
        return cls(
            surface=surface,
            fixture=fixture,
            guid=diff.guid,
            group=diff.group,
            kind=diff.kind,
            detail=diff.detail,
            classification=classification,
            ours=diff.ours,
            truth=diff.truth,
            diverging_keys=tuple(diff.diverging_keys),
        )

    @property
    def is_blocking(self) -> bool:
        return self.classification not in BENIGN

    def __str__(self) -> str:  # pragma: no cover - formatting only
        return (
            f"[{self.classification.value}] {self.surface}/{self.fixture} "
            f"{self.kind} guid={self.guid} group={self.group}: {self.detail}"
        )


#: A classifier maps a raw GroupDiff to a Classification (e.g. to tag a
#: known ifcopenshell quirk as benign). Return None to leave it ``unknown``.
Classifier = Callable[[GroupDiff], Classification | None]


@dataclass
class Collector:
    """Accumulates :class:`DisagreementRecord`s across a sweep."""

    records: list[DisagreementRecord] = field(default_factory=list)

    def record(self, rec: DisagreementRecord) -> None:
        self.records.append(rec)

    def extend_from_diffs(
        self,
        diffs: Iterable[GroupDiff],
        *,
        surface: str,
        fixture: str,
        classify: Classifier | None = None,
    ) -> None:
        """Convert and absorb a batch of :class:`GroupDiff`s.

        ``classify`` optionally tags each diff (e.g. mark a known
        ifcopenshell quirk benign); diffs it returns ``None`` for stay
        ``unknown`` (hence blocking).
        """
        for d in diffs:
            cls = classify(d) if classify is not None else None
            self.records.append(
                DisagreementRecord.from_group_diff(
                    d,
                    surface=surface,
                    fixture=fixture,
                    classification=cls or Classification.unknown,
                )
            )

    def blocking(self) -> list[DisagreementRecord]:
        """Records whose classification fails CI."""
        return [r for r in self.records if r.is_blocking]

    def is_clean(self) -> bool:
        """True if no blocking records were collected."""
        return not self.blocking()

    def summary(self) -> dict[str, int]:
        """Count of records by classification label."""
        out: dict[str, int] = {c.value: 0 for c in Classification}
        for r in self.records:
            out[r.classification.value] += 1
        return out

    def assert_clean(self) -> None:
        """Raise ``AssertionError`` listing every blocking record."""
        blocking = self.blocking()
        if blocking:
            lines = "\n".join(f"  - {r}" for r in blocking)
            raise AssertionError(
                f"{len(blocking)} blocking oracle disagreement(s):\n{lines}\n"
                f"summary={self.summary()}"
            )
