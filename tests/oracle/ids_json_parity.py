"""IfcTester JSON parity: ``IdsReport.to_ifctester_json()`` vs IfcTester's
``reporter.Json`` over the buildingSMART IDS suite (GH #192 slice 4).

Same suite, same case discovery and the same filename-truth labels as
``tests/oracle/ids_conformance.py``. For every case BOTH engines label
green (both agree with truth, both produced a report — an ``invalid-`` case
ifcfast rejects outright as ``IdsInvalidError`` has no report to compare),
the two JSON documents are normalised and compared field by field.

Normalisation — only what legitimately differs:

- ``date``, ``filepath``, ``filename`` (run-dependent), and every key
  prefixed ``ifcfast_`` (ours, additive).
- ``element`` / ``element_type``: IfcTester prints
  ``str(entity_instance)`` (ifcopenshell's re-serialisation), ifcfast the
  source record. Both reduce to ``#<id>=<IfcClass>``; the arguments are not
  compared.
- Entity lists are sorted by ``id`` (``passed_entities`` is a Python set in
  IfcTester, no stable order).
- A Python set / list literal quoted inside a ``reason`` sentence is
  compared as a sorted multiset where IfcTester builds it from a set
  (material names, classification references and systems): hash order.

Everything else — spec and requirement statuses, totals, percents,
labels, values, descriptions, metadata, per-entity class / predefined type /
name / description / GlobalId / tag and every reason sentence — must be
equal.

CLI::

    python -m tests.oracle.ids_json_parity [--folder property] [--json out.json] [-v]
"""

from __future__ import annotations

import argparse
import ast
import json
import re
import sys
from pathlib import Path
from typing import Any

import pytest

from .ids_conformance import (
    Case,
    IfcfastUnavailable,
    Label,
    Outcome,
    _err,
    _ifcfast_api,
    cases_root,
    classify,
    iter_cases,
)

#: Top-level keys that depend on the run, not on the validation.
RUN_KEYS = frozenset({"date", "filepath", "filename"})
ENTITY_LISTS = frozenset({"applicable_entities", "passed_entities", "failed_entities"})
#: Reason sentences whose quoted literal IfcTester builds from a set.
_SET_REASONS = (
    re.compile(r'^(The material names and categories of ")(.*)(" does not match the requirement)$'),
    re.compile(r'^(The references ")(.*)(" do not match the requirements)$'),
    re.compile(r'^(The systems ")(.*)(" do not match the requirements)$'),
)
_ELEMENT = re.compile(r"^(#\d+=\w+)")


# --------------------------------------------------------------------------- #
# the two legs
# --------------------------------------------------------------------------- #
def ifctester_leg(case: Case) -> tuple[Outcome, dict | None]:
    """IfcTester overall outcome (as ``ids_conformance.run_ifctester``) and
    its ``reporter.Json`` report, JSON round-tripped like ``to_string()``."""
    import ifcopenshell
    from ifctester import ids as ids_mod
    from ifctester import reporter
    from ifctester.facet import get_pset, get_psets

    try:
        spec = ids_mod.open(str(case.ids_path), validate=True)
    except ids_mod.IdsXmlValidationError as e:
        return Outcome("invalid", _err(e.xml_error)), None
    except Exception as e:
        return Outcome("error", "ids.open: " + _err(e)), None
    if case.ifc_path is None:
        return Outcome("error", "no IFC"), None
    try:
        ifc = ifcopenshell.open(str(case.ifc_path))
        get_pset.cache_clear()
        get_psets.cache_clear()
        spec.validate(ifc)
        rep = reporter.Json(spec)
        rep.report()
        doc = json.loads(rep.to_string())
    except Exception as e:
        return Outcome("error", "validate: " + _err(e)), None
    failed = [s.name for s in spec.specifications if not s.status]
    out = Outcome("fail", "; ".join(failed)[:300]) if failed else Outcome("pass")
    return out, doc


def ifcfast_leg(case: Case) -> tuple[Outcome, dict | None]:
    """ifcfast overall outcome and ``IdsReport.to_ifctester_json()``."""
    validate_ids, IdsInvalidError, IdsUnsupportedError = _ifcfast_api()
    if case.ifc_path is None:
        return Outcome("error", "no IFC"), None
    try:
        rep = validate_ids(str(case.ids_path), str(case.ifc_path), on_unsupported="raise")
        doc = rep.to_ifctester_json()
    except IdsInvalidError as e:
        return Outcome("invalid", _err(e)), None
    except IdsUnsupportedError as e:
        return Outcome("unsupported", _err(e)), None
    except Exception as e:
        return Outcome("error", _err(e)), None
    return (Outcome("pass") if rep.ok else Outcome("fail")), doc


# --------------------------------------------------------------------------- #
# normalisation + diff
# --------------------------------------------------------------------------- #
def _sorted_literal(text: str) -> str:
    try:
        v = ast.literal_eval(text)
    except (ValueError, SyntaxError):
        return text
    if isinstance(v, (set, frozenset, list, tuple)):
        return repr(sorted((repr(x) for x in v)))
    return text


def normalise_reason(reason: str) -> str:
    for pat in _SET_REASONS:
        m = pat.match(reason)
        if m:
            return m.group(1) + _sorted_literal(m.group(2)) + m.group(3)
    return reason


def normalise(doc: Any, key: str | None = None, top: bool = True) -> Any:
    """Strip run-dependent and ``ifcfast_`` keys, canonicalise the
    fields that legitimately differ (module docstring)."""
    if isinstance(doc, dict):
        out = {}
        for k, v in doc.items():
            if k.startswith("ifcfast_") or (top and k in RUN_KEYS):
                continue
            if k in ("element", "element_type"):
                m = _ELEMENT.match(v) if isinstance(v, str) else None
                out[k] = m.group(1) if m else v
            elif k == "reason" and isinstance(v, str):
                out[k] = normalise_reason(v)
            else:
                out[k] = normalise(v, k, top=False)
        return out
    if isinstance(doc, list):
        items = [normalise(x, None, top=False) for x in doc]
        if key in ENTITY_LISTS:
            items.sort(key=lambda e: (e.get("id", 0) if isinstance(e, dict) else 0))
        return items
    return doc


def diff(a: Any, b: Any, path: str = "$") -> list[str]:
    """Field-level differences between two normalised documents
    (``a`` = IfcTester, ``b`` = ifcfast)."""
    out: list[str] = []
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k not in b:
                out.append(f"{path}.{k}: missing in ifcfast (ifctester={a[k]!r:.120})")
            elif k not in a:
                out.append(f"{path}.{k}: extra in ifcfast ({b[k]!r:.120})")
            else:
                out.extend(diff(a[k], b[k], f"{path}.{k}"))
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append(f"{path}: len ifctester={len(a)} ifcfast={len(b)}")
        for i, (x, y) in enumerate(zip(a, b)):
            out.extend(diff(x, y, f"{path}[{i}]"))
    elif a != b or type(a) is not type(b):
        out.append(f"{path}: ifctester={a!r:.160} ifcfast={b!r:.160}")
    return out


# --------------------------------------------------------------------------- #
# run
# --------------------------------------------------------------------------- #
def compare_case(case: Case, fast_leg=ifcfast_leg) -> dict:
    """``{"case", "label", "compared", "diffs"}`` for one suite case."""
    t, tdoc = ifctester_leg(case)
    f, fdoc = fast_leg(case)
    label = classify(case.expected, t, f)
    row = {"case": case.id, "label": label.value, "compared": False, "diffs": []}
    if label is Label.green and tdoc is not None and fdoc is not None:
        row["compared"] = True
        row["diffs"] = diff(normalise(tdoc), normalise(fdoc))
    return row


def run(folder: str | None = None, fast_leg=ifcfast_leg) -> list[dict]:
    cases = iter_cases(cases_root())
    if folder:
        cases = [c for c in cases if c.folder == folder]
    return [compare_case(c, fast_leg) for c in cases]


def summary(rows: list[dict]) -> tuple[int, int, list[dict]]:
    compared = [r for r in rows if r["compared"]]
    bad = [r for r in compared if r["diffs"]]
    return len(compared), len(bad), bad


# --------------------------------------------------------------------------- #
# pytest entry
# --------------------------------------------------------------------------- #
def _available() -> str | None:
    try:
        root = cases_root()
    except Exception as e:  # lock file unreadable
        return f"suite lock unreadable: {e}"
    if not root.is_dir():
        return f"IDS suite not fetched ({root})"
    try:
        import ifctester  # noqa: F401
    except ImportError:
        return "ifctester not installed"
    try:
        _ifcfast_api()
    except IfcfastUnavailable as e:
        return str(e)
    return None


def test_ifctester_json_parity():
    why = _available()
    if why:
        pytest.skip(why)
    rows = run()
    n, n_bad, bad = summary(rows)
    assert n > 0, "no case compared"
    detail = "\n".join(f"{r['case']}: {r['diffs'][:3]}" for r in bad[:20])
    assert not bad, f"{n_bad}/{n} IfcTester-JSON mismatches:\n{detail}"


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #
def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--folder", help="only this suite folder (entity, property, ...)")
    ap.add_argument("--json", type=Path, help="write every row (with diffs) here")
    ap.add_argument("-v", "--verbose", action="store_true", help="print every diff line")
    a = ap.parse_args(argv)
    why = _available()
    if why:
        print(f"cannot run: {why}", file=sys.stderr)
        return 2
    rows = run(a.folder)
    n, n_bad, bad = summary(rows)
    labels: dict[str, int] = {}
    for r in rows:
        labels[r["label"]] = labels.get(r["label"], 0) + 1
    print(f"cases {len(rows)}  labels {dict(sorted(labels.items()))}")
    print(f"compared {n}  mismatched {n_bad}")
    for r in bad:
        print(f"MISMATCH {r['case']} ({len(r['diffs'])} fields)")
        for d in r["diffs"] if a.verbose else r["diffs"][:3]:
            print(f"    {d}")
    if a.json:
        a.json.write_text(json.dumps(rows, indent=1), encoding="utf-8")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
