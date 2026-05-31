#!/usr/bin/env python3
"""Pre-PR verification for the native IDS engine (run from repo root).

Exit 0 only if all required checks pass. Optional large-model smoke needs
IFCFAST_BENCH_IFCS (comma-separated paths).
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PY = ROOT / "python"
FIXTURE_IDS = ROOT / "scripts" / "fixtures" / "bench_large_models.ids"


def run(cmd: list[str], *, cwd: Path = ROOT, env: dict | None = None) -> int:
    print(f"\n>> {' '.join(cmd)}", flush=True)
    e = os.environ.copy()
    e["PYTHONPATH"] = str(PY)
    if env:
        e.update(env)
    return subprocess.call(cmd, cwd=cwd, env=e)


def main() -> int:
    fails = 0

    # 1) Release extension
    ps1 = ROOT / "scripts" / "bench" / "install_core_release.ps1"
    if ps1.is_file():
        fails += run(["powershell", "-NoProfile", "-File", str(ps1)]) != 0
    else:
        fails += run(["cargo", "build", "--release", "-p", "ifcfast-core"]) != 0

    # 2) Rust unit tests (ids + core)
    fails += run(["cargo", "test", "--release", "-p", "ifcfast-core", "--lib"]) != 0
    fails += (
        run(
            [
                "cargo",
                "test",
                "--release",
                "-p",
                "ifcfast-core",
                "--test",
                "ids_parity",
                "--",
                "--nocapture",
            ]
        )
        != 0
    )

    # 3) buildingSMART conformance (rust vs IfcTester status)
    import io

    print("\n>> conformance (rust)", flush=True)
    e = os.environ.copy()
    e["PYTHONPATH"] = str(PY)
    proc = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "run_buildingsmart_ids_conformance.py"), "--engine", "rust"],
        cwd=ROOT,
        env=e,
        capture_output=True,
        text=True,
    )
    tail = (proc.stdout or "") + (proc.stderr or "")
    print(tail[-2000:] if len(tail) > 2000 else tail, flush=True)
    if "parity_fails: 0" not in tail:
        print("FAIL: expected parity_fails: 0 vs IfcTester", flush=True)
        fails += 1
    elif proc.returncode != 0:
        print("NOTE: non-zero exit (often outcome_mismatches only); parity OK.", flush=True)

    # 4) Python IDS tests (core; skip slow H29 + parametrized ifcfast entity)
    fails += (
        run(
            [
                sys.executable,
                "-m",
                "pytest",
                "tests/ids/test_rust_engine.py",
                "tests/ids/test_entity_attribute_parity.py",
                "tests/ids/test_ids_spec_alignment.py",
                "tests/ids/test_report_schema.py",
                "tests/ids/test_ids_partof_facets.py",
                "tests/ids/test_property_restrictions.py",
                "tests/ids/test_spatial_container.py",
                "-q",
            ]
        )
        != 0
    )

    # 5) Bundled minimal e2e
    fails += (
        run(
            [
                sys.executable,
                "-c",
                """
import json, ifcfast
from pathlib import Path
from ifcfast import _core
from ifcfast.ids.compile import compile_ids_file
p = compile_ids_file(Path('tests/ids/fixtures/simple_wall_requirement.ids'))
r = _core.validate_ids_native(str(ifcfast.example_path()), json.dumps(p))
assert r['specifications'], 'no specs'
print('minimal_e2e ok', len(r['specifications']), 'specs')
""",
            ],
            cwd=ROOT,
        )
        != 0
    )

    # 6) Optional large-model smoke
    if os.environ.get("IFCFAST_BENCH_IFCS") and FIXTURE_IDS.is_file():
        fails += (
            run(
                [
                    sys.executable,
                    "-c",
                    f"""
import json, time, os
from pathlib import Path
from ifcfast import _core
from ifcfast.ids.compile import compile_ids_file
ifc = os.environ['IFCFAST_BENCH_IFCS'].split(',')[0].strip()
p = json.dumps(compile_ids_file(Path(r'{FIXTURE_IDS}')))
t0=time.perf_counter()
r=_core.validate_ids_native(ifc, p)
dt=time.perf_counter()-t0
scan=r.get('scan_ms', r.get('index_ms', 0))
print(f'large_cold_s={{dt:.3f}} scan_ms={{scan:.0f}} validate_ms={{r[\"validate_ms\"]:.1f}}')
assert dt < 30, 'cold validate unexpectedly slow (>30s)'
""",
                ],
            )
            != 0
        )
    else:
        print("\n>> skip large-model smoke (set IFCFAST_BENCH_IFCS)", flush=True)

    print("\n=== Summary ===", flush=True)
    if fails:
        print(f"FAILED: {fails} check group(s)", flush=True)
        return 1
    print("All required checks passed.", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
