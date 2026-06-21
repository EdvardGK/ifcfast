"""Geometry-QTO oracle test (GH #59 M1 / GH #58).

The automated form of the G55 volume differential: drives
:func:`tests.oracle._geom_adapter.diff_geometry_volumes` over a committed
tiny fixture with real meshable geometry, then asserts ifcfast's
``mesh_qto`` volume agrees with ifcopenshell's kernel mesh volume per
element — through the same :class:`tests.oracle.report.Collector` the
quantities / psets / materials oracle tests use.

The reusable ``diff_geometry_volumes`` adapter can be pointed at
``G55_RIB.ifc`` by hand to reproduce the differential at scale; this test
guards the small, committed case so a volume regression fails CI.

``ifcopenshell`` is imported *inside* the adapter (not at module scope),
so this file collects-and-skips cleanly under the plain ``pytest -q`` run
when the dev-only oracle extra is absent (via the conftest gate).
"""

from __future__ import annotations

from pathlib import Path

from . import _geom_adapter as geom
from .report import Collector

SURFACE = "geometry"

# A committed tiny fixture with one IfcBuildingElementProxy whose body is a
# single IfcExtrudedAreaSolid (2 x 3 rectangle extruded 4) -> 24 m^3 exactly.
# None of the other tiny fixtures (minimal/quantities/materials/aggregate_part)
# carry a meshable solid, so this one was created for the geometry gate.
FIXTURE = "geom_box.ifc"
FIXTURES_DIR = Path(__file__).resolve().parents[1] / "fixtures"
FIXTURE_PATH = FIXTURES_DIR / FIXTURE


def test_geometry_volumes_match_ifcopenshell():
    """Differential gate: ifcfast mesh_qto volume == ifcopenshell kernel volume.

    Runs the reusable adapter, funnels its
    :class:`~tests.oracle.report.DisagreementRecord`s into a
    :class:`~tests.oracle.report.Collector`, and asserts no blocking
    record. Any disagreement is ``unknown`` (hence blocking) until a human
    triages it ``ifcfast_bug`` vs ``ifcopenshell_quirk``.
    """
    if not FIXTURE_PATH.exists():  # pragma: no cover - committed fixture
        import pytest

        pytest.skip(f"fixture {FIXTURE!r} not committed at {FIXTURE_PATH}")

    # --- anti-vacuous-green guard -------------------------------------------
    # An adapter that fails to mesh anything (wrong column, empty fixture, a
    # kernel that swallowed every element) yields zero elements -> zero
    # disagreements -> a green pass that compared NOTHING. Assert both sides
    # actually produced a volume for at least one shared element before we
    # trust an empty diff list.
    fast = geom.fast_volumes_by_guid(FIXTURE_PATH)
    truth = geom.ios_volumes_by_guid(FIXTURE_PATH)
    shared = set(fast) & set(truth)
    assert fast, f"ifcfast mesh_qto produced no volumes from {FIXTURE}"
    assert truth, f"ifcopenshell meshed no elements from {FIXTURE}"
    assert len(shared) >= 1, (
        f"no element was compared on both sides for {FIXTURE}: "
        f"fast_guids={sorted(fast)} ios_guids={sorted(truth)}"
    )
    print(
        f"[oracle:{SURFACE}] compared {len(shared)} element(s) "
        f"in {FIXTURE} (fast={len(fast)}, ios={len(truth)})"
    )

    records = geom.diff_geometry_volumes(FIXTURE_PATH)

    collector = Collector()
    for rec in records:
        collector.record(rec)
    collector.assert_clean()


def test_signed_mesh_volume_unit_cube():
    """Guard the volume integrator itself on a hand-built unit cube.

    Independent of any kernel: a closed, outward-oriented triangulation of
    the unit cube must integrate to 1.0 via the signed-tetra sum, so a
    regression in :func:`_geom_adapter._signed_mesh_volume` (a sign slip, a
    bad stride) fails here without needing ifcopenshell or ifcfast.
    """
    # 8 corners of the unit cube.
    verts = [
        0.0, 0.0, 0.0,  # 0
        1.0, 0.0, 0.0,  # 1
        1.0, 1.0, 0.0,  # 2
        0.0, 1.0, 0.0,  # 3
        0.0, 0.0, 1.0,  # 4
        1.0, 0.0, 1.0,  # 5
        1.0, 1.0, 1.0,  # 6
        0.0, 1.0, 1.0,  # 7
    ]
    # 12 outward-facing (CCW seen from outside) triangles.
    faces = [
        0, 2, 1,  0, 3, 2,   # bottom (z=0), normal -z
        4, 5, 6,  4, 6, 7,   # top    (z=1), normal +z
        0, 1, 5,  0, 5, 4,   # front  (y=0), normal -y
        2, 3, 7,  2, 7, 6,   # back   (y=1), normal +y
        1, 2, 6,  1, 6, 5,   # right  (x=1), normal +x
        0, 4, 7,  0, 7, 3,   # left   (x=0), normal -x
    ]
    vol = geom._signed_mesh_volume(verts, faces)
    assert abs(vol - 1.0) < 1e-12, f"unit cube volume = {vol!r}, expected 1.0"
