"""IDS test fixtures paths."""

from __future__ import annotations

from pathlib import Path

import pytest

FIXTURE_IDS = Path(r"C:\code\IDS\ifc-ids-mcp\tests\validation\fixtures\valid_ids_files")
SIMPLE_WALL_IDS = FIXTURE_IDS / "simple_wall_requirement.ids"
H29_GENERELLT = Path(r"C:\code\IDS\H29\ids\GENERELLT_IDS_H29.ids")
NOBEL_A1_0002 = Path(
    r"C:\Users\JonatanJacobsson\Nobel Center\Syncpoint - General\StreamBIM\A1_2b_BIM_XXX_0002_00.ifc"
)


@pytest.fixture
def example_ifc():
    import ifcfast

    return ifcfast.example_path()


def pytest_configure(config):
    config.addinivalue_line("markers", "slow: slow Nobel/H29 parity tests")
