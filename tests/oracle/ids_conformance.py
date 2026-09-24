"""IDS 1.0 conformance harness: ifcfast vs IfcTester vs filename truth.

Design: docs/plans/2026-09-24_ids-validation-design.md §4.

Suite
-----
The buildingSMART IDS test cases (``Documentation/ImplementersDocumentation/
TestCases`` in ``buildingSMART/IDS``, CC BY-ND 4.0, never vendored) are
fetched by ``scripts/fetch_ids_testcases.py`` into
``~/.cache/ifcfast/ids-testcases/<sha>/`` at the sha pinned in
``tests/oracle/ids_testcases.lock`` (``IFCFAST_IDS_TESTCASES`` overrides the
directory). Each case is ``<folder>/<prefix>-<name>.ids`` + same-stem ``.ifc``.

Truth
-----
From the filename prefix (TestCases/scripts.md):

- ``pass-``    all requirements are satisfied        -> overall ``pass``
- ``fail-``    at least one requirement fails         -> overall ``fail``
- ``invalid-`` "at least one requirement fails (invalid files do not comply
  with the Audit tool, they could not be satisfied, regardless of IFC
  contents)"                                          -> ``fail`` OR ``invalid``

Agreement rule, identical for BOTH engines: an ``invalid-`` case agrees when
the engine reports ``fail`` or rejects the IDS as ``invalid`` (IfcTester:
``IdsXmlValidationError``; ifcfast: typed ``IdsInvalidError``). Only ``pass``
or ``error`` on an ``invalid-`` case is a disagreement. IfcTester has no
semantic IDS audit (only XSD decoding, ``ifctester/ids.py:56-65``), so it
reaches these cases through ``fail``. The CLI's ``rejct`` column is
informational: how many ``invalid-`` cases an engine rejected outright.

IfcTester overall status
------------------------
Computed exactly as IfcTester reports it: ``ids.open(path, validate=True)``
(XSD; ``IdsXmlValidationError`` -> ``invalid``), ``ifcopenshell.open(ifc)``,
``Ids.validate(ifc)`` with the default ``should_filter_version=False`` (as the
IfcTester CLI does, ``ifctester/__main__.py``), then overall pass iff every
specification's ``status`` is truthy — ``ifctester/reporter.py:261-274``
(``Json.report``: ``status = False`` as soon as one spec fails). IfcOpenShell's
own generator for these cases asserts the same per-spec ``spec.status is
expected`` (``src/ifctester/test/ids_doc_generator.py:143-160`` at tag
``ifcopenshell-python-0.8.5``; facet cases wrap one requirement in a
``minOccurs=1`` spec, lines 71-133).

Labels (per case)
-----------------
``green`` both agree with truth · ``ifcfast_bug`` IfcTester right, ifcfast
wrong (the only label that fails pytest) · ``ifctester_bug`` ifcfast right,
IfcTester wrong · ``test_case_drift`` both wrong and both give the same
answer · ``both_error`` both raised. Both wrong with *different* answers is
``ifcfast_bug`` until triaged into ``tests/oracle/ids_xfail.toml`` as
``test_case_drift``.

Known failures
--------------
``tests/oracle/ids_xfail.toml``::

    [[xfail]]
    case  = "tolerance/pass-comparison_tolerance_for_floating_point_one_lower_bound"
    label = "ifctester_bug"          # or "test_case_drift"
    issue = "#NNN"
    note  = "IfcTester is_x() is relative-only (facet.py:54-60)"

Strict: an xfailed case where IfcTester now agrees with truth, or where the
observed label no longer matches, FAILS ("remove the xfail").

CLI (runs the IfcTester leg now; adds the ifcfast leg once built)::

    python -m tests.oracle.ids_conformance [--folder entity] [--json out.json]
"""

from __future__ import annotations

import argparse
import enum
import json
import os
import re
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Literal

import pytest

from .report import Classification, Collector, DisagreementRecord

REPO_ROOT = Path(__file__).resolve().parents[2]
LOCK_PATH = Path(__file__).resolve().parent / "ids_testcases.lock"
XFAIL_PATH = Path(__file__).resolve().parent / "ids_xfail.toml"
CACHE_BASE = Path.home() / ".cache" / "ifcfast" / "ids-testcases"
ENV_OVERRIDE = "IFCFAST_IDS_TESTCASES"
FETCH_HINT = "python scripts/fetch_ids_testcases.py  (downloads + verifies the pinned suite)"

Expected = Literal["pass", "fail", "invalid"]
Status = Literal["pass", "fail", "invalid", "error"]
_PREFIX = re.compile(r"^(pass|fail|invalid)-")


# --------------------------------------------------------------------------- #
# data model
# --------------------------------------------------------------------------- #
@dataclass(frozen=True)
class Case:
    folder: str
    stem: str
    expected: Expected
    ids_path: Path
    ifc_path: Path | None

    @property
    def id(self) -> str:
        return f"{self.folder}/{self.stem}"


@dataclass(frozen=True)
class Outcome:
    status: Status
    detail: str = ""


class Label(enum.Enum):
    green = "green"
    ifcfast_bug = "ifcfast_bug"
    ifctester_bug = "ifctester_bug"
    test_case_drift = "test_case_drift"
    both_error = "both_error"


class IfcfastUnavailable(RuntimeError):
    """The native engine is not built into this ifcfast (skip the leg)."""


# --------------------------------------------------------------------------- #
# suite discovery
# --------------------------------------------------------------------------- #
def cases_root() -> Path:
    """Case root: ``$IFCFAST_IDS_TESTCASES`` or the cache dir at the locked sha."""
    env = os.environ.get(ENV_OVERRIDE)
    if env:
        return Path(env).expanduser()
    lock = json.loads(LOCK_PATH.read_text(encoding="utf-8"))
    return CACHE_BASE / lock["sha"]


def iter_cases(root: Path) -> list[Case]:
    """Every ``<folder>/<prefix>-*.ids`` under ``root``, sorted by id.

    Fails loudly on a ``.ids`` with an unknown prefix or a pass/fail case
    without its ``.ifc``. ``invalid-`` cases may lack an ``.ifc``.
    """
    root = Path(root)
    if not root.is_dir():
        raise FileNotFoundError(f"IDS test-case dir not found: {root}. Fetch: {FETCH_HINT}")
    out: list[Case] = []
    for ids_path in sorted(root.glob("*/*.ids")):
        m = _PREFIX.match(ids_path.name)
        if not m:
            raise ValueError(f"test case without pass-/fail-/invalid- prefix: {ids_path}")
        expected = m.group(1)
        ifc = ids_path.with_suffix(".ifc")
        if not ifc.exists():
            if expected != "invalid":
                raise FileNotFoundError(f"{expected} case has no paired IFC: {ifc}")
            ifc = None
        out.append(Case(ids_path.parent.name, ids_path.stem, expected, ids_path, ifc))
    if not out:
        raise FileNotFoundError(f"no *.ids cases under {root}")
    return out


# --------------------------------------------------------------------------- #
# the two legs
# --------------------------------------------------------------------------- #
def _err(e: BaseException) -> str:
    return f"{type(e).__name__}: {str(e).splitlines()[0][:200] if str(e) else ''}"


def run_ifctester(case: Case) -> Outcome:
    """IfcTester overall outcome (see module docstring for the mapping)."""
    import ifcopenshell
    from ifctester import ids as ids_mod
    from ifctester.facet import get_pset, get_psets

    try:
        spec = ids_mod.open(str(case.ids_path), validate=True)
    except ids_mod.IdsXmlValidationError as e:
        return Outcome("invalid", _err(e.xml_error))
    except Exception as e:  # parse crash is not an "invalid" verdict
        return Outcome("error", "ids.open: " + _err(e))
    if case.ifc_path is None:
        return Outcome("error", "IDS parsed as valid but case has no IFC to validate")
    try:
        ifc = ifcopenshell.open(str(case.ifc_path))
        get_pset.cache_clear()  # Ids.validate clears too; belt and braces across cases
        get_psets.cache_clear()
        spec.validate(ifc)
    except Exception as e:
        return Outcome("error", "validate: " + _err(e))
    failed = [s.name for s in spec.specifications if not s.status]
    if failed:
        return Outcome("fail", "failed specs: " + "; ".join(failed)[:300])
    return Outcome("pass")


def _ifcfast_api():
    try:
        import ifcfast
    except ImportError as e:
        raise IfcfastUnavailable(f"ifcfast not importable: {e}") from e
    fn = getattr(ifcfast, "validate_ids", None)
    if fn is None:
        raise IfcfastUnavailable("ifcfast.validate_ids not built yet")
    try:
        return fn, ifcfast.IdsInvalidError, ifcfast.IdsUnsupportedError
    except AttributeError as e:
        raise IfcfastUnavailable(f"ifcfast IDS error classes missing: {e}") from e


def ifcfast_available() -> bool:
    try:
        _ifcfast_api()
    except IfcfastUnavailable:
        return False
    return True


def run_ifcfast(case: Case) -> Outcome:
    """ifcfast overall outcome. Raises :class:`IfcfastUnavailable` if not built.

    ``IdsInvalidError`` -> ``invalid``; ``IdsUnsupportedError`` -> ``error``
    (a coverage gap is a failure, never a skip); ``report.ok`` -> pass/fail.
    """
    validate_ids, IdsInvalidError, IdsUnsupportedError = _ifcfast_api()
    if case.ifc_path is None:
        # No IFC: only the IDS can be judged. Must raise IdsInvalidError.
        return Outcome("error", "case has no IFC; ifcfast needs an IDS-only validation entry")
    try:
        report = validate_ids(str(case.ids_path), str(case.ifc_path), on_unsupported="raise")
    except IdsInvalidError as e:
        return Outcome("invalid", _err(e))
    except IdsUnsupportedError as e:
        return Outcome("error", "unsupported: " + _err(e))
    except Exception as e:
        return Outcome("error", _err(e))
    if report.ok:
        return Outcome("pass")
    failed = report.specs[report.specs["status"] != "pass"]["name"].astype(str).tolist()
    return Outcome("fail", "failed specs: " + "; ".join(failed)[:300])


# --------------------------------------------------------------------------- #
# classification
# --------------------------------------------------------------------------- #
def agrees(expected: Expected, outcome: Outcome) -> bool:
    """Filename-truth agreement; same rule for both engines (module docstring)."""
    if outcome.status == "error":
        return False
    if expected == "invalid":
        return outcome.status in ("invalid", "fail")
    return outcome.status == expected


def rejected_outright(expected: Expected, outcome: Outcome) -> bool:
    """Informational: an ``invalid-`` case the engine refused as an invalid IDS."""
    return expected == "invalid" and outcome.status == "invalid"


ifctester_agrees = agrees
ifcfast_agrees = agrees


def classify(expected: Expected, ifctester: Outcome, ifcfast: Outcome) -> Label:
    t_ok = ifctester_agrees(expected, ifctester)
    f_ok = ifcfast_agrees(expected, ifcfast)
    if t_ok and f_ok:
        return Label.green
    if t_ok:
        return Label.ifcfast_bug
    if f_ok:
        return Label.ifctester_bug
    if ifctester.status == "error" and ifcfast.status == "error":
        return Label.both_error
    if ifctester.status == ifcfast.status:
        return Label.test_case_drift
    return Label.ifcfast_bug  # both wrong, differently: ours until triaged


def record(collector: Collector, case: Case, label: Label, t: Outcome, f: Outcome | None) -> None:
    """Record a non-green case into the shared oracle Collector."""
    if label is Label.green:
        return
    collector.record(
        DisagreementRecord(
            surface="ids",
            fixture=case.id,
            guid="",
            group=case.folder,
            kind=label.value,
            detail=f"expected={case.expected} ifctester={t.status} ifcfast={f.status if f else 'n/a'}"
            f" | ifctester: {t.detail} | ifcfast: {f.detail if f else ''}",
            classification=Classification(label.value),
            ours=f.status if f else None,
            truth=case.expected,
        )
    )


# --------------------------------------------------------------------------- #
# xfail register
# --------------------------------------------------------------------------- #
XFAIL_LABELS = frozenset({Label.ifctester_bug, Label.test_case_drift})


@dataclass(frozen=True)
class XFail:
    case: str
    label: Label
    issue: str
    note: str


def load_xfails(path: Path = XFAIL_PATH, known_cases: set[str] | None = None) -> dict[str, XFail]:
    """Parse ``ids_xfail.toml``; every field validated, duplicates rejected."""
    try:
        import tomllib
    except ModuleNotFoundError:  # py3.10
        import tomli as tomllib  # type: ignore[no-redef]
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    unknown_top = set(data) - {"xfail"}
    if unknown_top:
        raise ValueError(f"{path.name}: unknown top-level keys {sorted(unknown_top)}")
    out: dict[str, XFail] = {}
    for i, e in enumerate(data.get("xfail", [])):
        where = f"{path.name} [[xfail]] #{i}"
        extra = set(e) - {"case", "label", "issue", "note"}
        if extra:
            raise ValueError(f"{where}: unknown keys {sorted(extra)}")
        for k in ("case", "label", "issue", "note"):
            if not isinstance(e.get(k), str) or not e[k].strip():
                raise ValueError(f"{where}: {k!r} must be a non-empty string")
        try:
            label = Label(e["label"])
        except ValueError:
            raise ValueError(f"{where}: label {e['label']!r} not a Label") from None
        if label not in XFAIL_LABELS:
            raise ValueError(
                f"{where}: label {label.value!r} cannot be xfailed; only "
                f"{sorted(x.value for x in XFAIL_LABELS)} are benign"
            )
        if not re.fullmatch(r"#\d+", e["issue"]):
            raise ValueError(f"{where}: issue must look like '#123', got {e['issue']!r}")
        if e["case"] in out:
            raise ValueError(f"{where}: duplicate case {e['case']!r}")
        if known_cases is not None and e["case"] not in known_cases:
            raise ValueError(f"{where}: case {e['case']!r} not in the pinned suite")
        out[e["case"]] = XFail(e["case"], label, e["issue"], e["note"])
    return out


def xfail_verdict(xf: XFail, expected: Expected, t: Outcome, observed: Label | None) -> str | None:
    """Return a failure message if the xfail is stale, else None.

    ``observed`` is None while the ifcfast leg is not built; the IfcTester
    half of the check still applies (both benign labels need IfcTester wrong).
    """
    if ifctester_agrees(expected, t):
        return (
            f"xfail stale: IfcTester now agrees with truth on {xf.case} "
            f"({xf.label.value}, {xf.issue}) — remove the xfail"
        )
    if observed is None:
        return None
    if observed is Label.green:
        return f"xfail stale: {xf.case} is green — remove the xfail ({xf.issue})"
    if xf.label is Label.test_case_drift and observed in (Label.test_case_drift, Label.ifcfast_bug):
        return None  # triaged: both wrong, drift confirmed by a human
    if observed is xf.label:
        return None
    return f"xfail label mismatch on {xf.case}: register says {xf.label.value}, observed {observed.value}"


# --------------------------------------------------------------------------- #
# pytest entry
# --------------------------------------------------------------------------- #
def _collect_params() -> list:
    try:
        root = cases_root()
        cases = iter_cases(root)
    except FileNotFoundError as e:
        return [pytest.param(None, id="suite-missing", marks=pytest.mark.skip(reason=f"{e}"))]
    return [pytest.param(c, id=c.id) for c in cases]


_SESSION = Collector()


@pytest.fixture(scope="module")
def ids_session(request):
    yield _SESSION
    by = _SESSION.summary_by_group()
    if by:
        tr = request.config.pluginmanager.get_plugin("terminalreporter")
        lines = ["", "IDS conformance (non-green, per folder):"]
        lines += [f"  {g}: {c}" for g, c in by.items()]
        if tr is not None:
            tr.write_line("\n".join(lines))


@pytest.fixture(scope="module")
def xfails():
    try:
        known = {c.id for c in iter_cases(cases_root())}
    except FileNotFoundError:
        known = None
    return load_xfails(XFAIL_PATH, known)


@pytest.mark.parametrize("case", _collect_params())
def test_ids_conformance(case: Case, ids_session: Collector, xfails: dict[str, XFail]):
    pytest.importorskip("ifctester", reason="IfcTester oracle missing: pip install -e '.[dev]'")
    t = run_ifctester(case)
    xf = xfails.get(case.id)

    # IfcTester half of the strict-xfail check runs even before ifcfast exists.
    if xf is not None:
        msg = xfail_verdict(xf, case.expected, t, None)
        if msg:
            pytest.fail(msg)

    try:
        f = run_ifcfast(case)
    except IfcfastUnavailable as e:
        if not ifctester_agrees(case.expected, t):
            print(f"[ids] IfcTester disagrees with truth on {case.id}: "
                  f"expected {case.expected}, got {t.status} ({t.detail})")
        pytest.skip(str(e))

    label = classify(case.expected, t, f)
    record(ids_session, case, label, t, f)
    if xf is not None:
        msg = xfail_verdict(xf, case.expected, t, label)
        if msg:
            pytest.fail(msg)
        return
    if label is Label.ifcfast_bug:
        pytest.fail(
            f"ifcfast_bug on {case.id}: expected {case.expected}, ifcfast {f.status} "
            f"({f.detail}); IfcTester {t.status} ({t.detail})"
        )


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #
def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="IDS conformance: IfcTester (and ifcfast once built) vs filename truth")
    ap.add_argument("--folder", action="append", help="restrict to folder(s), e.g. entity")
    ap.add_argument("--json", type=Path, help="write per-case results as JSON")
    a = ap.parse_args(argv)

    try:
        root = cases_root()
        cases = iter_cases(root)
    except FileNotFoundError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 2
    if a.folder:
        cases = [c for c in cases if c.folder in set(a.folder)]
        if not cases:
            print(f"ERROR: no cases in folder(s) {a.folder}", file=sys.stderr)
            return 2

    import ifctester

    with_fast = ifcfast_available()
    known = {c.id for c in iter_cases(root)}
    xf = load_xfails(XFAIL_PATH, known)
    collector = Collector()
    rows = []
    for c in cases:
        t = run_ifctester(c)
        f = run_ifcfast(c) if with_fast else None
        label = classify(c.expected, t, f) if f else None
        if label:
            record(collector, c, label, t, f)
        rows.append({
            "case": c.id, "folder": c.folder, "expected": c.expected,
            "ifctester": asdict(t), "ifctester_agrees": ifctester_agrees(c.expected, t),
            "ifctester_rejected_invalid": rejected_outright(c.expected, t),
            "ifcfast": asdict(f) if f else None, "label": label.value if label else None,
            "xfail": asdict(xf[c.id]) | {"label": xf[c.id].label.value} if c.id in xf else None,
        })

    print(f"suite: {root}  ({len(cases)} cases)  IfcTester {ifctester.__version__}"
          f"  ifcfast leg: {'ON' if with_fast else 'not built (skipped)'}\n")
    hdr = (f"{'folder':<15}{'total':>6}{'pass':>6}{'fail':>6}{'inval':>6}"
           f"{'agree':>7}{'disagr':>7}{'rejct':>7}")
    print(hdr)
    print("-" * len(hdr))
    folders = sorted({r["folder"] for r in rows})
    tot = [0] * 7
    for fo in folders + ["TOTAL"]:
        rs = rows if fo == "TOTAL" else [r for r in rows if r["folder"] == fo]
        vals = [
            len(rs),
            sum(r["expected"] == "pass" for r in rs),
            sum(r["expected"] == "fail" for r in rs),
            sum(r["expected"] == "invalid" for r in rs),
            sum(r["ifctester_agrees"] for r in rs),
            sum(not r["ifctester_agrees"] for r in rs),
            sum(r["ifctester_rejected_invalid"] for r in rs),
        ]
        if fo == "TOTAL":
            print("-" * len(hdr))
        print(f"{fo:<15}" + "".join(f"{v:>{w}}" for v, w in zip(vals, (6, 6, 6, 6, 7, 7, 7))))
    print("\nagree/disagr: IfcTester vs filename truth (invalid- accepts 'fail' or 'invalid', per scripts.md)."
          "\nrejct: invalid- cases rejected outright as an invalid IDS (informational).\n")

    dis = [r for r in rows if not r["ifctester_agrees"]]
    print(f"IfcTester disagrees with filename truth on {len(dis)} case(s):")
    for r in dis:
        tag = f"  [xfail {r['xfail']['issue']}]" if r["xfail"] else ""
        print(f"  {r['case']}: expected {r['expected']}, IfcTester {r['ifctester']['status']}"
              f" — {r['ifctester']['detail'][:140]}{tag}")
    strict_only = [r for r in rows if r["expected"] == "invalid" and r["ifctester"]["status"] == "fail"]
    print(f"\ninvalid- cases IfcTester reports as 'fail' (accepted) rather than rejecting the IDS: {len(strict_only)}")
    for r in strict_only:
        print(f"  {r['case']}")

    bugs = 0
    if with_fast:
        print("\nifcfast vs IfcTester vs truth (non-green, per folder):")
        for g, cnt in collector.summary_by_group().items():
            print(f"  {g}: {cnt}")
        bugs = sum(1 for r in rows if r["label"] == "ifcfast_bug" and not r["xfail"])
        for r in rows:
            if r["label"] not in (None, "green"):
                print(f"  [{r['label']}] {r['case']}: expected {r['expected']}, "
                      f"ifcfast {r['ifcfast']['status']}, IfcTester {r['ifctester']['status']}")

    if a.json:
        a.json.write_text(json.dumps({"root": str(root), "rows": rows}, indent=2), encoding="utf-8")
        print(f"\nwrote {a.json}")
    return 1 if bugs else 0


if __name__ == "__main__":
    sys.exit(main())
