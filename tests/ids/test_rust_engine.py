"""Rust-native IDS engine parity."""

from __future__ import annotations

import json

import pytest

from tests.ids.conftest import SIMPLE_WALL_IDS
from tests.ids.parity import assert_parity


@pytest.mark.skipif(not SIMPLE_WALL_IDS.is_file(), reason="fixture IDS not on disk")
def test_simple_wall_rust_engine(example_ifc):
    assert_parity(SIMPLE_WALL_IDS, example_ifc, engine="rust")


@pytest.mark.skipif(not SIMPLE_WALL_IDS.is_file(), reason="fixture IDS not on disk")
def test_compile_ids_roundtrip(example_ifc):
    from ifcfast.ids.compile import compile_ids_file
    from ifcfast import _core

    payload = compile_ids_file(SIMPLE_WALL_IDS)
    raw = _core.validate_ids_native(str(example_ifc), json.dumps(payload))
    assert raw["engine"] == "rust"
    assert len(raw["specifications"]) >= 1
