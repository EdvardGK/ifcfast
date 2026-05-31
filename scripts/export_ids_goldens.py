#!/usr/bin/env python3
"""Export IfcTester IDS validation goldens for Rust parity tests."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def export(ids_path: Path, ifc_path: Path, out_path: Path) -> None:
    from tests.ids.parity import ifctester_results

    rows = ifctester_results(ids_path, ifc_path)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(rows, indent=2), encoding="utf-8")
    print(f"Wrote {out_path} ({len(rows)} specs)")


def main() -> None:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("--ids", type=Path, required=True)
    parser.add_argument("--ifc", type=Path, required=True)
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        default=root / "tests/ids/goldens" / "golden.json",
    )
    args = parser.parse_args()
    export(args.ids, args.ifc, args.output)


if __name__ == "__main__":
    main()
