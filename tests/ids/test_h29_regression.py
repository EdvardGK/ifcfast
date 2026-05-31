"""Slow parity on H29 + Nobel (optional local paths)."""

from __future__ import annotations

import pytest

from tests.ids.conftest import H29_GENERELLT, NOBEL_A1_0002
from tests.ids.parity import assert_parity


@pytest.mark.slow
@pytest.mark.skipif(not H29_GENERELLT.is_file(), reason="H29 IDS not on disk")
@pytest.mark.skipif(not NOBEL_A1_0002.is_file(), reason="Nobel A1_0002 not on disk")
def test_h29_generellt_nobel_a1_0002_auto():
    """Full parity via engine=auto (IfcTester fallback for non-product / PartOf specs)."""
    assert_parity(H29_GENERELLT, NOBEL_A1_0002, engine="auto")


@pytest.mark.slow
@pytest.mark.skipif(not H29_GENERELLT.is_file(), reason="H29 IDS not on disk")
@pytest.mark.skipif(not NOBEL_A1_0002.is_file(), reason="Nobel A1_0002 not on disk")
def test_h29_generellt_nobel_a1_0002_ifcfast():
    """Fast-path only — may differ on building/storey-level applicability."""
    from ifcfast.ids import validate

    report = validate(H29_GENERELLT, NOBEL_A1_0002, engine="ifcfast", use_cache=False)
    assert report.total_specifications == 9
    assert report.applicable_total > 0 or report.failed_specifications > 0
