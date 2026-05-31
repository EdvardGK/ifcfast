"""Optional IDS-Audit-tool on fixture .ids (skipped if tool missing)."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from tests.ids.conftest import SIMPLE_WALL_IDS

AUDIT_TOOL = os.environ.get("IDS_AUDIT_TOOL_PATH", "")


@pytest.mark.skipif(not SIMPLE_WALL_IDS.is_file(), reason="fixture IDS missing")
@pytest.mark.skipif(not AUDIT_TOOL, reason="Set IDS_AUDIT_TOOL_PATH to ids-tool.exe")
def test_fixture_ids_audit_tool():
    tool = Path(AUDIT_TOOL)
    if not tool.is_file():
        pytest.skip(f"IDS audit tool not found: {tool}")
    proc = subprocess.run(
        [str(tool), str(SIMPLE_WALL_IDS)],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr or proc.stdout
