"""IDS parse differential: IfcTester's parsed IDS vs ifcfast's IR, as canonical JSON.

Design: docs/plans/2026-09-24_ids-validation-design.md §4 ("Parse differential").
Every ``.ids`` in the pinned buildingSMART suite is parsed by both engines. Each
engine emits the canonical JSON below and the two are compared for equality, so our
XML semantics cannot drift away from IfcTester without a test going red. The ifcfast
side (``to_canonical_json()`` on the Rust IR) is a documented skip until the native
engine lands.

Canonical JSON schema (v1)
--------------------------
Serialise with ``json.dumps(obj, sort_keys=True, separators=(",", ":"),
ensure_ascii=False)``. Every key listed is always present; absent values are ``null``.
Strings are verbatim: no case folding and no trimming. The engines must agree on
exactly what was written in the IDS, and invalid-IDS detection depends on that.

::

    {
      "schema": "ifcfast.ids.canonical/1",
      "info": {"title": str|null, "copyright": str|null, "version": str|null,
               "description": str|null, "author": str|null, "date": str|null,
               "purpose": str|null, "milestone": str|null},
      "specifications": [                       # document order (index = spec_index)
        {
          "name": str,
          "identifier": str|null,
          "description": str|null,
          "instructions": str|null,
          "ifcVersion": [str, ...],             # sorted, verbatim tokens
          "minOccurs": int,                     # applicability/@minOccurs, default 0
          "maxOccurs": int|"unbounded",         # applicability/@maxOccurs, default "unbounded"
          "cardinality": "required"|"optional"|"prohibited",
                                                # minOccurs>0 -> required;
                                                # maxOccurs==0 -> prohibited; else optional
          "applicability": [Facet, ...],
          "requirements":  [Facet, ...]
        }
      ]
    }

    Facet = {
      "facet": "entity"|"partOf"|"classification"|"attribute"|"property"|"material",
      "cardinality": "required"|"optional"|"prohibited"|null,
                          # null in applicability (the XSD has no cardinality there);
                          # in requirements defaults to "required" (entity: always "required")
      "instructions": str|null,
      "fields": {...}     # per facet type, below; every key present
    }

    fields by facet type (IdsValue fields marked *):
      entity:         name*, predefinedType*
      partOf:         name*, predefinedType*   (the nested <entity>), relation: str|null
      classification: value*, system*, uri: str|null
      attribute:      name*, value*
      property:       propertySet*, baseName*, value*, dataType: str|null, uri: str|null
      material:       value*, uri: str|null

    IdsValue = null
             | {"simpleValue": str}
             | {"restriction": {
                  "base": str,                         # without the "xs:" prefix, e.g. "string", "double"
                  "enumeration": [str, ...],           # sorted, present only if used
                  "pattern": [str, ...],               # sorted, present only if used (patterns are ORed)
                  "length"|"minLength"|"maxLength"|
                  "minInclusive"|"maxInclusive"|
                  "minExclusive"|"maxExclusive"|
                  "totalDigits"|"fractionDigits": str  # present only if used; lexical form
               }}

Facet order: the IDS XSD fixes the element order inside ``applicability`` and
``requirements`` (entity, partOf, classification, attribute, property, material),
so document order equals that type order and is stable within a type. IfcTester
regroups facets by type (``ifctester/ids.py:256-267``), which is why the canonical
order is defined by facet type first and document order second. The index into
``requirements`` is the ``requirement_index`` of the ``failures`` table (design §3.2).

Normalisation applied on the IfcTester side, which the Rust side must reproduce:
- ``Restriction.base`` has the ``xs:`` prefix stripped already (``facet.py:1010``).
- IfcTester gives applicability facets ``cardinality="required"`` (``facet.py:105``),
  and the canonical form writes ``null`` there instead.
- A single ``xs:enumeration`` or ``xs:pattern`` is still emitted as a list.
- Integer restriction bounds (``length`` etc. come back as ``int``) are written as
  ``str(int)``.

Usage::

    python -m tests.oracle.ids_parse_differential [--dump DIR]   # IfcTester side only, for now
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

import pytest

from .ids_conformance import Case, cases_root, iter_cases

SCHEMA_ID = "ifcfast.ids.canonical/1"
FACET_ORDER = ("entity", "partOf", "classification", "attribute", "property", "material")
IDS_VALUE_FIELDS = {
    "entity": ("name", "predefinedType"),
    "partOf": ("name", "predefinedType"),
    "classification": ("value", "system"),
    "attribute": ("name", "value"),
    "property": ("propertySet", "baseName", "value"),
    "material": ("value",),
}
PLAIN_FIELDS = {
    "entity": (),
    "partOf": ("relation",),
    "classification": ("uri",),
    "attribute": (),
    "property": ("dataType", "uri"),
    "material": ("uri",),
}
_LIST_CONSTRAINTS = ("enumeration", "pattern")
_SCALAR_CONSTRAINTS = (
    "length", "minLength", "maxLength", "minInclusive", "maxInclusive",
    "minExclusive", "maxExclusive", "totalDigits", "fractionDigits",
)
_INFO_KEYS = ("title", "copyright", "version", "description", "author", "date", "purpose", "milestone")


def canonical_dumps(obj: Any) -> str:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


# --------------------------------------------------------------------------- #
# IfcTester -> canonical
# --------------------------------------------------------------------------- #
def _ids_value(v: Any) -> Any:
    from ifctester.facet import Restriction

    if v is None:
        return None
    if isinstance(v, Restriction):
        r: dict[str, Any] = {"base": str(v.base)}
        for k, val in v.options.items():
            if k in _LIST_CONSTRAINTS:
                vals = val if isinstance(val, list) else [val]
                r[k] = sorted(str(x) for x in vals)
            elif k in _SCALAR_CONSTRAINTS:
                if isinstance(val, list):
                    if len(val) != 1:
                        raise ValueError(f"restriction {k} has {len(val)} values: {val!r}")
                    val = val[0]
                r[k] = str(val)
            else:
                raise ValueError(f"unknown restriction constraint {k!r}")
        return {"restriction": r}
    if isinstance(v, str):
        return {"simpleValue": v}
    raise TypeError(f"unexpected IDS value type {type(v).__name__}: {v!r}")


def _opt_str(v: Any) -> str | None:
    if v is None or v == "":
        return None
    if not isinstance(v, str):
        raise TypeError(f"expected str, got {type(v).__name__}: {v!r}")
    return v


def _facet(f: Any, clause: str) -> dict[str, Any]:
    kind = type(f).__name__
    kind = kind[0].lower() + kind[1:]
    if kind not in FACET_ORDER:
        raise ValueError(f"unknown facet type {kind!r}")
    fields: dict[str, Any] = {k: _ids_value(getattr(f, k)) for k in IDS_VALUE_FIELDS[kind]}
    fields.update({k: _opt_str(getattr(f, k)) for k in PLAIN_FIELDS[kind]})
    if clause == "applicability":
        card = None
    elif kind == "entity":
        card = "required"
    else:
        card = getattr(f, "cardinality", None) or "required"
    return {
        "facet": kind,
        "cardinality": card,
        "instructions": _opt_str(getattr(f, "instructions", None)),
        "fields": fields,
    }


def _clause(facets: list, clause: str) -> list[dict[str, Any]]:
    out = [_facet(f, clause) for f in facets]
    # stable sort: facet-type order, document order within a type
    return sorted(out, key=lambda d: FACET_ORDER.index(d["facet"]))


def _max_occurs(v: Any) -> int | str:
    if v == "unbounded":
        return "unbounded"
    return int(v)


def _cardinality(min_occurs: int, max_occurs: int | str) -> str:
    if min_occurs > 0:
        return "required"
    if max_occurs == 0:
        return "prohibited"
    return "optional"


def ifctester_canonical(ids_path: Path) -> dict[str, Any]:
    """Parse ``ids_path`` with IfcTester and return the canonical dict (schema v1)."""
    from ifctester import ids as ids_mod

    doc = ids_mod.open(str(ids_path), validate=True)
    specs = []
    for s in doc.specifications:
        mn = int(s.minOccurs)
        mx = _max_occurs(s.maxOccurs)
        versions = s.ifcVersion if isinstance(s.ifcVersion, list) else str(s.ifcVersion).split()
        specs.append({
            "name": s.name,
            "identifier": _opt_str(s.identifier),
            "description": _opt_str(s.description),
            "instructions": _opt_str(s.instructions),
            "ifcVersion": sorted(versions),
            "minOccurs": mn,
            "maxOccurs": mx,
            "cardinality": _cardinality(mn, mx),
            "applicability": _clause(s.applicability, "applicability"),
            "requirements": _clause(s.requirements, "requirements"),
        })
    info = {k: _opt_str(doc.info.get(k)) for k in _INFO_KEYS}
    return {"schema": SCHEMA_ID, "info": info, "specifications": specs}


# --------------------------------------------------------------------------- #
# ifcfast -> canonical (`ifcfast._ids_canonical_json`)
# --------------------------------------------------------------------------- #
def ifcfast_canonical(ids_path: Path) -> dict[str, Any]:
    """ifcfast's canonical IR for ``ids_path``.

    Contract for the Rust side: the IR's ``to_canonical_json()`` returns the
    schema-v1 document above, exposed to Python. The exact binding name is not
    fixed yet; until it exists this raises ``NotImplementedError`` and the
    differential test skips.
    """
    import ifcfast

    fn = getattr(ifcfast, "_ids_canonical_json", None)
    if fn is None or not hasattr(ifcfast._core, "_ids_canonical_json"):
        raise NotImplementedError("ifcfast IDS IR canonical JSON binding not built into this wheel")
    return json.loads(fn(str(ids_path)))


# --------------------------------------------------------------------------- #
# pytest
# --------------------------------------------------------------------------- #
def _params() -> list:
    try:
        cases = iter_cases(cases_root())
    except FileNotFoundError as e:
        return [pytest.param(None, id="suite-missing", marks=pytest.mark.skip(reason=str(e)))]
    return [pytest.param(c, id=c.id) for c in cases]


@pytest.mark.parametrize("case", _params())
def test_ids_parse_differential(case: Case):
    pytest.importorskip("ifctester", reason="IfcTester oracle missing: pip install -e '.[dev]'")
    from ifctester.ids import IdsXmlValidationError

    try:
        ours_ref = ifctester_canonical(case.ids_path)
    except IdsXmlValidationError:
        pytest.skip("IDS fails the XSD in IfcTester; there is no IR to compare")
    canonical_dumps(ours_ref)  # must serialise
    import ifcfast

    try:
        fast = ifcfast_canonical(case.ids_path)
    except NotImplementedError as e:
        pytest.skip(str(e))
    except ifcfast.IdsInvalidError as e:
        # ifcfast's parse applies the schema-free audit (ids/audit.rs), which
        # refuses some `invalid-` IDS that pass IfcTester's XSD-only decode.
        # There is no ifcfast IR to compare; any other case refused is a bug.
        if case.expected == "invalid":
            pytest.skip(f"ifcfast audit rejects this invalid- IDS: {e}")
        raise
    assert canonical_dumps(fast) == canonical_dumps(ours_ref)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Dump IfcTester canonical IDS JSON for the pinned suite")
    ap.add_argument("--dump", type=Path, help="write <folder>/<stem>.json per case")
    ap.add_argument("--folder", action="append")
    a = ap.parse_args(argv)
    cases = iter_cases(cases_root())
    if a.folder:
        cases = [c for c in cases if c.folder in set(a.folder)]
    from ifctester.ids import IdsXmlValidationError

    n_ok = n_xsd = 0
    for c in cases:
        try:
            doc = ifctester_canonical(c.ids_path)
        except IdsXmlValidationError:
            n_xsd += 1
            continue
        n_ok += 1
        if a.dump:
            out = a.dump / c.folder / f"{c.stem}.json"
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(json.dumps(doc, sort_keys=True, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"canonicalised {n_ok} IDS files, {n_xsd} rejected by XSD")
    return 0


if __name__ == "__main__":
    sys.exit(main())
