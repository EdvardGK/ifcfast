"""GH #179 — the three mesh entry points, and which frame each speaks.

`m.meshes()`, `m.iter_meshes()` and `m.mesh(guid)` return the same
geometry in two different frames: the batch paths subtract a model-wide
`global_shift` so far-from-origin geometry survives `float32`, the
single-product path returns absolute `float64`. That is by design — but
`iter_meshes()` used to be a bare generator, so the shift it had applied
was unreachable. Streaming a georeferenced model then yielded
plausible-looking coordinates that were wrong by exactly the
georeference offset: the silent kind of wrong.

Everything here runs on a FAR-ORIGIN fixture. On a near-origin model the
shift is `[0, 0, 0]` and all three paths agree, so a near-origin fixture
cannot catch a lost shift — `test_near_origin_shift_is_zero` pins that
control case explicitly.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import ifcfast
from ifcfast.model import LocalMesh, Mesh, MeshIter


FIXTURES = Path(__file__).parent / "fixtures"
FAR = FIXTURES / "far_origin_box.ifc"
NEAR = FIXTURES / "geom_box.ifc"

#: The one product in `far_origin_box.ifc`, placed at
#: (213000.01, 6720000.32, 120.0) m and extruded 4 m up.
BOX = "0F341KN2n67eWRM6f6jhL0"
#: Storey elevation in the same fixture, metres.
STOREY_Z = 120.0


@pytest.fixture
def far():
    return ifcfast.open(FAR, use_cache=False, write_cache=False)


def test_far_origin_fixture_actually_shifts(far):
    """Guard on the guard: if this fixture ever stops triggering a
    non-zero shift, every other assertion here becomes vacuous."""
    ms = far.meshes()
    assert ms.global_shift != [0.0, 0.0, 0.0]
    assert ms.global_shift == pytest.approx([213000.0, 6720000.0, 120.0])


def test_iter_meshes_exposes_global_shift(far):
    """The defect: the streaming path carried no route to the shift."""
    it = far.iter_meshes()
    assert isinstance(it, MeshIter)
    assert it.global_shift == far.meshes().global_shift
    assert it.frame == "world"
    assert it.unit == "m"
    assert len(it) == 1


def test_iter_meshes_still_iterates_like_a_generator(far):
    """`for mesh in m.iter_meshes():` must keep working unchanged."""
    rows = []
    for mesh in far.iter_meshes():
        rows.append(mesh)
    assert len(rows) == 1
    assert isinstance(rows[0], Mesh)
    assert rows[0].guid == BOX
    # Strictly more capable than the old generator: re-iterable.
    it = far.iter_meshes()
    assert [m.guid for m in it] == [m.guid for m in it] == [BOX]


def test_iter_meshes_plus_shift_equals_single_getter(far):
    """`vertices + global_shift` from the ITERATOR must reproduce the
    absolute f64 coordinates `m.mesh(guid)` returns."""
    np = pytest.importorskip("numpy")

    it = far.iter_meshes()
    shift = np.asarray(it.global_shift, dtype=np.float64)
    streamed = {m.guid: m for m in it}

    one = far.mesh(BOX)
    assert one is not None
    assert one.vertices.dtype == np.float64

    absolute = streamed[BOX].vertices.astype(np.float64) + shift
    assert absolute.shape == one.vertices.shape
    # f32 batch frame vs f64 single getter — compare sorted per axis, the
    # same way test_smoke compares meshes() against mesh().
    delta = np.abs(np.sort(absolute, 0) - np.sort(one.vertices, 0)).max()
    assert delta < 1e-3, f"iterator+shift is {delta} m off the absolute mesh"


def test_streamed_zmin_in_world_frame_is_the_storey_elevation(far):
    """The check the missing shift blocked: an element's mesh bottom in
    WORLD Z, compared against the elevation of the storey containing it.

    Shifted, the box bottoms out at z=0 and looks like it sits on a
    ground floor 120 m below where it really is."""
    np = pytest.importorskip("numpy")

    it = far.iter_meshes()
    mesh = next(iter(it))

    shifted_zmin = float(mesh.vertices[:, 2].min())
    assert shifted_zmin == pytest.approx(0.0, abs=1e-6), (
        "fixture no longer exercises the shift"
    )

    world_zmin = shifted_zmin + it.global_shift[2]
    assert world_zmin == pytest.approx(STOREY_Z, abs=1e-3)

    storey = far.storeys[0]
    assert storey.elevation_m == pytest.approx(STOREY_Z)
    assert abs(world_zmin - storey.elevation_m) < 1e-3


def test_near_origin_shift_is_zero():
    """Control: the same assertions on a near-origin model pass even
    with a lost shift, which is why the fixture above has to be far."""
    m = ifcfast.open(NEAR, use_cache=False, write_cache=False)
    it = m.iter_meshes()
    assert it.global_shift == [0.0, 0.0, 0.0]
    assert m.meshes().global_shift == [0.0, 0.0, 0.0]


def test_iter_meshes_unit_is_reported_and_shift_scales(far):
    """`unit=` scales vertices AND the shift; the iterator says which."""
    it = far.iter_meshes(unit="mm")
    assert it.unit == "mm"
    assert it.global_shift == pytest.approx(
        [213000.0 * 1000, 6720000.0 * 1000, 120.0 * 1000]
    )


def test_iter_meshes_local_frame_pins_zero_shift(far):
    """`frame="local"` yields LocalMesh rows in native units — no metre
    scaling and, by contract, no shift."""
    it = far.iter_meshes(frame="local")
    assert it.frame == "local"
    assert it.global_shift == [0.0, 0.0, 0.0]
    rows = list(it)
    assert len(rows) == 1
    assert isinstance(rows[0], LocalMesh)


def test_iter_meshes_carries_the_pass_stats(far):
    """Parity with MeshList: the GH #166 counters stay reachable."""
    it = far.iter_meshes()
    assert it.stats == far.meshes().stats
    assert it.stats["products_meshed"] == 1
