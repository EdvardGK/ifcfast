"""Local-frame mesh extraction + placement matrix — the hotswap bridge
(GH #127).

``m.hotswap`` writes vertices verbatim into the element's ``Body``
representation, i.e. in the representation-item frame, native units.
``m.mesh(guid, frame="local")`` / ``m.meshes(frame="local")`` return
exactly that frame, so extract → (decimate) → hotswap round-trips without
double-applying the ``ObjectPlacement``.

The acceptance criterion here is the round-trip itself: extract local,
hotswap it back **unchanged**, reopen the output, extract world — the
world geometry must match the original within float tolerance. The
synthetic fixture places two walls under loud non-identity placements
(translation + 90° rotation; a mapped item with a LocalOrigin offset);
the same proof runs over the discipline-diverse G55 corpus when
``IFCFAST_SUBSET_CORPUS`` is set (same env var as the subset/hotswap
gates).
"""

from __future__ import annotations

import os
from pathlib import Path

import numpy as np
import pytest

import ifcfast

FIXTURE = Path(__file__).parent / "fixtures" / "hotswap_roundtrip.ifc"
WALL_DIRECT = "0LocalFrameWallA000001"  # extrusion, translated + rotated
WALL_MAPPED = "0LocalFrameWallB000001"  # mapped item, LocalOrigin offset


@pytest.fixture(scope="module")
def model():
    return ifcfast.open(FIXTURE, use_cache=False, write_cache=False)


def _directed_max_min_dist(a: np.ndarray, b: np.ndarray) -> float:
    """max over a of (min distance to b) — chunked so RAM stays bounded."""
    worst = 0.0
    for i in range(0, len(a), 256):
        chunk = a[i : i + 256]
        d2 = ((chunk[:, None, :] - b[None, :, :]) ** 2).sum(axis=2)
        worst = max(worst, float(np.sqrt(d2.min(axis=1)).max()))
    return worst


def _assert_same_vertex_set(a: np.ndarray, b: np.ndarray, tol: float) -> None:
    """Vertex-set equality within ``tol``, robust to reordering: every
    vertex of each cloud has a counterpart in the other within ``tol``."""
    assert a.ndim == 2 and a.shape[1] == 3
    assert b.ndim == 2 and b.shape[1] == 3
    ab = _directed_max_min_dist(a, b)
    ba = _directed_max_min_dist(b, a)
    assert max(ab, ba) <= tol, f"vertex sets differ: a→b {ab:.6g}, b→a {ba:.6g} > {tol}"


# --------------------------------------------------------------------------
# Frame semantics on the synthetic fixture
# --------------------------------------------------------------------------


@pytest.mark.parametrize("guid", [WALL_DIRECT, WALL_MAPPED])
def test_local_frame_differs_from_world(model, guid):
    w = model.mesh(guid)
    lm = model.mesh(guid, frame="local")
    assert w is not None and lm is not None
    assert lm.vertices.shape == w.vertices.shape
    assert np.array_equal(lm.faces, w.faces)
    # The placements are metres-scale translations — local and world must
    # be loudly different (this is the double-placement trap).
    # World is metres; local is native mm. Compare in native units.
    w_native = w.vertices / model.unit_scale
    delta = np.abs(w_native - lm.vertices).max()
    assert delta > 1000.0, f"placement did not separate frames (max delta {delta})"


@pytest.mark.parametrize("guid", [WALL_DIRECT, WALL_MAPPED])
def test_placement_maps_local_to_world(model, guid):
    w = model.mesh(guid)
    lm = model.mesh(guid, frame="local")
    assert lm.placement.shape == (4, 4)
    assert np.allclose(lm.placement[3], [0.0, 0.0, 0.0, 1.0])
    # world_metres = (placement @ [local, 1]) * unit_scale, per vertex
    # (same tessellation order on both extractions).
    ones = np.ones((len(lm.vertices), 1))
    homo = np.hstack([lm.vertices, ones])
    mapped = (homo @ lm.placement.T)[:, :3] * model.unit_scale
    assert np.abs(mapped - w.vertices).max() <= 1e-4  # 0.1 mm


def test_batch_local_matches_single(model):
    ms = model.meshes(frame="local")
    assert ms.global_shift == [0.0, 0.0, 0.0]
    by_guid = {m_.guid: m_ for m_ in ms}
    for guid in (WALL_DIRECT, WALL_MAPPED):
        batch = by_guid[guid]
        single = model.mesh(guid, frame="local")
        assert batch.placement.shape == (4, 4)
        assert np.allclose(batch.placement, single.placement, atol=1e-9)
        _assert_same_vertex_set(
            batch.vertices.astype(np.float64), single.vertices, tol=1e-3
        )


# --------------------------------------------------------------------------
# THE acceptance criterion: identity round-trip through hotswap
# --------------------------------------------------------------------------


@pytest.mark.parametrize("guid", [WALL_DIRECT, WALL_MAPPED])
def test_roundtrip_local_extract_hotswap_reproduces_world(model, guid, tmp_path):
    """extract local → hotswap unchanged → reopen → extract world must
    reproduce the original world mesh (the element must NOT move)."""
    w0 = model.mesh(guid)
    lm = model.mesh(guid, frame="local")

    out = tmp_path / f"{guid}.roundtrip.ifc"
    model.hotswap(guid, lm.vertices, lm.faces, out_path=str(out))

    m2 = ifcfast.open(out, use_cache=False, write_cache=False)
    w1 = m2.mesh(guid)
    assert w1 is not None, "hotswapped element lost its geometry"
    # IFC4 keeps the point list verbatim in the new faceset.
    assert w1.vertices.shape == w0.vertices.shape
    assert w1.faces.shape == w0.faces.shape
    _assert_same_vertex_set(w1.vertices, w0.vertices, tol=1e-4)  # 0.1 mm


# --------------------------------------------------------------------------
# Fail-loud contract
# --------------------------------------------------------------------------


def test_unknown_frame_is_loud(model):
    with pytest.raises(ValueError):
        model.mesh(WALL_DIRECT, frame="banana")
    with pytest.raises(ValueError):
        model.meshes(frame="banana")


def test_local_frame_rejects_cut_openings(model):
    with pytest.raises(ValueError):
        model.mesh(WALL_DIRECT, frame="local", cut_openings=True)
    with pytest.raises(ValueError):
        model.meshes(frame="local", cut_openings=True)


def test_local_frame_rejects_unit_conversion(model):
    with pytest.raises(ValueError):
        model.mesh(WALL_DIRECT, frame="local", unit="mm")
    with pytest.raises(ValueError):
        model.meshes(frame="local", unit="mm")


# --------------------------------------------------------------------------
# Real-corpus gate (same env var as the subset / hotswap gates)
# --------------------------------------------------------------------------


def _corpus_paths() -> list[Path]:
    raw = os.environ.get("IFCFAST_CORPUS", "") or os.environ.get(
        "IFCFAST_SUBSET_CORPUS", ""
    )
    return [Path(p) for p in raw.split(":") if p.strip()]


@pytest.mark.skipif(
    not _corpus_paths(),
    reason="set IFCFAST_CORPUS=/a.ifc:/b.ifc to run the real-file gate",
)
@pytest.mark.parametrize("path", _corpus_paths(), ids=lambda p: p.name)
def test_local_roundtrip_over_real_corpus(path, tmp_path):
    """GH #127 acceptance over Revit/MagiCAD output: on a real element,
    local-extract → identity hotswap → reopen → world-extract must match
    the original world mesh. Covers both dialects (IFC4 faceset and
    IFC2x3 surface model — the 2x3 re-tessellation may reorder/duplicate
    vertices, hence the set-based comparison)."""
    assert path.exists(), f"corpus file missing: {path}"
    m = ifcfast.open(path, use_cache=False, write_cache=False)

    # Modest-sized candidates keep the O(N^2) set comparison cheap.
    candidates = [
        m_.guid
        for m_ in m.meshes()
        if 4 <= len(m_.faces) <= 1000 and len(m_.vertices) <= 800
    ][:20]
    if not candidates:
        pytest.skip(f"{path.name}: no modest-sized meshed product")

    swapped = 0
    for guid in candidates:
        w0 = m.mesh(guid)
        lm = m.mesh(guid, frame="local")
        if w0 is None or lm is None:
            continue
        out = tmp_path / f"{path.stem}.{swapped}.roundtrip.ifc"
        try:
            m.hotswap(guid, lm.vertices, lm.faces, out_path=str(out))
        except ValueError:
            # e.g. no 'Body' shape representation — try the next one.
            continue

        m2 = ifcfast.open(out, use_cache=False, write_cache=False)
        w1 = m2.mesh(guid)
        assert w1 is not None, f"{path.name}: {guid} lost geometry after hotswap"
        extent = float(np.abs(w0.vertices).max())
        tol = max(1e-3, extent * 1e-6)  # 1 mm floor, georef-scaled slack
        _assert_same_vertex_set(w1.vertices, w0.vertices, tol=tol)
        swapped += 1
        if swapped >= 3:
            break

    assert swapped > 0, f"{path.name}: no candidate survived to a round-trip"
