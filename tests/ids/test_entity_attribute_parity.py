"""Parity vs IfcTester on bundled IFC + simple_wall IDS."""

from __future__ import annotations

import pytest

from tests.ids.conftest import SIMPLE_WALL_IDS
from tests.ids.parity import assert_parity


@pytest.mark.skipif(not SIMPLE_WALL_IDS.is_file(), reason="fixture IDS not on disk")
def test_simple_wall_example_ifc(example_ifc):
    assert_parity(SIMPLE_WALL_IDS, example_ifc, engine="ifcfast")


@pytest.mark.skipif(not SIMPLE_WALL_IDS.is_file(), reason="fixture IDS not on disk")
def test_simple_wall_auto_engine(example_ifc):
    assert_parity(SIMPLE_WALL_IDS, example_ifc, engine="auto")
