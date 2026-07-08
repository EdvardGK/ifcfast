"""Mesh round-trip fidelity gate: parse mesh → rebuild IFC → re-parse.

Self-referential (ifcfast vs ifcfast, no ifcopenshell) so it runs in the
main ``pytest -q``. Complements two existing gates without duplicating
them:

* ``doc_roundtrip.rs`` — byte-identity at the **STEP-record** level
  (geometry untouched).
* ``test_geometry_oracle`` — mesh **volume** vs ifcopenshell (a different
  kernel), one axis only.

This one closes the gap between them: it drives the *mesh* through the
writer (``m.hotswap``) and re-parses, gating vertex geometry, triangle
**connectivity**, and **signed volume / winding** — the axes a
vertex-cloud-only comparison (GH #127 ``test_mesh_local_frame``) leaves
open.

The reusable sweep lives in ``tests/oracle/mesh_roundtrip.py`` (also a
CLI: ``python -m tests.oracle.mesh_roundtrip MODEL.ifc``).
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

import ifcfast

# Import the adapter robustly: `tests` is not a package (and a stray
# editable install can shadow it — see next-steps), so fall back to a
# direct path insert on the oracle dir.
try:  # pragma: no cover - import-path robustness
    from tests.oracle import mesh_roundtrip as mrt
except Exception:  # pragma: no cover
    import sys

    sys.path.insert(0, str(Path(__file__).parent / "oracle"))
    import mesh_roundtrip as mrt  # type: ignore

FIXTURES = Path(__file__).parent / "fixtures"

# Committed fixtures spanning both serialisation dialects. hotswap writes
# an IfcTriangulatedFaceSet on IFC4 (point list verbatim → byte-identical
# local round-trip) and an IfcFaceBasedSurfaceModel on IFC2x3 (per-face
# re-tessellation → same point SET, may reorder → not byte-identical).
IFC4_FIXTURES = ["hotswap_body.ifc", "hotswap_roundtrip.ifc"]
IFC2X3_FIXTURES = ["hotswap_body_2x3.ifc"]


def _open(name: str):
    p = FIXTURES / name
    if not p.exists():  # pragma: no cover - committed fixture
        pytest.skip(f"fixture {name!r} not committed")
    return ifcfast.open(p, use_cache=False, write_cache=False)


def _all_guids(model):
    return [mm.guid for mm in model.meshes(frame="local") if len(mm.faces)]


# --------------------------------------------------------------------------
# IFC4: the strong invariant — byte-identical local mesh across the trip
# --------------------------------------------------------------------------


@pytest.mark.parametrize("name", IFC4_FIXTURES)
def test_ifc4_local_roundtrip_is_byte_identical(name, tmp_path):
    model = _open(name)
    assert model.schema.startswith("IFC4")
    guids = _all_guids(model)
    assert guids, f"{name}: no meshed product"
    for guid in guids:
        rec = mrt.roundtrip_element(model, guid, tmp_path, cycles=2)
        assert rec is not None and "skip_reason" not in rec, rec
        for i, cyc in enumerate(rec["cycles"]):
            # verts AND faces bit-identical — the honest "byte-identical mesh".
            assert cyc["exact_array"], f"{name}:{guid} cycle {i} not byte-identical: {cyc}"
            assert cyc["faceset_equal"]
            assert not cyc["winding_flip"]
            assert cyc["dvol_rel"] == 0.0


# --------------------------------------------------------------------------
# IFC2x3: re-tessellation reorders vertices — geometry/topology/volume
# must still be preserved exactly, just not the array order.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("name", IFC2X3_FIXTURES)
def test_ifc2x3_local_roundtrip_preserves_geometry(name, tmp_path):
    model = _open(name)
    assert model.schema.startswith("IFC2X3")
    guids = _all_guids(model)
    assert guids, f"{name}: no meshed product"
    for guid in guids:
        rec = mrt.roundtrip_element(model, guid, tmp_path, cycles=2)
        assert rec is not None and "skip_reason" not in rec, rec
        for i, cyc in enumerate(rec["cycles"]):
            # Same point set (reorder-robust), same connectivity, same volume.
            assert cyc["ok"], f"{name}:{guid} c{i} failed round-trip: {cyc}"
            assert cyc["hausdorff_ok"], f"{name}:{guid} c{i}: {cyc}"
            assert cyc["faceset_equal"], f"{name}:{guid} c{i} topology changed: {cyc}"
            assert cyc["volume_ok"]
            assert not cyc["winding_flip"]


# --------------------------------------------------------------------------
# Regression detectors: prove the gate FAILS on the mutations it claims to
# catch (a vertex-cloud-only check would pass these).
# --------------------------------------------------------------------------


def test_winding_flip_is_detected():
    model = _open("hotswap_body.ifc")
    guid = _all_guids(model)[0]
    v, f = mrt.local_mesh(model, guid)
    flipped = f[:, ::-1].copy()  # reverse each triangle's winding
    cyc = mrt.compare_meshes(v, f, v, flipped)
    assert cyc["winding_flip"]
    assert cyc["faceset_equal"]  # cloud + connectivity unchanged — only winding


def test_topology_change_is_detected():
    model = _open("hotswap_body.ifc")
    guid = _all_guids(model)[0]
    v, f = mrt.local_mesh(model, guid)
    dropped = f[:-1]  # remove one triangle
    cyc = mrt.compare_meshes(v, f, v, dropped)
    assert not cyc["faceset_equal"]
    assert cyc["faces_lost"] == 1


# --------------------------------------------------------------------------
# Real-corpus gate (same env var as the subset / hotswap / local-frame gates)
# --------------------------------------------------------------------------


def _corpus_paths() -> list[Path]:
    raw = os.environ.get("IFCFAST_CORPUS", "") or os.environ.get(
        "IFCFAST_SUBSET_CORPUS", ""
    )
    return [Path(p) for p in raw.split(":") if p.strip()]


@pytest.mark.skipif(
    not _corpus_paths(),
    reason="set IFCFAST_CORPUS=/a.ifc:/b.ifc to run the real-file mesh round-trip gate",
)
@pytest.mark.parametrize("path", _corpus_paths(), ids=lambda p: p.name)
def test_mesh_roundtrip_over_real_corpus(path):
    assert path.exists(), f"corpus file missing: {path}"
    # Size-band keeps the O(N·M) Hausdorff cheap; 40 elements is a broad
    # enough sample to catch a systematic extractor/writer regression.
    recs = mrt.sweep(path, limit=40, cycles=1, face_max=4000, vert_max=3000)
    swept = [r for r in recs if "skip_reason" not in r]
    assert swept, f"{path.name}: no element survived to a round-trip"
    bad = mrt.regressions(recs)
    assert not bad, (
        f"{path.name}: {len(bad)} element(s) failed mesh round-trip; "
        f"first: {bad[0]['guid']} {bad[0]['cycles'][0]}"
    )
