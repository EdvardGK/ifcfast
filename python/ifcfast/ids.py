"""Native IDS 1.0 validation (buildingSMART Information Delivery Specification).

Design: ``docs/plans/2026-09-24_ids-validation-design.md`` (GH #192).

IfcTester is the reference implementation; ifcfast is the speed-first
companion (the same relationship ``m.mesh_qto()`` has to ifcopenshell
geometry). Slices 1-3 implement all six facets: **Entity**,
**Attribute**, **Property**, **Classification**, **Material** and
**PartOf**. A construct the engine does not implement (some XSD regex
blocks) raises :class:`IdsUnsupportedError` (``on_unsupported="raise"``,
the default) or marks that specification ``status="unsupported"``
(``on_unsupported="mark"``). A value comparison
that needs a unit the model does not declare raises :class:`IdsUnitError`
(or, under ``"mark"``, marks the spec ``unsupported_feature="unit:<TYPE>"``).
Nothing is ever guessed.

Usage::

    import ifcfast
    rep = ifcfast.validate_ids("spec.ids", "model.ifc")
    rep.ok                       # every spec passed (or was version-skipped)
    rep.specs                    # one row per specification
    rep.failures                 # one row per spec x element x failing requirement
    m = ifcfast.open("model.ifc")
    rep = m.validate_ids(["a.ids", "b.ids"])   # one EntityTable, several IDS files
    rep.to_ifctester_json(ids_index=0)         # IfcTester reporter.Json shape
"""

from __future__ import annotations

import datetime
import json
import os
from pathlib import Path
from typing import NamedTuple, Sequence, Union

try:  # the native module carries the typed exceptions (subclasses of IfcfastError)
    from ._core import IdsInvalidError, IdsUnitError, IdsUnsupportedError  # type: ignore[attr-defined]
except ImportError:  # pragma: no cover - native module built without the `ids` feature
    try:
        from ._core import IfcfastError as _Base  # type: ignore[attr-defined]
    except ImportError:
        class _Base(Exception):  # type: ignore[no-redef]
            pass

    class IdsInvalidError(_Base):  # type: ignore[no-redef]
        """The IDS is malformed or violates IDS 1.0 (``.path``, ``.line``)."""

    class IdsUnsupportedError(_Base):  # type: ignore[no-redef]
        """A valid IDS construct ifcfast does not implement (``.feature``, ``.spec_index``)."""

    class IdsUnitError(_Base):  # type: ignore[no-redef]
        """A requirement needs a unit the model does not declare (``.unit_type``)."""


__all__ = [
    "IdsInvalidError",
    "IdsReport",
    "IdsUnitError",
    "IdsUnsupportedError",
    "validate_ids",
]

IdsInput = Union[str, bytes, bytearray, os.PathLike, Sequence[Union[str, bytes, bytearray, os.PathLike]]]

#: Category vocabularies (design §3.2). Fixed so empty reports and
#: concatenated reports share dtypes.
SPEC_CARDINALITY = ["required", "optional", "prohibited"]
SPEC_STATUS = ["pass", "fail", "skipped_ifc_version", "unsupported"]
SPEC_REASON = ["SPEC_NO_APPLICABLE", "SPEC_PROHIBITED_APPLICABLE"]
ELEMENT_STATUS = ["pass", "fail"]
FACET_TYPE = ["entity", "part_of", "classification", "attribute", "property", "material"]
FACET_CARDINALITY = ["required", "optional", "prohibited"]
REASON_CODES = [
    "ENTITY_MISMATCH", "PREDEFINED_MISMATCH", "ATTR_MISSING", "ATTR_VALUE_MISMATCH",
    "PSET_MISSING", "PROP_MISSING", "PROP_NULL", "PROP_UNSUPPORTED", "PROP_DATATYPE_MISMATCH",
    "PROP_VALUE_MISMATCH",
    "CLASS_MISSING", "CLASS_SYSTEM_MISMATCH", "CLASS_VALUE_MISMATCH", "MATERIAL_MISSING",
    "MATERIAL_VALUE_MISMATCH", "PARTOF_MISSING", "PARTOF_ENTITY_MISMATCH", "PROHIBITED_PRESENT",
    "SPEC_NO_APPLICABLE", "SPEC_PROHIBITED_APPLICABLE",
]
VALUE_SOURCE = ["instance", "type"]


class _IdsFrames(NamedTuple):
    specs: "object"
    elements: "object"
    failures: "object"


class IdsReport(_IdsFrames):
    """Result of :func:`validate_ids`: three long-format DataFrames.

    * ``specs`` — one row per specification (``spec_index`` runs across
      every IDS document of the call; ``ids_index`` names the document).
    * ``elements`` — one row per spec x applicable element.
    * ``failures`` — one row per spec x element x failing requirement.

    Join ``elements`` / ``failures`` to ``specs`` on ``spec_index``.

    Still a 3-tuple (``specs, elements, failures = rep`` works); it also
    remembers its inputs so :meth:`to_ifctester_json` can build the
    IfcTester-shaped report on demand.
    """

    # No __slots__: instances carry a __dict__ for the validation inputs
    # (`_inputs`) and a JSON computed in the same native pass (`_json`).

    @property
    def ok(self) -> bool:
        """Every specification passed or was skipped for its ifcVersion.

        An ``unsupported`` specification (``on_unsupported="mark"``) is NOT
        ok: nothing was checked for it.
        """
        return bool(self.specs["status"].isin(["pass", "skipped_ifc_version"]).all())

    def to_parquet(self, directory: Union[str, os.PathLike]) -> Path:
        """Write ``specs.parquet``, ``elements.parquet`` and
        ``failures.parquet`` into ``directory`` (created if missing)."""
        d = Path(directory)
        d.mkdir(parents=True, exist_ok=True)
        self.specs.to_parquet(d / "specs.parquet", index=False)
        self.elements.to_parquet(d / "elements.parquet", index=False)
        self.failures.to_parquet(d / "failures.parquet", index=False)
        return d

    def to_ifctester_json(self, ids_index: Union[int, None] = None) -> dict:
        """The report in IfcTester's ``reporter.Json`` shape (GH #192 slice 4).

        Same structure IfcTester 0.8.5 emits (``Json(ids).report()``):
        ``title`` / ``date`` / ``filepath`` / ``filename`` / totals, and
        ``specifications`` -> ``requirements`` -> ``passed_entities`` /
        ``failed_entities``, each failed entity with IfcTester's ``reason``
        sentence word for word — a tool that reads IfcTester JSON reads
        this. Built by the native core (the same code as the browser's
        ``IfcModel.validateIds``). Gated against IfcTester on the IDS
        suite by ``tests/oracle/ids_json_parity.py``.

        Differences: ``element`` / ``element_type`` are the source STEP
        record ``#id=IfcClass(args as written)`` (IfcTester prints
        ifcopenshell's re-serialisation, same id and class); extra keys
        are prefixed ``ifcfast_`` (``ifcfast_status`` keeps
        ``unsupported`` / ``skipped_ifc_version`` apart, failed entities
        carry ``ifcfast_reason_code``); entity lists are in step-id order.

        One IfcTester report covers one IDS document. ``ids_index`` picks
        the document when the report came from several (``None`` is
        allowed only for a single-document report). The native pass is
        re-run with the same inputs unless the report already carries the
        JSON (the CLI builds both in one pass); ``date`` is the time the
        JSON was built.
        """
        texts = self.__dict__.get("_json")
        if texts is None:
            inputs = self.__dict__.get("_inputs")
            if inputs is None:
                raise ValueError(
                    "to_ifctester_json() needs a report returned by validate_ids(); "
                    "this IdsReport was constructed by hand"
                )
            texts = _run(*inputs, want_json=True).__dict__["_json"]
            self.__dict__["_json"] = texts
        n = len(texts)
        if ids_index is None:
            if n != 1:
                raise ValueError(
                    f"this report covers {n} IDS documents; pass ids_index= (0..{n - 1}) — "
                    "one IfcTester report per IDS document"
                )
            ids_index = 0
        if not 0 <= int(ids_index) < n:
            raise IndexError(f"ids_index {ids_index} out of range for {n} IDS document(s)")
        return json.loads(texts[int(ids_index)])


def _ids_bytes(item) -> bytes:
    if isinstance(item, (bytes, bytearray, memoryview)):
        return bytes(item)
    if isinstance(item, os.PathLike):
        return Path(item).read_bytes()
    if isinstance(item, str):
        head = item.lstrip("﻿ \t\r\n")
        if head.startswith("<"):
            return item.encode("utf-8")
        p = Path(item)
        if not p.is_file():
            raise FileNotFoundError(f"IDS not found: {item!r} (pass a path, an XML string or bytes)")
        return p.read_bytes()
    raise TypeError(f"IDS must be a path, an XML string or bytes, got {type(item).__name__}")


def _ids_list(ids: IdsInput) -> list[bytes]:
    if isinstance(ids, (list, tuple)):
        out = [_ids_bytes(x) for x in ids]
    else:
        out = [_ids_bytes(ids)]
    if not out:
        raise ValueError("validate_ids: no IDS documents given")
    return out


def _ifc_arg(ifc):
    if isinstance(ifc, (bytes, bytearray, memoryview)):
        return bytes(ifc)
    if isinstance(ifc, (str, os.PathLike)):
        from .header import native_path_for

        return str(native_path_for(ifc))
    raise TypeError(f"ifc must be a path or bytes, got {type(ifc).__name__}")


def _frames(raw: dict) -> IdsReport:
    import pandas as pd

    from .classify import canonical_entity_any_schema

    def cat(values, cats):
        return pd.Categorical(values, categories=cats)

    s = raw["specs"]
    specs = pd.DataFrame({
        "ids_index": pd.array(s["ids_index"], dtype="int32"),
        "spec_index": pd.array(s["spec_index"], dtype="int32"),
        "name": pd.array(s["name"], dtype="string"),
        "identifier": pd.array(s["identifier"], dtype="string"),
        "description": pd.array(s["description"], dtype="string"),
        "instructions": pd.array(s["instructions"], dtype="string"),
        "ifc_versions": pd.array(s["ifc_versions"], dtype="string"),
        "cardinality": cat(s["cardinality"], SPEC_CARDINALITY),
        "status": cat(s["status"], SPEC_STATUS),
        "reason_code": cat(s["reason_code"], SPEC_REASON),
        "unsupported_feature": pd.array(s["unsupported_feature"], dtype="string"),
        "applicable": pd.array(s["applicable"], dtype="int64"),
        "passed": pd.array(s["passed"], dtype="int64"),
        "failed": pd.array(s["failed"], dtype="int64"),
        "applicability_label": pd.array(s["applicability_label"], dtype="string"),
        "requirement_labels": pd.Series(s["requirement_labels"], dtype="object"),
    })

    e = raw["elements"]
    elements = pd.DataFrame({
        "spec_index": pd.array(e["spec_index"], dtype="int32"),
        "step_id": pd.array(e["step_id"], dtype="int64"),
        "guid": pd.array(e["guid"], dtype="string"),
        "entity": pd.array([canonical_entity_any_schema(x) for x in e["entity"]], dtype="string"),
        "predefined_type": pd.array(e["predefined_type"], dtype="string"),
        "name": pd.array(e["name"], dtype="string"),
        "description": pd.array(e["description"], dtype="string"),
        "tag": pd.array(e["tag"], dtype="string"),
        "type_step_id": pd.array(e["type_step_id"], dtype="Int64"),
        "status": cat(e["status"], ELEMENT_STATUS),
        "n_failed": pd.array(e["n_failed"], dtype="int16"),
    })

    f = raw["failures"]
    failures = pd.DataFrame({
        "spec_index": pd.array(f["spec_index"], dtype="int32"),
        "step_id": pd.array(f["step_id"], dtype="int64"),
        "guid": pd.array(f["guid"], dtype="string"),
        "requirement_index": pd.array(f["requirement_index"], dtype="int16"),
        "facet_type": cat(f["facet_type"], FACET_TYPE),
        "facet_cardinality": cat(f["facet_cardinality"], FACET_CARDINALITY),
        "reason_code": cat(f["reason_code"], REASON_CODES),
        "expected": pd.array(f["expected"], dtype="string"),
        "actual": pd.array(f["actual"], dtype="string"),
        "value_source": cat(f["value_source"], VALUE_SOURCE),
    })
    return IdsReport(specs, elements, failures)


def validate_ids(
    ids: IdsInput,
    ifc,
    *,
    on_unsupported: str = "raise",
    filter_ifc_version: bool = False,
) -> IdsReport:
    """Validate an IFC against one or more IDS 1.0 documents.

    Args:
        ids: an IDS as a path, an XML string, ``bytes``, or a list of those.
            A list is validated over ONE parse of the IFC.
        ifc: the IFC as a path (``.ifc`` / ``.ifczip``) or its ``bytes``.
        on_unsupported: ``"raise"`` (default) raises
            :class:`IdsUnsupportedError` for a spec using a facet or construct
            ifcfast does not implement yet; ``"mark"`` reports that spec with
            ``status="unsupported"`` and ``unsupported_feature`` set, and no
            element rows. A value comparison needing an undeclared unit
            follows the same switch (``IdsUnitError`` vs
            ``unsupported_feature="unit:<UNITTYPE>"``).
        filter_ifc_version: skip specs whose ``ifcVersion`` excludes the
            file's schema (``status="skipped_ifc_version"``). Off by default,
            like IfcTester, which validates every spec regardless.

    Raises:
        IdsInvalidError: malformed IDS, or an IDS that can never be satisfied
            for this schema (unknown entity/attribute, value of the wrong type).
        IdsUnsupportedError: see ``on_unsupported``.
        IdsUnitError: a value comparison needs a unit the model does not
            declare (``on_unsupported="raise"``).
        IfcfastError: the IFC is truncated or declares an unsupported schema.
    """
    return _run(_ifc_arg(ifc), _ids_list(ids), str(on_unsupported), bool(filter_ifc_version),
                _json_filepath(ifc), want_json=False)


def _json_filepath(ifc) -> Union[str, None]:
    """IfcTester's ``filepath``: the IFC path as given; ``None`` for bytes."""
    if isinstance(ifc, (str, os.PathLike)):
        return os.fspath(ifc)
    return None


def _run(ifc_arg, ids_list, on_unsupported, filter_ifc_version, json_filepath, *, want_json):
    """One native pass -> IdsReport; ``want_json`` also builds the
    IfcTester JSON of every document in the same pass."""
    native = _native("validate_ids")
    kw = {}
    if want_json:
        kw = {
            "ifctester_json": True,
            "json_date": datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
            "json_filepath": json_filepath,
        }
    raw = native(ifc_arg, ids_list, on_unsupported, filter_ifc_version, **kw)
    rep = _frames(raw)
    rep.__dict__["_inputs"] = (ifc_arg, ids_list, on_unsupported, filter_ifc_version, json_filepath)
    if want_json:
        rep.__dict__["_json"] = list(raw["ifctester_json"])
    return rep


def _validate_with_json(ids: IdsInput, ifc, *, on_unsupported: str = "raise",
                        filter_ifc_version: bool = False) -> IdsReport:
    """:func:`validate_ids` that also builds the IfcTester JSON in the
    same native pass (the CLI's ``--json``; internal)."""
    return _run(_ifc_arg(ifc), _ids_list(ids), str(on_unsupported), bool(filter_ifc_version),
                _json_filepath(ifc), want_json=True)


def _native(name: str):
    """The native entry point, or a loud error when the extension was
    built without the ``ids`` feature (never a silent fallback)."""
    from . import _core

    fn = getattr(_core, name, None)
    if fn is None:
        base = getattr(_core, "IfcfastError", RuntimeError)
        raise base(
            f"ifcfast native module has no {name}: it was built without the `ids` "
            "feature or predates it — rebuild/upgrade ifcfast"
        )
    return fn


def _ids_canonical_json(ids) -> str:
    """Canonical IR JSON of one IDS (parse differential vs IfcTester; internal)."""
    return _native("_ids_canonical_json")(_ids_bytes(ids))
