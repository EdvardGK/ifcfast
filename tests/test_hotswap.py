"""Agent-facing ``Model.hotswap()`` — the mesh-swap write primitive (GH #124
Phase 3).

Exercises the Python surface: local-frame mesh in, a repointed ``Body``
representation out, orphan GC of the geometry the old body uniquely owned,
and fail-loud on bad input. The real proof — that the emitted file reopens
in ifcopenshell with the element's body now an ``IfcTriangulatedFaceSet`` —
runs over the discipline-diverse G55 corpus when ``IFCFAST_SUBSET_CORPUS``
is set (same corpus var the subset gate uses).
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

import ifcfast

FIXTURE = Path(__file__).parent / "fixtures" / "hotswap_body.ifc"
WALL1 = "7XvctVUKr0kugbFTf53O9L"


def _unit_cube():
    v = [
        [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
    ]
    t = [
        [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
        [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
        [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7],
    ]
    return v, t


@pytest.fixture(scope="module")
def model():
    return ifcfast.open(FIXTURE, use_cache=False, write_cache=False)


def test_hotswap_returns_bytes_with_faceset(model):
    v, t = _unit_cube()
    data = model.hotswap(WALL1, v, t)
    assert isinstance(data, (bytes, bytearray))
    assert b"IFCTRIANGULATEDFACESET" in data
    assert b"IFCCARTESIANPOINTLIST3D" in data
    assert b"Tessellation" in data


def test_hotswap_stats_and_gc(model, tmp_path):
    v, t = _unit_cube()
    out = tmp_path / "swapped.ifc"
    stats = model.hotswap(WALL1, v, t, out_path=str(out))
    assert stats["product"] == 30
    assert stats["shape_rep"] == 41
    # Wall1 uniquely owned its mapped item + mapping target: exactly 2 GC'd.
    assert stats["records_gc"] == 2
    assert stats["path"] == str(out)
    assert out.exists()
    # The shared map's swept solid text survives (wall2 still maps to it).
    assert b"IFCEXTRUDEDAREASOLID" in out.read_bytes()


def test_hotswap_unknown_guid_is_loud(model):
    v, t = _unit_cube()
    with pytest.raises(ValueError):
        model.hotswap("THISGUIDDOESNOTEXIST00", v, t)


def test_hotswap_empty_mesh_is_loud(model):
    with pytest.raises(ValueError):
        model.hotswap(WALL1, [], [])


def test_hotswap_out_of_range_triangle_is_loud(model):
    with pytest.raises(ValueError):
        model.hotswap(WALL1, [[0.0, 0.0, 0.0]], [[0, 1, 2]])


ifcopenshell = pytest.importorskip("ifcopenshell")


def test_hotswap_fixture_reopens_clean_in_ifcopenshell(model, tmp_path):
    v, t = _unit_cube()
    out = tmp_path / "swapped_oracle.ifc"
    model.hotswap(WALL1, v, t, out_path=str(out))
    f = ifcopenshell.open(str(out))

    dangling = 0
    for inst in f:
        try:
            inst.get_info(recursive=False)
        except Exception:  # noqa: BLE001
            dangling += 1
    assert dangling == 0

    # Wall1's body representation is now a triangulated face set.
    wall = next(w for w in f.by_type("IfcWall") if w.GlobalId == WALL1)
    body = next(
        r for r in wall.Representation.Representations
        if r.RepresentationIdentifier == "Body"
    )
    assert body.RepresentationType == "Tessellation"
    assert body.Items[0].is_a("IfcTriangulatedFaceSet")
    assert body.Items[0].Coordinates.is_a("IfcCartesianPointList3D")


def _corpus_paths() -> list[Path]:
    raw = os.environ.get("IFCFAST_SUBSET_CORPUS", "")
    return [Path(p) for p in raw.split(":") if p.strip()]


@pytest.mark.skipif(
    not _corpus_paths(),
    reason="set IFCFAST_SUBSET_CORPUS=/a.ifc:/b.ifc to run the real-file gate",
)
@pytest.mark.parametrize("path", _corpus_paths(), ids=lambda p: p.name)
def test_hotswap_over_real_corpus_is_ifcopenshell_clean(path, tmp_path):
    """Durable acceptance gate for GH #124 Phase 3: on a real element with a
    ``Body`` representation, swap in a triangle mesh and prove the emitted
    file reopens in ifcopenshell with zero dangling refs and the element's
    body now an ``IfcTriangulatedFaceSet`` — over Revit/MagiCAD output the
    synthetic fixture can't represent.
    """
    assert path.exists(), f"corpus file missing: {path}"
    src = ifcopenshell.open(str(path))

    target = None
    for prod in src.by_type("IfcProduct"):
        rep = getattr(prod, "Representation", None)
        if rep is None or not getattr(prod, "GlobalId", None):
            continue
        if any(
            getattr(r, "RepresentationIdentifier", None) == "Body"
            for r in rep.Representations
        ):
            target = prod
            break
    if target is None:
        pytest.skip(f"{path.name}: no product with a Body representation")

    guid = target.GlobalId
    v, t = _unit_cube()
    model = ifcfast.open(path, use_cache=False, write_cache=False)
    out = tmp_path / f"{path.stem}.hotswap.ifc"
    stats = model.hotswap(guid, v, t, out_path=str(out))

    f = ifcopenshell.open(str(out))
    dangling = 0
    for inst in f:
        try:
            inst.get_info(recursive=False)
        except Exception:  # noqa: BLE001
            dangling += 1
    assert dangling == 0, f"{path.name}: {dangling} dangling after hotswap"

    prod = next(p for p in f.by_type("IfcProduct") if getattr(p, "GlobalId", None) == guid)
    body = next(
        r for r in prod.Representation.Representations
        if r.RepresentationIdentifier == "Body"
    )
    # Geometry dialect follows the file's schema.
    if str(f.schema).upper().startswith("IFC4"):
        assert body.RepresentationType == "Tessellation"
        assert body.Items[0].is_a("IfcTriangulatedFaceSet")
    else:
        assert body.RepresentationType == "SurfaceModel"
        assert body.Items[0].is_a("IfcShellBasedSurfaceModel")
    print(
        f"OK {path.name}: swapped {prod.is_a()} {guid}, "
        f"GC'd {stats['records_gc']} records, {stats['records_out']} out"
    )
