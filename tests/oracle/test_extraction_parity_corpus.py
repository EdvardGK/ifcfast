"""Real-corpus gate for classifications / materials / psets vs ifcopenshell.

Wraps :mod:`tests.oracle.extraction_parity` (GH #216 / #217 / #219 /
#213) over every file in ``IFCFAST_CORPUS`` (colon-separated), the same
variable the subset / hotswap / mutate real-file gates read. Skips when
unset. One model at a time — each parametrised case opens the file in
both libraries, so never run these in parallel.

    IFCFAST_CORPUS="scratch/knm/KNM_ARK.ifc:scratch/hi90/HI90_ARK.ifc" \\
        python -m pytest tests/oracle/test_extraction_parity_corpus.py -s
"""

from __future__ import annotations

import json
import os
import tomllib
from pathlib import Path

import pytest

from . import extraction_parity as ep

_XFAIL = Path(__file__).with_name("extraction_xfail.toml")


def _xfail_surfaces(model: str) -> dict[str, str]:
    """surface → issue for registered divergences on this model (strict: a
    registered surface that comes back clean FAILS — remove the entry)."""
    if not _XFAIL.exists():
        return {}
    reg = tomllib.loads(_XFAIL.read_text())
    return {
        e["surface"]: e["issue"]
        for e in reg.get("xfail", [])
        if e["model"] == model
    }


def _corpus_paths() -> list[Path]:
    raw = os.environ.get("IFCFAST_CORPUS", "")
    return [Path(p) for p in raw.split(":") if p.strip()]


@pytest.mark.skipif(
    not _corpus_paths(),
    reason="set IFCFAST_CORPUS=/a.ifc:/b.ifc to run the real-file gate",
)
@pytest.mark.parametrize("path", _corpus_paths(), ids=lambda p: p.name)
def test_extraction_matches_ifcopenshell(path):
    assert path.exists(), f"corpus file missing: {path}"
    rep = ep.run_model(path, sample=200)
    print(json.dumps(rep, indent=1, default=str))
    # anti-vacuous: something was compared on this model
    compared = (
        rep["classifications"]["matched_refs"]
        + rep["materials"]["matched_rows"]
        + rep["psets"]["properties_compared"]
    )
    assert compared > 0, f"{path.name}: nothing compared"
    status = ep.surface_status(rep)
    xfail = _xfail_surfaces(path.name)
    stale = [f"{s} ({xfail[s]})" for s in xfail if status.get(s, True)]
    assert not stale, f"{path.name}: registered divergence now clean — remove from extraction_xfail.toml: {stale}"
    dirty = {s: rep[s] for s, ok in status.items() if not ok and s not in xfail}
    assert not dirty, json.dumps(dirty, default=str)
