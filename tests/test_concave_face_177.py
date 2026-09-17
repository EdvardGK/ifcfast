"""GH #177 — concave, hole-free faces must not be fan-bridged.

A brep face without declared holes used to be fan-triangulated from
vertex 0. That is exact only for convex loops: a concave outline
(L / C / U footprint, notched plate) gets triangles spanning the notch,
so the mesh over-fills the opening and the closed shell over-reports
both surface area and volume — silently, with the result still
classified reliable.

``brep_concave_l.ifc`` is the minimal shape that exposes it: an
``IfcFacetedBrep`` L-prism, footprint (0,0) (10,0) (10,4) (4,4) (4,10)
(0,10) extruded 2 m, authored as eight ``IfcFace`` /
``IfcFaceOuterBound`` / ``IfcPolyLoop`` faces.

Analytic truth (confirmed against ifcopenshell on this fixture):

* volume       = 64 m2 footprint x 2 m           = 128 m3
* surface area = 2 x 64 caps + 40 m perimeter x 2 = 208 m2
* triangles    = 2 concave caps x 4 + 6 quad sides x 2 = 20

The caps are the concave faces. A fan started at the wrong cap vertex
inflates the area past 208; the volume is the shell's, so it moves too.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import ifcfast


FIXTURE = Path(__file__).parent / "fixtures" / "brep_concave_l.ifc"

EXPECTED_VOLUME_M3 = 128.0
EXPECTED_AREA_M2 = 208.0
EXPECTED_TRIANGLES = 20


@pytest.fixture(scope="module")
def model():
    return ifcfast.open(FIXTURE, use_cache=False, write_cache=False)


def test_concave_l_prism_mesh_area_and_triangle_count(model):
    """Area computed straight off the emitted triangles — the ground
    truth the fan path cannot reach."""
    np = pytest.importorskip("numpy")

    meshes = model.meshes()
    assert len(meshes) == 1, f"expected one meshed product, got {len(meshes)}"

    # Single-GUID getter: float64 in absolute metres, so the
    # divergence-theorem volume below needs no shift bookkeeping.
    mesh = model.mesh(meshes[0].guid)
    assert mesh is not None

    v = np.asarray(mesh.vertices, dtype=np.float64)
    f = np.asarray(mesh.faces, dtype=np.int64).reshape(-1, 3)

    assert len(f) == EXPECTED_TRIANGLES, (
        f"expected {EXPECTED_TRIANGLES} triangles "
        f"(2 concave caps x 4 + 6 quad sides x 2), got {len(f)}"
    )

    a, b, c = v[f[:, 0]], v[f[:, 1]], v[f[:, 2]]
    area = 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1).sum()
    assert area == pytest.approx(EXPECTED_AREA_M2, rel=1e-3), (
        f"surface area {area:.4f} m2 — expected {EXPECTED_AREA_M2} "
        "(a fan bridging the L's notch over-fills it)"
    )

    # Divergence-theorem volume off the same triangles. Outward-wound
    # closed shell -> positive.
    volume = (np.einsum("ij,ij->i", a, np.cross(b, c)) / 6.0).sum()
    assert volume == pytest.approx(EXPECTED_VOLUME_M3, rel=1e-3), (
        f"shell volume {volume:.4f} m3 — expected {EXPECTED_VOLUME_M3}"
    )


def test_concave_l_prism_mesh_qto(model):
    """The same numbers through the public QTO surface, and the result
    must still be flagged reliable (a closed shell that is now also
    geometrically right)."""
    pytest.importorskip("pandas")

    products, _surfaces = model.mesh_qto()
    assert len(products) == 1
    row = products.iloc[0]

    assert float(row.volume_m3) == pytest.approx(EXPECTED_VOLUME_M3, rel=1e-3), (
        f"volume_m3 {row.volume_m3} — expected {EXPECTED_VOLUME_M3}"
    )
    assert float(row.surface_area_m2) == pytest.approx(EXPECTED_AREA_M2, rel=1e-3), (
        f"surface_area_m2 {row.surface_area_m2} — expected {EXPECTED_AREA_M2}"
    )
    assert row.mesh_quality == "closed"
