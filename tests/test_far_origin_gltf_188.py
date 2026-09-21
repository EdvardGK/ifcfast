"""GH #188 — `m.to_gltf()` on a far-origin, millimetre-unit model.

`BakeFrame::World` casts vertices to `float32` at the *absolute*
magnitude. On a Norwegian NTM site placement authored in millimetres
(x ~ 9.2e7, y ~ 1.25e9) the f32 ulp there is 8 mm in X and 128 mm in Y,
so a Ø400 duct wall — 2 mm thick — lands on a lattice coarser than
itself: the circle wobbles ±4 mm and half its faces collapse to zero
area. The glTF writer used to take exactly that frame.

These tests hold both writer paths to the fix:

* the **baked** path (`cut_openings=True` ⇒ instancing off), whose
  vertices arrive already shifted from the sink, and
* the **instanced** path (`cut_openings=False` ⇒ instancing on), whose
  per-instance TRS must come from the f64 `mesh_anchor` rather than the
  f32 `world_transform`, and must carry the model's unit scale because
  the shared mesh it points at is still in native units.

A near-origin fixture cannot catch any of this: there `global_shift` is
`[0, 0, 0]` and every frame agrees — which is why `geom_box.ifc` is
checked for *unchanged* output while `far_origin_box.ifc` is checked for
a reported shift.
"""

from __future__ import annotations

import json
import struct

import numpy as np
import pytest

import ifcfast

FIXTURES = "tests/fixtures"

# The fixture's placement chain, in metres: site (92200000, 1247000000,
# 0) mm + duct (319899.3, 315537.2, 127250) mm, duct B 2000 mm east.
DUCT_AXIS = {
    "1VluSltEX0WftNcGXN68O9": (92_519.899_3, 1_247_315.537_2),
    "1VluSltEX0WftNcGXN68P0": (92_521.899_3, 1_247_315.537_2),
}
Z_SPAN_M = (127.25, 134.636_5)
# 200 mm * arc_area_scale(pi, 16) — the area-preserving radius the
# adaptive tessellator (GH #170) samples a 16-chord semicircle at.
OUTER_R_MM = 200.644_4
# The bound the issue asked CI to hold: a known IfcArcIndex circle comes
# back with a radius spread below 0.05 mm.
SPREAD_TOL_MM = 0.05


# ---------------------------------------------------------------------
# a minimal GLB reader (numpy only — no extra deps)
# ---------------------------------------------------------------------


def load_glb(path):
    """`(json_dict, bin_chunk)` from a .glb."""
    raw = open(path, "rb").read()
    magic, version, _total = struct.unpack_from("<III", raw, 0)
    assert magic == 0x46546C67 and version == 2, "not a glTF 2.0 binary"
    json_len, json_tag = struct.unpack_from("<I4s", raw, 12)
    assert json_tag == b"JSON"
    doc = json.loads(raw[20 : 20 + json_len])
    off = 20 + json_len
    bin_len, bin_tag = struct.unpack_from("<I4s", raw, off)
    assert bin_tag == b"BIN\x00"
    return doc, raw[off + 8 : off + 8 + bin_len]


_DTYPE = {5120: "<i1", 5121: "<u1", 5122: "<i2", 5123: "<u2", 5125: "<u4", 5126: "<f4"}
_NCOMP = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}


def accessor(doc, blob, index):
    """Read one accessor as an `(count, ncomp)` float64 array (raw units
    — quantized u16 positions come back as 0..65535, undenormalised)."""
    acc = doc["accessors"][index]
    view = doc["bufferViews"][acc["bufferView"]]
    n = acc["count"] * _NCOMP[acc["type"]]
    start = view.get("byteOffset", 0) + acc.get("byteOffset", 0)
    dt = np.dtype(_DTYPE[acc["componentType"]])
    # No interleaving in ifcfast output; byteStride is never emitted.
    assert "byteStride" not in view, "reader assumes tightly packed views"
    arr = np.frombuffer(blob, dtype=dt, count=n, offset=start).astype(np.float64)
    return arr.reshape(acc["count"], _NCOMP[acc["type"]])


def quat_rotate(q, v):
    """Rotate `v` (N,3) by the glTF quaternion `q` (x, y, z, w)."""
    x, y, z, w = q
    u = np.array([x, y, z], dtype=np.float64)
    return (
        v * (w * w - u @ u)
        + 2.0 * np.outer(v @ u, u)
        + 2.0 * w * np.cross(np.broadcast_to(u, v.shape), v)
    )


def positions_by_guid(path):
    """`{guid: (N, 3) array}` in ifcfast's own frame — shifted world
    metres — for both writer paths.

    The `ifcfast_root` node carries only the Z-up → Y-up rotation the
    viewer wants, so its children's local frames *are* ifcfast's frame;
    the reader deliberately stops there rather than composing it.
    """
    doc, blob = load_glb(path)
    roots = [n for n in doc["nodes"] if n.get("name") == "ifcfast_root"]
    assert len(roots) == 1, "expected one ifcfast_root"
    out = {}
    for child in roots[0]["children"]:
        node = doc["nodes"][child]
        mesh = doc["meshes"][node["mesh"]]
        prim = mesh["primitives"][0]
        quantized = accessor(doc, blob, prim["attributes"]["POSITION"])
        inst = node.get("extensions", {}).get("EXT_mesh_gpu_instancing")
        if inst is None:
            # Baked: node TRS denormalises the u16 stream in place.
            t = np.array(node.get("translation", [0.0, 0.0, 0.0]))
            s = np.array(node.get("scale", [1.0, 1.0, 1.0]))
            assert "rotation" not in node, "baked nodes are axis-aligned"
            out[node["extras"]["guid"]] = quantized * s + t
            continue
        attrs = inst["attributes"]
        trans = accessor(doc, blob, attrs["TRANSLATION"])
        rot = accessor(doc, blob, attrs["ROTATION"])
        scale = accessor(doc, blob, attrs["SCALE"])
        for i, meta in enumerate(node["extras"]["instances"]):
            out[meta["guid"]] = quat_rotate(rot[i], quantized * scale[i]) + trans[i]
    return out


def radius_probe(pts, guid):
    """`(outer_count, mean_mm, spread_mm)` about the duct axis, plus the
    absolute XY bbox centre and z span — `pts` are shifted world metres,
    `shift` already added back by the caller."""
    ax, ay = DUCT_AXIS[guid]
    r_mm = np.hypot(pts[:, 0] - ax, pts[:, 1] - ay) * 1000.0
    outer = r_mm[r_mm > 199.5]
    centre = ((pts[:, 0].min() + pts[:, 0].max()) / 2, (pts[:, 1].min() + pts[:, 1].max()) / 2)
    return outer, centre, (pts[:, 2].min(), pts[:, 2].max())


def _csg_available() -> bool:
    """`cut_openings=True` needs a wheel built with the `csg` Cargo
    feature; without it the core raises `RuntimeError`. Probe once."""
    import tempfile
    from pathlib import Path

    try:
        m = ifcfast.open(f"{FIXTURES}/geom_box.ifc")
        with tempfile.TemporaryDirectory() as d:
            m.to_gltf(Path(d) / "probe.glb", cut_openings=True)
        return True
    except RuntimeError as exc:  # pragma: no cover - depends on build
        if "csg" in str(exc):
            return False
        raise


requires_csg = pytest.mark.skipif(
    not _csg_available(), reason="wheel built without the `csg` Cargo feature"
)


def write(tmp_path, fixture, **kw):
    m = ifcfast.open(f"{FIXTURES}/{fixture}")
    tmp_path.mkdir(parents=True, exist_ok=True)
    out = tmp_path / f"{fixture}.glb"
    stats = m.to_gltf(out, **kw)
    return out, stats


# ---------------------------------------------------------------------
# the far-origin duct
# ---------------------------------------------------------------------


@pytest.mark.parametrize(
    "cut_openings,want_instancing",
    [(False, True), pytest.param(True, False, marks=requires_csg)],
)
def test_far_origin_duct_is_round_in_both_writer_paths(
    tmp_path, cut_openings, want_instancing
):
    out, stats = write(
        tmp_path, "far_origin_duct_mm.ifc", cut_openings=cut_openings
    )
    assert stats["instancing"] is want_instancing
    shift = np.array(stats["global_shift"])
    assert np.any(shift != 0.0), "a georeferenced model must report its shift"

    by_guid = positions_by_guid(out)
    assert set(by_guid) == set(DUCT_AXIS)
    for guid, local in by_guid.items():
        pts = local + shift
        outer, centre, (z0, z1) = radius_probe(pts, guid)
        assert len(outer) >= 32, f"{guid}: only {len(outer)} outer vertices"
        spread = outer.max() - outer.min()
        assert spread < SPREAD_TOL_MM, (
            f"{guid}: outer radius spread {spread:.4f} mm "
            f"(mean {outer.mean():.4f}) — f32 quantisation at NTM magnitude"
        )
        # The quantization denorm is the only error left, and it is
        # ~1/65535 of the node's extent — well inside 0.05 mm here.
        assert abs(outer.mean() - OUTER_R_MM) < 0.01
        ax, ay = DUCT_AXIS[guid]
        assert abs(centre[0] - ax) < 1.0e-4 and abs(centre[1] - ay) < 1.0e-4
        assert abs(z0 - Z_SPAN_M[0]) < 1.0e-4 and abs(z1 - Z_SPAN_M[1]) < 1.0e-4


@requires_csg
def test_baked_and_instanced_agree_on_world_positions(tmp_path):
    """The two writer paths are two encodings of one frame. Before the
    fix they disagreed twice over: the instanced translation was the f32
    `world_transform` (up to 64 mm out at NTM) and it carried no unit
    scale (1000x on a millimetre model)."""
    inst_path, inst_stats = write(
        tmp_path / "a", "far_origin_duct_mm.ifc", cut_openings=False
    )
    baked_path, baked_stats = write(
        tmp_path / "b", "far_origin_duct_mm.ifc", cut_openings=True
    )
    assert inst_stats["instancing"] and not baked_stats["instancing"]
    assert inst_stats["global_shift"] == baked_stats["global_shift"]

    instanced = positions_by_guid(inst_path)
    baked = positions_by_guid(baked_path)
    assert set(instanced) == set(baked)
    for guid in baked:
        a, b = baked[guid], instanced[guid]
        assert a.shape == b.shape, f"{guid}: {a.shape} vs {b.shape}"
        worst_mm = np.abs(a - b).max() * 1000.0
        assert worst_mm < SPREAD_TOL_MM, (
            f"{guid}: baked vs instanced disagree by {worst_mm:.4f} mm"
        )


def test_one_wheel_reports_one_global_shift(tmp_path):
    """Every surface that hands out shifted world metres reports the
    same three numbers, and they are the rounded anchor in *true* metres
    — `92519899 mm * 0.001`, not `* float32(0.001)`.

    `crates/wasm/test/stream.mjs` holds `shiftJson()` / `streamShiftJson()`
    to this same literal, so the wheel and the browser build are pinned
    to one convention from both sides.
    """
    expected = [92519.899, 1247315.537, 127.25]
    m = ifcfast.open(f"{FIXTURES}/far_origin_duct_mm.ifc")
    out = tmp_path / "shift.glb"
    tmp_path.mkdir(parents=True, exist_ok=True)
    shifts = {
        "meshes": m.meshes().global_shift,
        "iter_meshes": m.iter_meshes().global_shift,
        "to_gltf": m.to_gltf(out, cut_openings=False)["global_shift"],
        "point_cloud": list(m.point_cloud(per_m2=1.0).attrs["global_shift"]),
    }
    for name, got in shifts.items():
        assert np.allclose(got, expected, rtol=0, atol=1e-9), f"{name}: {got}"
    first = list(shifts.values())[0]
    for name, got in shifts.items():
        assert list(got) == list(first), f"{name} {got} != {first}"


UNPLACED = "far_origin_unplaced_first_mm.ifc"


def test_shift_survives_an_unplaced_first_product(tmp_path):
    """The shift is a property of the FILE, not of whichever product a
    given path happens to emit first.

    `far_origin_unplaced_first_mm.ifc` leads with an
    `IfcBuildingElementProxy` whose `ObjectPlacement` is `$`. A `$`,
    cyclic or `IfcGridPlacement` placement resolves to identity, so that
    product's `mesh_anchor` is the origin — and it is the first product
    every path emits. Pinning the shift from the first emitted anchor
    would read `[0, 0, 0]` on a file 1 247 km out and encode every duct
    after it at absolute f32 magnitude: GH #188 in full, silently, with
    nothing in the output saying so.
    """
    expected = [92519.899, 1247315.537, 127.25]
    m = ifcfast.open(f"{FIXTURES}/{UNPLACED}")
    tmp_path.mkdir(parents=True, exist_ok=True)
    ms = m.meshes()

    # The premise: the unplaced box really is emitted first.
    assert ms[0].guid == "0UnplacedFirstProd00__", [x.guid for x in ms]

    shifts = {
        "meshes": ms.global_shift,
        "iter_meshes": m.iter_meshes().global_shift,
        "to_gltf": m.to_gltf(tmp_path / "u.glb", cut_openings=False)["global_shift"],
        "point_cloud": list(m.point_cloud(per_m2=1.0).attrs["global_shift"]),
    }
    for name, got in shifts.items():
        assert np.allclose(got, expected, rtol=0, atol=1e-9), f"{name}: {got}"

    # …and the ducts are still round, which is the thing the shift buys.
    shift = np.array(ms.global_shift)
    ducts = [x for x in ms if x.guid in DUCT_AXIS]
    assert len(ducts) == 2
    for mesh in ducts:
        outer, _, _ = radius_probe(mesh.vertices.astype(np.float64) + shift, mesh.guid)
        spread = outer.max() - outer.min()
        assert spread < SPREAD_TOL_MM, f"{mesh.guid}: {spread:.4f} mm"

    # The GLB self-describes the same value.
    doc, _ = load_glb(tmp_path / "u.glb")
    assert np.allclose(
        doc["asset"]["extras"]["ifcfast"]["global_shift"], expected, rtol=0, atol=1e-9
    )


def test_glb_self_describes_its_frame(tmp_path):
    out, stats = write(tmp_path, "far_origin_duct_mm.ifc", cut_openings=False)
    doc, _ = load_glb(out)
    extras = doc["asset"]["extras"]["ifcfast"]["global_shift"]
    assert extras == stats["global_shift"]
    assert len(extras) == 3 and any(v != 0.0 for v in extras)


def test_no_zero_area_triangles(tmp_path):
    """The fixture's own health check, not a GH #188 regression gate.

    `m.meshes()` already ran in the Local frame before this change, so it
    never had the collapse. What this pins is that the *fixture* is a
    clean Ø400 annulus: 262 faces, none of them degenerate, the thinnest
    being the 2 mm duct wall. If it ever stops being that, the radius
    bounds the other tests assert stop meaning anything.

    For reference, the same duct baked in the World frame — which is what
    the glTF writer and the browser stream used to do — comes back with
    134 of those 262 faces at exactly zero area.

    Deliberately not measured on the GLB: `KHR_mesh_quantization` folds
    this profile's two arc-seam vertices (where the two `IfcArcIndex`
    semicircles meet, a sub-micron apart) onto one u16 lattice point, so
    6 bridge triangles collapse there in *any* frame. That is a
    profile/quantization property, not a georeference one.
    """
    m = ifcfast.open(f"{FIXTURES}/far_origin_duct_mm.ifc")
    meshes = m.meshes()
    assert len(meshes) == 2
    for mesh in meshes:
        v, f = mesh.vertices.astype(np.float64), mesh.faces
        a, b, c = v[f[:, 0]], v[f[:, 1]], v[f[:, 2]]
        area = 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1)
        assert int((area <= 0.0).sum()) == 0, (
            f"{mesh.guid}: {int((area <= 0.0).sum())} of {len(f)} faces collapsed"
        )
        # The wall faces — everything but the two arc-seam bridges —
        # are healthy: the thinnest is the 2 mm duct wall.
        assert float(np.median(area)) > 1.0e-5


# ---------------------------------------------------------------------
# the control group: near-origin output is unchanged
# ---------------------------------------------------------------------


def test_near_origin_model_reports_no_shift_and_keeps_absolute_coords(tmp_path):
    """`geom_box.ifc` is a 2 x 3 x 4 m box at the origin: shift `[0,0,0]`,
    positions absolute, exactly as before GH #188."""
    out, stats = write(tmp_path, "geom_box.ifc", cut_openings=False)
    assert stats["global_shift"] == [0.0, 0.0, 0.0]
    doc, _ = load_glb(out)
    assert doc["asset"]["extras"]["ifcfast"]["global_shift"] == [0, 0, 0]
    pts = next(iter(positions_by_guid(out).values()))
    lo, hi = pts.min(axis=0), pts.max(axis=0)
    assert np.allclose(lo, [-1.0, -1.5, 0.0], atol=1e-4), lo
    assert np.allclose(hi, [1.0, 1.5, 4.0], atol=1e-4), hi


def test_far_origin_box_reports_its_shift(tmp_path):
    """`far_origin_box.ifc` (GH #179) is the same box at NTM-scale metre
    coordinates. It is past the 10 km threshold, so it gets a shift — and
    `position + shift` still lands on the authored placement."""
    out, stats = write(tmp_path, "far_origin_box.ifc", cut_openings=False)
    shift = np.array(stats["global_shift"])
    assert np.allclose(shift, [213000.0, 6720000.0, 120.0])
    pts = next(iter(positions_by_guid(out).values())) + shift
    lo, hi = pts.min(axis=0), pts.max(axis=0)
    assert np.allclose(lo, [213000.01 - 1.0, 6720000.32 - 1.5, 120.0], atol=1e-4), lo
    assert np.allclose(hi, [213000.01 + 1.0, 6720000.32 + 1.5, 124.0], atol=1e-4), hi
