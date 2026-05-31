#!/usr/bin/env python3
"""
Run buildingSMART IDS 1.0 implementer TestCases against IfcTester + ifcfast.

Clone official tests:
  git clone -b development https://github.com/buildingSMART/IDS.git C:\\code\\buildingSMART-IDS

Set:
  IDS_TESTCASES_ROOT=C:\\code\\buildingSMART-IDS\\Documentation\\ImplementersDocumentation\\TestCases
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

DEFAULT_ROOT = Path(
    os.environ.get(
        "IDS_TESTCASES_ROOT",
        r"C:\code\buildingSMART-IDS\Documentation\ImplementersDocumentation\TestCases",
    )
)


@dataclass
class CaseResult:
    facet: str
    stem: str
    expected: str
    ifctester_status: bool | None
    ifcfast_status: bool | None
    parity_ok: bool
    ifcfast_engine: str
    error: str | None = None


def _expected_from_stem(stem: str) -> str | None:
    if stem.startswith("pass-"):
        return "pass"
    if stem.startswith("fail-"):
        return "fail"
    if stem.startswith("invalid-"):
        return "invalid"
    return None


def _run_pair(ids_path: Path, ifc_path: Path, *, engine: str) -> tuple[bool, str]:
    from ifctester import ids as ifctester_ids
    import ifcopenshell

    doc = ifctester_ids.open(str(ids_path))
    ifc = ifcopenshell.open(str(ifc_path))
    doc.validate(ifc)
    ref_status = bool(doc.specifications[0].status) if doc.specifications else False

    if engine == "ifctester":
        return ref_status, "ifctester"

    import ifcfast
    from ifcfast.ids import validate

    report = validate(ids_path, ifc_path, engine=engine, use_cache=False)
    fast_status = bool(report.specifications[0].status) if report.specifications else False
    used = report.specifications[0].engine if report.specifications else engine
    return fast_status, used


def collect_cases(root: Path, facets: list[str] | None) -> list[tuple[str, Path, Path, str]]:
    out: list[tuple[str, Path, Path, str]] = []
    for facet_dir in sorted(root.iterdir()):
        if not facet_dir.is_dir():
            continue
        facet = facet_dir.name
        if facets and facet not in facets:
            continue
        seen: set[str] = set()
        for ids_file in sorted(facet_dir.glob("*.ids")):
            stem = ids_file.stem
            if stem in seen:
                continue
            seen.add(stem)
            expected = _expected_from_stem(stem)
            if expected is None:
                continue
            ifc_file = ids_file.with_suffix(".ifc")
            if not ifc_file.is_file():
                continue
            out.append((facet, ids_file, ifc_file, expected))
    return out


def main() -> int:
    p = argparse.ArgumentParser(description="buildingSMART IDS TestCases conformance")
    p.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    p.add_argument("--facet", action="append", help="Limit to facet folder(s), e.g. entity")
    p.add_argument(
        "--engine",
        choices=("ifcfast", "auto", "rust"),
        default="ifcfast",
        help="ifcfast engine (default ifcfast = no IfcTester fallback)",
    )
    p.add_argument("--limit", type=int, default=0)
    p.add_argument("--include-invalid", action="store_true")
    p.add_argument("--json", type=Path, default=None)
    args = p.parse_args()

    if not args.root.is_dir():
        print(f"TestCases root not found: {args.root}", file=sys.stderr)
        print("Clone buildingSMART/IDS or set IDS_TESTCASES_ROOT", file=sys.stderr)
        return 2

    cases = collect_cases(args.root, args.facet)
    if not args.include_invalid:
        cases = [c for c in cases if c[3] != "invalid"]
    if args.limit:
        cases = cases[: args.limit]

    results: list[CaseResult] = []
    mismatches = 0
    parity_fails = 0

    for facet, ids_path, ifc_path, expected in cases:
        try:
            ref_status, _ = _run_pair(ids_path, ifc_path, engine="ifctester")
            fast_status, used = _run_pair(ids_path, ifc_path, engine=args.engine)
        except Exception as exc:
            results.append(
                CaseResult(
                    facet=facet,
                    stem=ids_path.stem,
                    expected=expected,
                    ifctester_status=None,
                    ifcfast_status=None,
                    parity_ok=False,
                    ifcfast_engine=args.engine,
                    error=str(exc),
                )
            )
            mismatches += 1
            continue

        ref_pass = ref_status
        fast_pass = fast_status
        expect_pass = expected == "pass"
        parity_ok = ref_pass == fast_pass
        outcome_ok = fast_pass == expect_pass and ref_pass == expect_pass

        if not parity_ok:
            parity_fails += 1
        if not outcome_ok:
            mismatches += 1

        results.append(
            CaseResult(
                facet=facet,
                stem=ids_path.stem,
                expected=expected,
                ifctester_status=ref_pass,
                ifcfast_status=fast_pass,
                parity_ok=parity_ok,
                ifcfast_engine=used,
            )
        )

    total = len(results)
    errors = sum(1 for r in results if r.error)
    print(f"Cases: {total}  parity_fails: {parity_fails}  outcome_mismatches: {mismatches}  errors: {errors}")

    if args.json:
        args.json.write_text(
            json.dumps([asdict(r) for r in results], indent=2),
            encoding="utf-8",
        )
        print(f"Wrote {args.json}")

    return 1 if mismatches or parity_fails else 0


if __name__ == "__main__":
    raise SystemExit(main())
