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


def test_version_matches_installed_metadata():
    """`ifcfast.__version__` must match the installed package metadata,
    not a hand-bumped string in __init__.py (GH #46).

    Pre-fix, __version__ was hardcoded and silently drifted out of
    sync with pyproject.toml across releases (every release required
    four manual bumps). Single source of truth: importlib.metadata
    reads from the .dist-info maturin generates from pyproject.toml.
    """
    from importlib.metadata import version as _pkg_version

    assert ifcfast.__version__ == _pkg_version("ifcfast")
    # And it's an actual string, not the fallback sentinel — a source-
    # only import without any install would hit "0.0.0+unknown".
    assert ifcfast.__version__ != "0.0.0+unknown"


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


def test_cli_missing_file_clean_error(capfd, tmp_path):
    """CLI on a non-existent file must emit a clean one-line error and
    exit 1, not a Python traceback (GH #42 item 1)."""
    import subprocess
    import sys

    missing = tmp_path / "does-not-exist.ifc"
    result = subprocess.run(
        [sys.executable, "-m", "ifcfast.cli", "index", str(missing)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 1
    assert "Traceback" not in result.stderr
    assert "ifcfast:" in result.stderr
    assert str(missing) in result.stderr


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


def test_mesh_qto_cut_openings_flag(tmp_path):
    """`m.mesh_qto(cut_openings=...)` accepts the flag and emits the
    expected stats keys (GH #37). The default switched from False to
    True in v0.4.28 — uncut numbers were over-reporting volume by
    70–262% on any element with an opening.

    The bundled fixture has no openings, so both modes produce
    identical numbers; the test asserts the flag plumbs through, the
    new default is True, and the cut_stats keys are present on the
    underlying dict.
    """
    pytest.importorskip("pandas")
    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    products_default, _ = m.mesh_qto()
    products_cut, _ = m.mesh_qto(cut_openings=True)
    products_uncut, _ = m.mesh_qto(cut_openings=False)
    assert len(products_default) >= 1
    # No openings on the cube fixture, so all three agree.
    assert products_default.volume_m3.tolist() == products_cut.volume_m3.tolist()
    assert products_default.volume_m3.tolist() == products_uncut.volume_m3.tolist()

    # Underlying dict carries the flag + cut stats — proves the
    # cross-product pipeline ran (even with zero matches on this
    # fixture, the flag still threads through).
    from ifcfast.model import native_path_for
    from ifcfast import _core
    d_on = _core.mesh_qto(str(native_path_for(m.header.path)), True)
    assert d_on["cut_openings"] is True
    assert "cut_openings_cut" in d_on
    assert "cut_openings_passthrough" in d_on
    assert "cut_openings_fallback" in d_on
    d_off = _core.mesh_qto(str(native_path_for(m.header.path)), False)
    assert d_off["cut_openings"] is False


def test_ifcfast_error_is_exposed():
    """IfcfastError is the public catch type for recoverable Rust
    failures (panic-to-Python translation, validation errors). Make
    sure the public re-export is wired correctly.
    """
    assert issubclass(ifcfast.IfcfastError, Exception)


def test_data_extractors_panic_safe_success_path_unchanged():
    """GH #27: the data-layer `_core` entry points (`index_ifc`,
    `extract_psets`, `extract_quantities`, `extract_materials`,
    `extract_classifications`, `extract_all`) are now wrapped in the
    same `catch_panic` boundary as the geometry entries, so a Rust
    panic surfaces as a catchable `IfcfastError` instead of an
    uncatchable `PanicException` that aborts the interpreter.

    Wrapping must not perturb the success path: each extractor still
    returns its dict on the valid fixture. (The panic→`IfcfastError`
    translation itself is pinned by the Rust `panic_safety_tests`
    unit tests, which can run without linking libpython.)
    """
    from ifcfast import _core

    p = str(FIXTURE)
    for fn in (
        _core.index_ifc,
        _core.extract_psets,
        _core.extract_quantities,
        _core.extract_materials,
        _core.extract_classifications,
        _core.extract_all,
    ):
        out = fn(p)
        assert isinstance(out, dict), f"{fn.__name__} should return a dict"
        assert "size_bytes" in out


def test_to_gltf_writes_valid_glb_with_extensions(tmp_path):
    """`m.to_gltf` produces a glTF 2.0 binary with the magic header
    and declares the two extensions our writer uses. Both
    `cut_openings=False` (default-instancing-on) and `cut_openings=True`
    (instancing-off because cuts diverge per-product geometry) must
    write a syntactically valid .glb."""
    import json
    import struct

    p = _write_mm_cube(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    for cut in (False, True):
        out = tmp_path / f"cube_cut_{cut}.glb"
        stats = m.to_gltf(out, cut_openings=cut)
        assert out.exists()
        assert stats["out_size_bytes"] == out.stat().st_size
        assert stats["products_emitted"] >= 1
        # cut_openings forces instancing off.
        assert stats["instancing"] is (not cut)

        # glTF magic + chunk shapes.
        with open(out, "rb") as f:
            data = f.read()
        magic, ver, total = struct.unpack("<III", data[:12])
        assert magic == 0x46546C67  # 'glTF'
        assert ver == 2
        assert total == len(data)
        jlen, jtype = struct.unpack("<II", data[12:20])
        assert jtype == 0x4E4F534A  # 'JSON'
        obj = json.loads(data[20:20 + jlen].rstrip(b" "))
        # KHR_mesh_quantization fires whenever any baked node exists —
        # which it always does on a real file.
        assert "KHR_mesh_quantization" in obj.get("extensionsRequired", [])
        # EXT_mesh_gpu_instancing only fires when instancing is on AND
        # at least one rep group has ≥2 members. Single-wall fixture
        # has only one product, so the extension is absent.
        # The minimum sanity check is that it shows up in
        # extensionsUsed only when the plan emitted at least one
        # instanced group.


def test_bundle_then_clash_roundtrip(tmp_path):
    """The documented bundle -> clash chain must actually be invokable
    from the wheel (GH #41). Before v0.4.28, `ifcfast.clash()` was in
    __all__ with examples but `ifcfast.bundle()` didn't exist — calling
    clash() raised FileNotFoundError because nothing in the package
    could produce instances.parquet / representations.parquet.
    """
    pytest.importorskip("pandas")

    info = ifcfast.bundle(FIXTURE, tmp_path / "minimal.bundle")
    assert info["bundle_dir"] == str(tmp_path / "minimal.bundle")
    assert Path(info["instances_parquet"]).is_file()
    assert Path(info["representations_parquet"]).is_file()
    assert Path(info["view_sql"]).is_file()
    # Minimal fixture is one geometry-free product, but the substrate
    # itself must be written (1 instance row, 0 rep rows).
    assert info["instances_written"] >= 1

    df = ifcfast.clash(info["bundle_dir"])
    # The fixture has no overlapping geometry, so we expect zero pairs —
    # the assertion that matters is that clash() *ran* against the
    # bundle we just wrote rather than raising FileNotFoundError.
    assert len(df) == 0
    assert df.attrs["pair_count"] == 0


def test_bundle_default_out_dir(tmp_path):
    """Omitting out_dir writes to {stem}.bundle next to the IFC."""
    pytest.importorskip("pandas")

    src = tmp_path / "copy.ifc"
    src.write_bytes(FIXTURE.read_bytes())
    info = ifcfast.bundle(src)
    expected = tmp_path / "copy.bundle"
    assert Path(info["bundle_dir"]) == expected
    assert (expected / "instances.parquet").is_file()
    assert (expected / "representations.parquet").is_file()


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


# ---------------------------------------------------------------------------
# GH #38 — IfcPropertyTableValue, IfcComplexProperty, unhandled markers
# ---------------------------------------------------------------------------


_GH38_PSET_X = """ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('gh38_pset_x.ifc','2026-06-03T00:00:00',('t'),('t'),'ifcfast','ifcfast','');
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
#50=IFCWALL('7Wall00000000000000001',$,'host',$,$,#15,$,'tag',.STANDARD.);
#100=IFCPROPERTYSINGLEVALUE('S',$,IFCLABEL('hello'),$);
#101=IFCPROPERTYTABLEVALUE('TableProp',$,(IFCREAL(0.),IFCREAL(1.),IFCREAL(2.)),(IFCREAL(10.),IFCREAL(20.),IFCREAL(30.)),$,$,$,.LINEAR.);
#102=IFCPROPERTYSINGLEVALUE('Inner1',$,IFCLABEL('inner_value'),$);
#103=IFCCOMPLEXPROPERTY('ComplexProp',$,'GROUP',(#102));
#104=IFCPROPERTYREFERENCEVALUE('RefProp',$,$,$);
#110=IFCPROPERTYSET('1PSet000000000000000X1',$,'Pset_X',$,(#100,#101,#103,#104));
#120=IFCRELDEFINESBYPROPERTIES('1RDP000000000000000X1',$,$,$,(#50),#110);
ENDSEC;
END-ISO-10303-21;
"""


def test_psets_gh38_table_complex_and_unhandled_marker(tmp_path):
    """GH #38 acceptance: a Pset_X carrying all four shapes — single,
    table, complex (with one nested single), and one IfcSimpleProperty
    leaf ifcfast has no parser for (IfcPropertyReferenceValue) — must
    emit one row each:

    - `S` → captured (single value baseline)
    - `TableProp` → captured with `value = "d=>v, ..."` and the
      DefinedValues axis type
    - `ComplexProp.Inner1` → captured via dot-joined flattening
      (the wrapper name does NOT appear on its own; nested leaves carry
      the prefix so consumers can query `prop_name.startswith(...)`)
    - `RefProp` → captured as a marker row with
      `value_type = "unhandled:IFCPROPERTYREFERENCEVALUE"` and
      `value = None`

    Pre-#38: every shape but `S` was silently dropped, with no marker.
    Ed re-tested v0.4.29 and reported ComplexProp / Inner1 still
    missing — the flattening convention (Ed himself asked for "handle
    IfcComplexProperty (flatten nested)") writes them under the
    `ComplexProp.Inner1` joined name, not as separate `ComplexProp`
    + `Inner1` rows. This test pins the convention so the captured /
    not-captured boundary is unambiguous on future reads.
    """
    p = tmp_path / "gh38_pset_x.ifc"
    p.write_text(_GH38_PSET_X, encoding="utf-8")
    m = ifcfast.open(p, use_cache=False, write_cache=False)
    df = m.psets
    rows = {r.prop_name: (r.value, r.value_type) for r in df.itertuples()}

    assert "S" in rows
    assert rows["S"] == ("hello", "IfcLabel")

    assert "TableProp" in rows
    table_val, table_type = rows["TableProp"]
    # Pairing convention is "defining=>defined, ..." in input order.
    assert table_val == "0=>10, 1=>20, 2=>30"
    # value_type follows the DefinedValues axis (the payload).
    assert table_type == "IfcReal"

    # Dot-joined flatten: wrapper name does NOT appear as a row, but
    # the inner leaf carries the wrapper as a prefix.
    assert "ComplexProp" not in rows, (
        "complex wrapper should not appear as a standalone row — the "
        "convention is to flatten leaves with dot-joined names"
    )
    assert "Inner1" not in rows, (
        "nested leaf should carry the wrapper prefix, not appear "
        "ungrouped at top level"
    )
    assert "ComplexProp.Inner1" in rows, (
        "nested leaves must surface under `Wrapper.Leaf` so consumers "
        "can recover the grouping by filtering "
        "prop_name.startswith('ComplexProp.')"
    )
    assert rows["ComplexProp.Inner1"] == ("inner_value", "IfcLabel")

    # Unhandled-marker row for the IfcPropertyReferenceValue we don't
    # parse. Markers make the blind spot enumerable from Python
    # without re-parsing the file.
    import math
    assert "RefProp" in rows
    ref_val, ref_type = rows["RefProp"]
    # value is null in arrow → NaN after pandas round-trip.
    assert ref_val is None or (isinstance(ref_val, float) and math.isnan(ref_val))
    assert ref_type == "unhandled:IFCPROPERTYREFERENCEVALUE"

    # Enumerate blind spots — documented filter in CHANGELOG / docs.
    blind = df[df["value_type"].fillna("").str.startswith("unhandled:")]
    assert len(blind) == 1
    assert blind.iloc[0]["prop_name"] == "RefProp"
