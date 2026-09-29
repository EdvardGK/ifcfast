"""GH #194 — clipping bodies are the element's own shape in every mode.

A half-space second operand of an ``IfcBooleanClippingResult`` is applied
by the extractor itself, not only under ``cut_openings=True``. Fixtures are
Revit 2025 IFC2X3 walls (feet) from the Snowdon Towers sample, pinned
against ifcopenshell with opening subtraction disabled:

* ``clip_single_pbhs_194.ifc`` — one wall, one
  ``IfcPolygonalBoundedHalfSpace`` ``.T.``, no opening → 37.7596 m³
  (before GH #194 the no-cut value was the unclipped box, 38.9135 m³).
* ``clip_chain3_194.ifc`` — the same wall under three chained clips, its
  ``IfcRelVoidsElement`` removed → 23.8721 m³.
"""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

import ifcfast

FIX = Path(__file__).parent / "fixtures"
SINGLE = FIX / "clip_single_pbhs_194.ifc"
CHAIN3 = FIX / "clip_chain3_194.ifc"
WALL = "1G_I00WlX2MBQekqB04dQj"
IOS = {SINGLE: 37.7596, CHAIN3: 23.8721}
UNCLIPPED = 38.9135
# The void-bearing original stays in scratch (not committed); gated below.
REPRO = Path(__file__).parent.parent / "scratch" / "g194" / "repro.ifc"


def _open(path):
    return ifcfast.open(path, use_cache=False, write_cache=False)


def _mesh_volume(v: np.ndarray, f: np.ndarray) -> float:
    v = v.astype(np.float64)
    v = v - v.min(axis=0)
    t = v[f]
    return float(np.einsum("ij,ij->i", t[:, 0], np.cross(t[:, 1], t[:, 2])).sum() / 6)


def _row(df):
    return df.set_index("guid").loc[WALL]


@pytest.mark.parametrize("path", [SINGLE, CHAIN3], ids=lambda p: p.stem)
def test_no_cut_qto_is_the_clipped_solid(path):
    m = _open(path)
    row = _row(m.mesh_qto(cut_openings=False)[0])
    assert row.volume_m3 == pytest.approx(IOS[path], rel=1e-3)
    assert bool(row.volume_reliable)
    assert row.volume_method == "mesh"
    assert row.mesh_quality == "closed"


@pytest.mark.parametrize("path", [SINGLE, CHAIN3], ids=lambda p: p.stem)
def test_cut_mode_equals_no_cut_without_voids(path):
    m = _open(path)
    nc = _row(m.mesh_qto(cut_openings=False)[0])
    cu = _row(m.mesh_qto(cut_openings=True)[0])
    assert cu.volume_m3 == pytest.approx(nc.volume_m3, rel=1e-9)


@pytest.mark.parametrize("path", [SINGLE, CHAIN3], ids=lambda p: p.stem)
def test_meshes_default_equals_cut_mode_on_clipped_unvoided_wall(path):
    m = _open(path)
    a = {x.guid: x for x in m.meshes()}[WALL]
    b = {x.guid: x for x in m.meshes(cut_openings=True)}[WALL]
    assert np.array_equal(a.faces, b.faces)
    assert np.array_equal(a.vertices, b.vertices)
    assert _mesh_volume(a.vertices, a.faces) == pytest.approx(IOS[path], rel=1e-3)


def test_single_product_mesh_and_keep_cutters():
    m = _open(SINGLE)
    one = m.mesh(WALL)
    assert _mesh_volume(one.vertices, one.faces) == pytest.approx(IOS[SINGLE], rel=1e-3)
    # keep_cutters is a compatibility no-op: no stand-in slab exists any more.
    kept = {x.guid: x for x in m.meshes(keep_cutters=True)}[WALL]
    dflt = {x.guid: x for x in m.meshes()}[WALL]
    assert np.array_equal(kept.faces, dflt.faces)
    assert np.array_equal(kept.vertices, dflt.vertices)


@pytest.mark.parametrize("path", [SINGLE, CHAIN3], ids=lambda p: p.stem)
def test_stats_count_no_unapplied_clip_and_no_halfspace_leak(path):
    m = _open(path)
    stats = m.meshes().stats
    assert stats["halfspace_clip_unapplied"] == 0
    assert stats["halfspace_clip_manifold"] == 0
    assert not [k for k in stats["by_source"] if "halfspace" in k]
    qstats = m.mesh_qto(cut_openings=False)[0].attrs["mesh_stats"]
    assert qstats["halfspace_clip_unapplied"] == 0


def test_substrate_carries_the_clipped_wall(tmp_path):
    pq = pytest.importorskip("pyarrow.parquet")
    info = ifcfast.bundle(SINGLE, tmp_path / "b")
    inst = pq.read_table(info["instances_parquet"]).to_pandas().set_index("guid")
    row = inst.loc[WALL]
    # instances.volume_m3 is measured on the WORLD-baked f32 mesh; this
    # wall sits ~1.4e6 ft from the origin, where f32 quantises to 1/8 ft
    # (the far-origin substrate limit, GH #117) — hence 2 %, but clearly
    # below the unclipped 38.9135.
    assert row.volume_m3 == pytest.approx(IOS[SINGLE], rel=0.02)
    assert row.volume_m3 < UNCLIPPED * 0.99
    assert bool(row.volume_reliable)
    # The representation row carries the exact clipped local mesh.
    reps = pq.read_table(info["representations_parquet"]).to_pandas().set_index("rep_id")
    rep = reps.loc[int(row.rep_id)]
    v = np.frombuffer(rep.vertices_le, dtype="<f4").reshape(-1, 3)
    f = np.frombuffer(rep.indices_le, dtype="<u4").reshape(-1, 3)
    ft3 = 0.3048**3
    assert _mesh_volume(v, f) * ft3 == pytest.approx(IOS[SINGLE], rel=1e-3)


@pytest.mark.skipif(not REPRO.exists(), reason="corpus repro scratch/g194/repro.ifc not present")
def test_corpus_repro_with_void_cut_mode_unchanged():
    """The original report's wall (3 chained clips + an IfcRelVoidsElement
    opening): cut mode still subtracts the opening (22.0075, ifcopenshell
    22.0091); no-cut is now the clipped, unvoided solid (23.8721)."""
    m = _open(REPRO)
    cu = _row(m.mesh_qto(cut_openings=True)[0])
    nc = _row(m.mesh_qto(cut_openings=False)[0])
    assert cu.volume_m3 == pytest.approx(22.0075, rel=1e-3)
    assert nc.volume_m3 == pytest.approx(23.8721, rel=1e-3)


# A half-space on a non-planar base surface cannot be evaluated: the host
# is returned UNCLIPPED, tagged, counted and routed out of trusted sums.
UNRESOLVABLE = """ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('unresolvable.ifc','2026-09-28T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#11=IFCBUILDING('2Bldg000000000000000001',$,'b',$,$,#15,$,$,.ELEMENT.,$,$,$);
#12=IFCBUILDINGSTOREY('3Stor000000000000000001',$,'L1',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5Agg000000000000000001',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6Agg000000000000000001',$,$,$,#11,(#12));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#50=IFCCARTESIANPOINT((0.,0.,1500.));
#51=IFCAXIS2PLACEMENT3D(#50,$,$);
#52=IFCCYLINDRICALSURFACE(#51,5000.);
#53=IFCHALFSPACESOLID(#52,.F.);
#60=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#33,#53);
#70=IFCSHAPEREPRESENTATION(#5,'Body','Clipping',(#60));
#71=IFCPRODUCTDEFINITIONSHAPE($,$,(#70));
#80=IFCWALL('7Wall00000000000000001',$,'Unresolvable',$,$,#16,#71,'tag',.STANDARD.);
#90=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel000000000000000001',$,$,$,(#80),#12);
ENDSEC;
END-ISO-10303-21;
"""


@pytest.mark.parametrize("cut", [False, True])
def test_unapplied_clip_is_never_silent(tmp_path, cut):
    p = tmp_path / "unresolvable.ifc"
    p.write_text(UNRESOLVABLE)
    m = _open(p)
    df = m.mesh_qto(cut_openings=cut)[0].set_index("guid")
    row = df.loc["7Wall00000000000000001"]
    assert row.volume_m3 == pytest.approx(0.6, rel=1e-6)  # the unclipped box
    assert not bool(row.volume_reliable)
    assert row.volume_method == "mesh_unclipped"
    assert df.attrs["mesh_stats"]["halfspace_clip_unapplied"] == 1
    stats = m.meshes().stats
    assert stats["halfspace_clip_unapplied"] == 1
    assert any("halfspace_unclipped" in k for k in stats["by_source"])


# G55_ARK `3POXqHuM96DwZzM2c25Gqt` pattern: Revit clips the wall with an
# IfcHalfSpaceSolid whose plane is the wall's own bottom face (.F., axis
# -Z at z=0) — it removes nothing — and the wall also has an
# IfcRelVoidsElement opening. The clip must leave the solid closed and
# untouched, so cut mode still subtracts the opening (0.6 − 0.2 m³).
PLANE_ON_FACE_WITH_OPENING = """ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('onface.ifc','2026-09-28T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#8=IFCDIRECTION((0.,0.,-1.));
#9=IFCDIRECTION((0.,1.,0.));
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#11=IFCBUILDING('2Bldg000000000000000001',$,'b',$,$,#15,$,$,.ELEMENT.,$,$,$);
#12=IFCBUILDINGSTOREY('3Stor000000000000000001',$,'Level 1',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5Agg000000000000000001',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6Agg000000000000000001',$,$,$,#11,(#12));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#36=IFCCARTESIANPOINT((500.,-100.,0.));
#37=IFCAXIS2PLACEMENT3D(#36,#8,#9);
#38=IFCPLANE(#37);
#39=IFCHALFSPACESOLID(#38,.F.);
#47=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#33,#39);
#34=IFCSHAPEREPRESENTATION(#5,'Body','Clipping',(#47));
#35=IFCPRODUCTDEFINITIONSHAPE($,$,(#34));
#40=IFCRECTANGLEPROFILEDEF(.AREA.,'OpeningRect',#41,500.,200.);
#41=IFCAXIS2PLACEMENT2D(#7,$);
#42=IFCAXIS2PLACEMENT3D(#43,$,$);
#43=IFCCARTESIANPOINT((0.,0.,500.));
#44=IFCEXTRUDEDAREASOLID(#40,#42,#32,2000.);
#45=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#44));
#46=IFCPRODUCTDEFINITIONSHAPE($,$,(#45));
#50=IFCWALL('7Wall00000000000000001',$,'TestWall',$,$,#16,#35,'tag',.STANDARD.);
#51=IFCOPENINGELEMENT('8Open00000000000000001',$,'Opening',$,$,#16,#46,'tag',.OPENING.);
#60=IFCRELVOIDSELEMENT('9Rel000000000000000001',$,$,$,#50,#51);
#70=IFCRELCONTAINEDINSPATIALSTRUCTURE('ARelC0000000000000001',$,$,$,(#50),#12);
ENDSEC;
END-ISO-10303-21;
"""


def test_clip_plane_on_host_face_removes_nothing_and_keeps_cut(tmp_path):
    p = tmp_path / "onface.ifc"
    p.write_text(PLANE_ON_FACE_WITH_OPENING)
    m = _open(p)
    nc = m.mesh_qto(cut_openings=False)[0].set_index("guid").loc["7Wall00000000000000001"]
    cu = m.mesh_qto(cut_openings=True)[0].set_index("guid").loc["7Wall00000000000000001"]
    assert nc.volume_m3 == pytest.approx(0.6, rel=1e-6)
    assert nc.mesh_quality == "closed"
    assert cu.volume_m3 == pytest.approx(0.4, rel=1e-4)
    assert cu.mesh_quality == "closed"


# ---------------------------------------------------------------------
# GH #194 review fixtures — millimetre IFC, 1000 mm cube hosts, pinned
# against ifcopenshell 0.8.5 (use-world-coords, openings disabled unless
# stated, ifcopenshell.util.shape.get_volume).
# ---------------------------------------------------------------------

UNRES_OPENING = FIX / "clip_unresolvable_opening_194.ifc"
MAPPED = FIX / "clip_mapped_194.ifc"
BOXED = FIX / "clip_boxed_194.ifc"
AXES_2X3 = FIX / "clip_default_axes_2x3_194.ifc"
DIAGONAL = FIX / "clip_diagonal_hs_194.ifc"


def _qto(path, cut):
    df = _open(path).mesh_qto(cut_openings=cut)[0]
    return df.set_index("guid"), df.attrs["mesh_stats"]


def test_unapplied_clip_survives_cut_with_opening():
    """Review blocker: the cut pass rewrites the host's segments to one
    ``cut_openings`` segment, which used to erase the
    ``halfspace_unclipped`` token — an unresolvable clip plus an
    ``IfcRelVoidsElement`` opening came back ``volume_reliable=True``.
    ifcopenshell: 1.0 m³ openings disabled, 0.75 m³ with the opening (it
    cannot apply the cylindrical half-space either)."""
    df, stats = _qto(UNRES_OPENING, cut=True)
    row = df.loc["7Wall00000000000000001"]
    assert row.volume_m3 == pytest.approx(0.75, rel=1e-4)  # opening cut, clip not
    assert not bool(row.volume_reliable)
    assert row.volume_method == "mesh_unclipped"
    assert stats["halfspace_clip_unapplied"] == 1
    # meshes(cut_openings=True) still exposes the signal.
    ms = _open(UNRES_OPENING).meshes(cut_openings=True)
    assert "7Wall00000000000000001" in {x.guid for x in ms}
    assert ms.stats["halfspace_clip_unapplied"] == 1
    assert any("halfspace_unclipped" in k for k in ms.stats["by_source"])
    # No-cut: the unclipped, unvoided box, still untrusted.
    df, stats = _qto(UNRES_OPENING, cut=False)
    row = df.loc["7Wall00000000000000001"]
    assert row.volume_m3 == pytest.approx(1.0, rel=1e-6)
    assert not bool(row.volume_reliable)
    assert row.volume_method == "mesh_unclipped"


# ifcopenshell: MappedIdentity 0.68, MappedMirrorScaled 1.02 (Axis1 = −X,
# Scale2 = 1.5), Direct 0.68 m³.
MAPPED_IOS = {
    "0MapA00000000000000001": 0.68,
    "0MapB00000000000000001": 1.02,
    "0MapC00000000000000001": 0.68,
}


@pytest.mark.parametrize("cut", [False, True])
def test_mapped_clipping_result_volumes(cut):
    df, stats = _qto(MAPPED, cut)
    assert stats["halfspace_clip_unapplied"] == 0
    for guid, want in MAPPED_IOS.items():
        row = df.loc[guid]
        assert row.volume_m3 == pytest.approx(want, rel=1e-5), guid
        assert bool(row.volume_reliable), guid
        assert row.mesh_quality == "closed", guid


def test_mapped_clipping_result_is_one_shared_rep(tmp_path):
    pq = pytest.importorskip("pyarrow.parquet")
    info = ifcfast.bundle(MAPPED, tmp_path / "b")
    inst = pq.read_table(info["instances_parquet"]).to_pandas().set_index("guid")
    reps = pq.read_table(info["representations_parquet"]).to_pandas().set_index("rep_id")
    a, b, c = (inst.loc[g] for g in MAPPED_IOS)
    # Both map instances point at ONE rep keyed by the clipping result
    # (#60); the direct use of the same leaf is its own rep (#70).
    assert int(a.rep_id) == int(b.rep_id) == 60
    assert int(c.rep_id) == 70
    assert int((reps.index == 60).sum()) == 1
    assert reps.loc[60].source_kind == "shared_or_direct"
    v = np.frombuffer(reps.loc[60].vertices_le, dtype="<f4").reshape(-1, 3)
    f = np.frombuffer(reps.loc[60].indices_le, dtype="<u4").reshape(-1, 3)
    assert _mesh_volume(v, f) * 1e-9 == pytest.approx(0.68, rel=1e-5)
    for guid, want in MAPPED_IOS.items():
        assert inst.loc[guid].volume_m3 == pytest.approx(want, rel=1e-5), guid


def _glb_json(path):
    import json
    import struct

    raw = Path(path).read_bytes()
    json_len, tag = struct.unpack_from("<I4s", raw, 12)
    assert tag == b"JSON"
    return json.loads(raw[20 : 20 + json_len])


def test_mapped_clipping_result_is_gltf_instanced(tmp_path):
    out = tmp_path / "mapped.glb"
    _open(MAPPED).to_gltf(out, cut_openings=False)
    doc = _glb_json(out)
    groups = [
        [m["guid"] for m in n["extras"]["instances"]]
        for n in doc["nodes"]
        if "EXT_mesh_gpu_instancing" in n.get("extensions", {})
    ]
    assert groups == [["0MapA00000000000000001", "0MapB00000000000000001"]]


def test_boxed_half_space_equals_half_space_solid():
    df, _ = _qto(BOXED, cut=False)
    hs = df.loc["0HsSolid00000000000001"]
    bx = df.loc["0HsBoxed00000000000001"]
    assert hs.volume_m3 == pytest.approx(0.68, rel=1e-5)  # ifcopenshell 0.68
    assert bx.volume_m3 == pytest.approx(hs.volume_m3, rel=1e-9)
    assert bool(bx.volume_reliable) and bx.mesh_quality == "closed"


def test_ifc2x3_default_axes_and_axis_x_pbhs():
    """IFC2X3: an IfcPlane whose Position has ``$`` Axis/RefDirection
    (ifcopenshell 0.4 m³), and a PBHS whose base plane and boundary
    Position are Axis = (1,0,0), RefDirection = ``$`` — singular before the
    IfcFirstProjAxis fix (ifcopenshell 0.9625 m³; the centred square
    boundary makes the value independent of the in-plane default axis)."""
    df, stats = _qto(AXES_2X3, cut=False)
    assert stats["halfspace_clip_unapplied"] == 0
    assert df.loc["0DefPlane0000000000001"].volume_m3 == pytest.approx(0.4, rel=1e-5)
    assert df.loc["0PbhsAxisX000000000001"].volume_m3 == pytest.approx(0.9625, rel=1e-5)
    assert df.volume_reliable.all()


def test_diagonal_unbounded_half_space_no_cut():
    df, stats = _qto(DIAGONAL, cut=False)
    row = df.loc["0Diag00000000000000001"]
    assert row.volume_m3 == pytest.approx(0.352666667, rel=1e-5)  # ifcopenshell
    assert bool(row.volume_reliable)
    assert row.mesh_quality == "closed"
