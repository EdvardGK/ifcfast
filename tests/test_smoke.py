"""End-to-end smoke tests against a tiny synthetic IFC fixture.

These exercise the full pipeline (Rust indexer + Python wrappers + data
extractors) and are the first line of defence against broken wheels.
The fixture is hand-written STEP/ISO-10303-21 (tests/fixtures/minimal.ifc)
covering one wall under one storey with property set, base quantities,
material and NS 3451 classification.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import ifcfast

FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"
WALL_GUID = "7XvctVUKr0kugbFTf53O9L"


@pytest.fixture(scope="module")
def model():
    return ifcfast.open(FIXTURE, use_cache=False, write_cache=False)


def test_header():
    h = ifcfast.header(FIXTURE)
    assert h.schema == "IFC4"
    assert h.preprocessor_version == "ifcfast"
    assert h.originating_system == "ifcfast-tests"
    # authoring_app is an alias for originating_system (matches Model.authoring_app naming)
    assert h.authoring_app == h.originating_system
    assert "ifcfast tests" in h.author
    assert h.size_bytes == FIXTURE.stat().st_size
    assert len(h.cache_key) == 16  # 16-hex-char short digest


def test_top_level_metadata(model):
    assert model.schema == "IFC4"
    assert model.project_name == "Minimal Project"
    assert model.authoring_app == "ifcfast-tests"


def test_products(model):
    assert len(model) == 1
    assert model.types() == {"IfcWall": 1}
    wall = model.product(WALL_GUID)
    assert wall is not None
    assert wall.entity == "IfcWall"
    assert wall.name == "Wall-001"
    assert wall.tag == "tag-001"


def test_storey_and_containment(model):
    assert len(model.storeys) == 1
    storey = model.storeys[0]
    assert storey.name == "Plan 01"
    assert storey.elevation == pytest.approx(0.0)
    wall = model.product(WALL_GUID)
    assert wall.storey_guid == storey.guid
    assert wall.storey_name == "Plan 01"


def test_filter_returns_iterable(model):
    walls = list(model.filter(entity="IfcWall"))
    assert len(walls) == 1
    assert walls[0].guid == WALL_GUID


def test_psets(model):
    df = model.psets
    assert len(df) == 2
    rows = {r.prop_name: (r.value, r.value_type) for r in df.itertuples()}
    assert rows["IsExternal"] == ("True", "IfcBoolean")
    assert rows["LoadBearing"] == ("False", "IfcBoolean")
    assert (df["guid"] == WALL_GUID).all()
    assert (df["pset_name"] == "Pset_WallCommon").all()


def test_quantities(model):
    df = model.quantities
    assert len(df) == 2
    by_name = {r.quantity_name: (float(r.value), r.quantity_type) for r in df.itertuples()}
    assert by_name["Length"] == (3.0, "Length")
    assert by_name["NetSideArea"] == (7.5, "Area")
    assert (df["qto_name"] == "Qto_WallBaseQuantities").all()


def test_materials(model):
    df = model.materials
    assert len(df) == 1
    row = df.iloc[0]
    assert row["guid"] == WALL_GUID
    assert row["material_name"] == "Concrete"


def test_classifications(model):
    df = model.classifications
    assert len(df) == 1
    row = df.iloc[0]
    assert row["guid"] == WALL_GUID
    assert row["system_name"] == "NS 3451"
    assert row["edition"] == "2022"
    assert row["identification"] == "232.1"
    assert row["name"] == "Yttervegger"
    assert row["source"] == "Standard Norge"


def test_cli_index(capfd):
    import subprocess
    import sys

    result = subprocess.run(
        [sys.executable, "-m", "ifcfast.cli", "index", str(FIXTURE)],
        capture_output=True,
        text=True,
        check=True,
    )
    out = result.stdout
    assert "schema:        IFC4" in out
    assert "project:       Minimal Project" in out
    assert "products:      1" in out
    assert "IfcWall" in out


def test_cache_key_includes_schema_version():
    """Bumping `_CACHE_SCHEMA_VERSION` must change the cache_key for the
    same file. Without this, an upgrade that changes the *meaning* of a
    cached column (e.g. v0.4.1's materials.thickness_mm metres→mm fix)
    silently serves stale numbers from the old cache directory.
    """
    # `ifcfast.__init__` does `from .header import header`, which shadows
    # the `header` submodule attribute on the package with the `header()`
    # function — and pytest's `monkeypatch.setattr("ifcfast.header.X", ...)`
    # walks the same shadowed namespace. Pull the module from `sys.modules`
    # and swap the constant by hand.
    import sys
    hdr_mod = sys.modules["ifcfast.header"]

    baseline = hdr_mod._compute_cache_key(FIXTURE, FIXTURE.stat().st_size)
    original = hdr_mod._CACHE_SCHEMA_VERSION
    try:
        hdr_mod._CACHE_SCHEMA_VERSION = original + 1
        bumped = hdr_mod._compute_cache_key(FIXTURE, FIXTURE.stat().st_size)
    finally:
        hdr_mod._CACHE_SCHEMA_VERSION = original

    assert baseline != bumped, (
        "cache_key must change when _CACHE_SCHEMA_VERSION bumps — "
        f"both gave {baseline}; stale caches won't invalidate on upgrade"
    )
    assert len(baseline) == 16
    assert len(bumped) == 16


# Inline millimetre-unit IFC: a 1000 mm × 1000 mm × 1000 mm extruded
# cube. The whole point is `IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.)`
# — vertices are authored as 1000 (mm), so a correct metres-output
# point cloud / mesh must span ~1.0, not ~1000.
_MM_CUBE = """ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('mm_cube.ifc','2026-05-29T00:00:00',('t'),('t'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#15=IFCLOCALPLACEMENT($,#6);
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'Sq',#31,1000.,1000.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,1000.);
#40=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#33));
#41=IFCPRODUCTDEFINITIONSHAPE($,$,(#40));
#50=IFCWALL('7Wall00000000000000001',$,'mm cube',$,$,#15,#41,'tag',.STANDARD.);
ENDSEC;
END-ISO-10303-21;
"""


def _write_mm_cube(tmp_path):
    p = tmp_path / "mm_cube.ifc"
    p.write_text(_MM_CUBE, encoding="utf-8")
    return p


def test_point_cloud_returns_metres_not_native_units(tmp_path):
    """Regression: point_cloud() xyz must be metres regardless of the
    file's native length unit. A 1000 mm cube must span ~1.0 m, not
    ~1000. (v0.4.9/v0.4.10 shipped this in native mm — the docstring
    promised metres; fixed by scaling Rust-side via unit_scale.)
    """
    pytest.importorskip("numpy")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)
    assert m.unit_scale == pytest.approx(0.001)  # millimetre file

    df = m.point_cloud(per_m2=100, seed=1)
    assert len(df) > 0
    span = df[["x", "y", "z"]].max().values - df[["x", "y", "z"]].min().values
    # Each dimension is a 1000 mm = 1.0 m edge.
    for axis, s in zip("xyz", span):
        assert 0.9 < s < 1.1, f"{axis} span {s:.3f} m — expected ~1.0 (metres, not mm)"


def test_meshes_returns_metres_not_native_units(tmp_path):
    """Same regression for the raw-mesh API: meshes()[i].vertices must
    be metres. The mm cube's vertices must span ~1.0 m per axis.
    """
    pytest.importorskip("numpy")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)
    meshes = m.meshes()
    assert len(meshes) == 1
    v = meshes[0].vertices
    span = v.max(axis=0) - v.min(axis=0)
    for axis, s in zip("xyz", span):
        assert 0.9 < s < 1.1, f"{axis} span {s:.3f} m — expected ~1.0 (metres, not mm)"


def test_point_cloud_unit_parameter(tmp_path):
    """The `unit=` parameter rescales output coordinates. A 1000 mm
    cube spans ~1.0 m, ~1000 mm, ~100 cm, ~3.28 ft. Normals must stay
    unit-length regardless of unit; per_m2 density is physical and does
    not change with unit.
    """
    np = pytest.importorskip("numpy")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    expectations = {"m": 1.0, "mm": 1000.0, "cm": 100.0, "ft": 3.28084, "in": 39.3701}
    for unit, expect in expectations.items():
        df = m.point_cloud(per_m2=50, seed=1, unit=unit)
        span_x = df["x"].max() - df["x"].min()
        assert span_x == pytest.approx(expect, rel=0.02), (
            f"unit={unit}: x span {span_x:.4g}, expected ~{expect:.4g}"
        )
        # Normals are direction vectors — must remain unit-length.
        nmag = np.linalg.norm(df[["nx", "ny", "nz"]].values, axis=1)
        assert np.allclose(nmag, 1.0, atol=1e-4)

    # Unknown unit raises rather than silently mis-scaling.
    with pytest.raises(ValueError):
        m.point_cloud(unit="furlong")


def test_meshes_unit_parameter(tmp_path):
    """meshes(unit=) rescales vertices; default metres is a zero-copy
    read-only view, any other unit a writable scaled copy.
    """
    pytest.importorskip("numpy")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    v_m = m.meshes(unit="m")[0].vertices
    assert (v_m.max(axis=0) - v_m.min(axis=0))[0] == pytest.approx(1.0, rel=0.01)

    v_mm = m.meshes(unit="mm")[0].vertices
    assert (v_mm.max(axis=0) - v_mm.min(axis=0))[0] == pytest.approx(1000.0, rel=0.01)
    assert v_mm.flags.writeable  # scaled copy is writable


def test_length_unit_property(tmp_path):
    """m.length_unit reflects the file's authored unit."""
    p = _write_mm_cube(tmp_path)
    assert ifcfast.open(p, use_cache=False, write_cache=False).length_unit == "mm"
    # The bundled metres fixture.
    assert ifcfast.open(FIXTURE, use_cache=False, write_cache=False).length_unit == "m"


def test_zip_disguised_as_ifc(tmp_path):
    """An ifczip distributed with a plain `.ifc` extension (common in
    the wild — ACC, Dalux, Trimble Connect all do this; other tools
    misread it as "corrupted STEP") must still open transparently:
    same magic-byte dispatch as the Rust `source::open`, applied to
    Python's header() too so the path doesn't error before Rust runs.
    """
    import zipfile

    body = FIXTURE.read_bytes()
    bogus = tmp_path / "looks_like.ifc"
    with zipfile.ZipFile(bogus, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("Sannergata_RIV.ifc", body)

    # First 4 bytes are the ZIP magic, not "ISO-".
    assert bogus.read_bytes()[:4] == b"PK\x03\x04"

    h = ifcfast.header(bogus)
    assert h.schema == "IFC4"
    assert h.authoring_app == "ifcfast-tests"

    m = ifcfast.open(bogus, use_cache=False, write_cache=False)
    assert m.types() == {"IfcWall": 1}


# ---------------------------------------------------------------------------
# iter_point_cloud — streaming point cloud (GH #23 fix)
# ---------------------------------------------------------------------------


def test_iter_point_cloud_chunks_sum_matches_single_shot(tmp_path):
    """Streaming union must equal single-shot output. The iterator can
    flush mid-product, so a stable groupby-by-GUID across chunks
    reconstructs the per-product sample set; sum of points across
    chunks equals total from `point_cloud()` at the same (per_m2, seed).
    """
    pd = pytest.importorskip("pandas")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    single = m.point_cloud(per_m2=100, seed=1)
    chunks = list(m.iter_point_cloud(per_m2=100, seed=1, chunk_points=17))
    total = sum(len(c) for c in chunks)
    assert total == len(single)
    assert all(len(c) > 0 for c in chunks)
    # Every chunk except the last should be exactly chunk_points; the
    # current sink flushes the moment the buffer crosses the threshold,
    # so all middle chunks are pinned at exactly 17.
    for c in chunks[:-1]:
        assert len(c) == 17
    assert len(chunks[-1]) <= 17


def test_iter_point_cloud_global_shift_stable_per_chunk(tmp_path):
    """Every yielded chunk carries the same global_shift (model-wide,
    not per-chunk). Near-origin fixture → shift == [0, 0, 0].
    """
    pytest.importorskip("pandas")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    chunks = list(m.iter_point_cloud(per_m2=200, seed=7, chunk_points=25))
    assert len(chunks) >= 2
    shifts = [tuple(c.attrs["global_shift"]) for c in chunks]
    assert len(set(shifts)) == 1, f"global_shift drifted across chunks: {shifts}"
    assert shifts[0] == (0.0, 0.0, 0.0)


def test_iter_point_cloud_unit_factor_applies_per_chunk(tmp_path):
    """`unit="mm"` scales every chunk's xyz (and its global_shift) the
    same way the single-shot API does — verifies the Python wrapper
    applies the factor per yielded DataFrame.
    """
    pytest.importorskip("pandas")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    chunks_m = list(m.iter_point_cloud(per_m2=50, seed=3, unit="m", chunk_points=20))
    chunks_mm = list(m.iter_point_cloud(per_m2=50, seed=3, unit="mm", chunk_points=20))
    assert len(chunks_m) == len(chunks_mm)
    for cm, cmm in zip(chunks_m, chunks_mm):
        span_m = cm["x"].max() - cm["x"].min()
        span_mm = cmm["x"].max() - cmm["x"].min()
        # mm chunk should be 1000× the metres chunk per axis.
        assert span_mm == pytest.approx(span_m * 1000.0, rel=1e-3)


def test_iter_point_cloud_deterministic(tmp_path):
    """Same (per_m2, seed) at same chunk_points → bit-identical chunk
    contents across iterations. Determinism is a hard contract for ML
    pipelines using this as a sampling backend.
    """
    pytest.importorskip("pandas")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    a = list(m.iter_point_cloud(per_m2=50, seed=42, chunk_points=23))
    b = list(m.iter_point_cloud(per_m2=50, seed=42, chunk_points=23))
    assert len(a) == len(b)
    for ca, cb in zip(a, b):
        assert (ca["x"].values == cb["x"].values).all()
        assert (ca["y"].values == cb["y"].values).all()
        assert (ca["z"].values == cb["z"].values).all()
        assert (ca["guid"].values == cb["guid"].values).all()


def test_iter_point_cloud_zero_chunk_raises(tmp_path):
    """`chunk_points=0` is a user error — surface as IfcfastError, not
    a confusing downstream divide-by-zero deep in the Rust sink."""
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)
    with pytest.raises(ifcfast.IfcfastError):
        list(m.iter_point_cloud(per_m2=10, seed=1, chunk_points=0))


def test_ifcfast_error_is_exposed():
    """IfcfastError is the public catch type for recoverable Rust
    failures (panic-to-Python translation, validation errors). Make
    sure the public re-export is wired correctly.
    """
    assert issubclass(ifcfast.IfcfastError, Exception)


def test_iter_point_cloud_early_drop_does_not_hang(tmp_path):
    """Dropping the iterator before exhausting it must release the
    worker promptly via the AtomicBool stop flag — otherwise consumer
    code that reads one chunk and bails (e.g. preview / sampling
    pipelines) would leak a tessellation thread per file.
    """
    pytest.importorskip("pandas")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    it = m.iter_point_cloud(per_m2=200, seed=1, chunk_points=10)
    first = next(it)
    assert len(first) == 10
    # Explicitly drop the generator + underlying pyclass.
    del it
    # If the worker were stuck holding GIL / refs, a subsequent call
    # would block. Run a second iter immediately as a liveness check.
    second_chunks = list(m.iter_point_cloud(per_m2=50, seed=2, chunk_points=999_999))
    assert len(second_chunks) >= 1
