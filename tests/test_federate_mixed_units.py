"""GH #169: mixed-unit federation.

Real projects mix authoring units across disciplines (the
buildingSMART Medical-Dental Clinic sample is ARK/STR/EL in metres,
HVAC/PL in millimetres — Revit 2011 vs 2013 exports), so refusing to
merge them was the wrong policy. ``ifcfast.federate`` now converts
every source-unit length into the FINEST constituent's unit.

What this gate holds:

1. Coarse sources are scaled UP by ``unit_scale_src / unit_scale_tgt``
   — vertices, local + world bboxes, centroid, placement, and the
   transform's TRANSLATION column only.
2. QTO columns (m² / m³) and ``materials[].thickness_mm`` are
   unit-independent already and must come through untouched.
3. The finest source is bit-preserved; the merged tables are stamped
   with the target unit; the sidecar records per-source unit + factor.
4. The all-same-unit merge is BYTE-IDENTICAL to the pre-#169 merge —
   the rescale pass is never even entered.
5. An unresolvable unit still raises (fail loudly).

Everything runs on committed tiny fixtures: ``geom_box.ifc`` is
metre-authored, ``hotswap_roundtrip.ifc`` millimetre-authored.
"""

from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

import numpy as np
import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import ifcfast
import ifcfast.federate  # noqa: F401  (module, not the re-exported function)

fedmod = sys.modules["ifcfast.federate"]

FIXTURES = Path(__file__).parent / "fixtures"

_QTO_COLUMNS = (
    "volume_m3",
    "aabb_volume_m3",
    "surface_area_m2",
    "area_top_m2",
    "area_bottom_m2",
    "area_side_m2",
    "area_inclined_m2",
    "largest_surface_m2",
    "smallest_surface_m2",
    "volume_mesh_m3",
    "volume_prism_bound_m3",
)


def _bundle(fixture: str, out_dir: Path) -> Path:
    src = FIXTURES / fixture
    assert src.is_file(), f"missing committed fixture {src}"
    ifcfast.bundle(str(src), out_dir=str(out_dir))
    return out_dir


def _up(values, factor: float) -> np.ndarray:
    """The expected f32 result of scaling, computed independently of
    the implementation helper (f64 multiply, one f32 rounding)."""
    return (np.asarray(values, dtype=np.float64) * factor).astype(np.float32)


def _fsl(t: pa.Table, name: str) -> np.ndarray:
    return np.asarray(t.column(name).to_pylist(), dtype=np.float32)


def _verts(t: pa.Table, row: int) -> np.ndarray:
    return np.frombuffer(t.column("vertices_le")[row].as_py(), dtype="<f4")


@pytest.fixture()
def metre_and_mm(tmp_path: Path) -> tuple[Path, Path]:
    """A metre-authored and a millimetre-authored bundle, disjoint guids."""
    m = _bundle("geom_box.ifc", tmp_path / "metre.bundle")
    mm = _bundle("hotswap_roundtrip.ifc", tmp_path / "mm.bundle")
    assert pq.read_schema(m / "instances.parquet").metadata[
        b"ifcfast.unit_scale"
    ] == b"1"
    assert pq.read_schema(mm / "instances.parquet").metadata[
        b"ifcfast.unit_scale"
    ] == b"0.001"
    return m, mm


# -- 1. the rescale itself --------------------------------------------------


def test_mixed_units_federate_to_the_finest_unit(metre_and_mm, tmp_path):
    m, mm = metre_and_mm
    fed = tmp_path / "fed"
    sidecar = ifcfast.federate([m, mm], fed)

    # Target = the FINEST constituent (mm), coarse source scaled UP.
    assert sidecar["unit_scale"] == "0.001"
    assert sidecar["unit_scales"] == {"metre.bundle": "1", "mm.bundle": "0.001"}
    assert sidecar["unit_factors"] == {"metre.bundle": 1000.0, "mm.bundle": 1.0}
    on_disk = json.loads((fed / "federation.json").read_text())
    assert on_disk["unit_scales"] == sidecar["unit_scales"]
    assert on_disk["unit_factors"] == sidecar["unit_factors"]

    # Both merged parquets are stamped with the target unit — this is
    # what the Rust clash engine reads to convert to metres.
    for table in ("instances", "representations"):
        meta = pq.read_schema(fed / f"{table}.parquet").metadata
        assert meta[b"ifcfast.unit_scale"] == b"0.001", table
        # Other metadata keys (ifcfast.version) survive the stamp.
        assert b"ifcfast.version" in meta

    src = pq.read_table(m / "instances.parquet")
    got = pq.read_table(fed / "instances.parquet")
    got_m = got.filter(pa.compute.equal(got.column("source_model"), "metre.bundle"))
    assert got_m.num_rows == src.num_rows > 0

    for col in ("bbox_min_xyz", "bbox_max_xyz", "centroid_xyz", "placement_xyz"):
        np.testing.assert_array_equal(
            _fsl(got_m, col), _up(_fsl(src, col), 1000.0), err_msg=col
        )

    # transform: rotation/scale block untouched, translation ×1000.
    tr_src = np.asarray(src.column("transform").to_pylist(), np.float32).reshape(-1, 16)
    tr_got = np.asarray(got_m.column("transform").to_pylist(), np.float32).reshape(
        -1, 16
    )
    keep = [i for i in range(16) if i not in (12, 13, 14)]
    np.testing.assert_array_equal(tr_got[:, keep], tr_src[:, keep])
    np.testing.assert_array_equal(tr_got[:, 12:15], _up(tr_src[:, 12:15], 1000.0))

    # Representations of the metre source: vertices + local bbox ×1000,
    # topology (indices, counts, segments) byte-identical.
    rsrc = pq.read_table(m / "representations.parquet")
    rgot = pq.read_table(fed / "representations.parquet")
    assert rgot.num_rows >= rsrc.num_rows
    for i in range(rsrc.num_rows):
        np.testing.assert_array_equal(_verts(rgot, i), _up(_verts(rsrc, i), 1000.0))
        assert (
            rgot.column("indices_le")[i].as_py()
            == rsrc.column("indices_le")[i].as_py()
        )
        assert rgot.column("segments")[i].as_py() == rsrc.column("segments")[i].as_py()
    for col in ("local_bbox_min_xyz", "local_bbox_max_xyz"):
        np.testing.assert_array_equal(
            _fsl(rgot, col)[: rsrc.num_rows], _up(_fsl(rsrc, col), 1000.0), err_msg=col
        )

    # Dtypes/shapes are preserved EXACTLY — the Rust substrate reader
    # rejects a silently widened arrow type.
    assert got.schema.remove_metadata() == src.schema.remove_metadata()
    assert rgot.schema.remove_metadata() == rsrc.schema.remove_metadata()


def test_qto_and_thickness_columns_are_not_rescaled(metre_and_mm, tmp_path):
    """m² / m³ are unit-independent; ``thickness_mm`` is normalised to
    millimetres by the extractor. Scaling either would corrupt them."""
    m, mm = metre_and_mm
    fed = tmp_path / "fed"
    ifcfast.federate([m, mm], fed)

    src = pq.read_table(m / "instances.parquet")
    got = pq.read_table(fed / "instances.parquet")
    got_m = got.filter(pa.compute.equal(got.column("source_model"), "metre.bundle"))
    for col in _QTO_COLUMNS:
        a = np.asarray(src.column(col).to_pylist(), np.float64)
        b = np.asarray(got_m.column(col).to_pylist(), np.float64)
        # NaN-safe: volume_prism_bound_m3 is NaN where no bound exists.
        np.testing.assert_array_equal(np.isnan(a), np.isnan(b), err_msg=col)
        np.testing.assert_array_equal(a[~np.isnan(a)], b[~np.isnan(b)], err_msg=col)
    assert got_m.column("surfaces").to_pylist() == src.column("surfaces").to_pylist()
    assert got_m.column("materials").to_pylist() == src.column("materials").to_pylist()
    assert (
        got_m.column("quantities").to_pylist() == src.column("quantities").to_pylist()
    )


def test_finest_source_rows_pass_through_untouched(metre_and_mm, tmp_path):
    m, mm = metre_and_mm
    fed = tmp_path / "fed"
    ifcfast.federate([m, mm], fed)

    src = pq.read_table(mm / "instances.parquet")
    got = pq.read_table(fed / "instances.parquet")
    got_mm = got.filter(pa.compute.equal(got.column("source_model"), "mm.bundle"))
    for col in (
        "bbox_min_xyz",
        "bbox_max_xyz",
        "centroid_xyz",
        "placement_xyz",
        "transform",
    ):
        np.testing.assert_array_equal(_fsl(got_mm, col), _fsl(src, col), err_msg=col)


def test_metre_round_trip_recovers_the_millimetre_source(metre_and_mm, tmp_path):
    """Nonzero translations (10 000 mm) and real vertices survive a
    mm → m → mm round trip through the merge.

    The metre copy is manufactured by scaling the mm bundle DOWN; the
    federation then scales it back UP, and the assertion is against the
    untouched original — so the check is a round trip, not a tautology.
    """
    _, mm = metre_and_mm
    copy = tmp_path / "as_metres.bundle"
    copy.mkdir()
    for table in ("instances", "representations"):
        t = pq.read_table(mm / f"{table}.parquet")
        t = fedmod._rescale_table(t, table, 0.001)
        pq.write_table(fedmod._stamp_unit_scale(t, "1"), copy / f"{table}.parquet")

    src = pq.read_table(mm / "instances.parquet")
    tr_src = np.asarray(src.column("transform").to_pylist(), np.float32).reshape(-1, 16)
    assert np.abs(tr_src[:, 12:15]).max() > 100.0, "fixture must have a real translation"

    fed = tmp_path / "fed"
    with pytest.warns(UserWarning, match="guid"):
        sidecar = ifcfast.federate([copy, mm], fed, on_collision="warn")
    assert sidecar["unit_factors"] == {"as_metres.bundle": 1000.0, "mm.bundle": 1.0}

    got = pq.read_table(fed / "instances.parquet")
    got_c = got.filter(pa.compute.equal(got.column("source_model"), "as_metres.bundle"))
    for col in ("bbox_min_xyz", "bbox_max_xyz", "centroid_xyz", "placement_xyz"):
        np.testing.assert_allclose(
            _fsl(got_c, col), _fsl(src, col), rtol=1e-5, atol=1e-3, err_msg=col
        )
    tr_got = np.asarray(got_c.column("transform").to_pylist(), np.float32).reshape(
        -1, 16
    )
    np.testing.assert_allclose(tr_got, tr_src, rtol=1e-5, atol=1e-3)

    rsrc = pq.read_table(mm / "representations.parquet")
    rgot = pq.read_table(fed / "representations.parquet")
    for i in range(rsrc.num_rows):
        np.testing.assert_allclose(
            _verts(rgot, i), _verts(rsrc, i), rtol=1e-5, atol=1e-3
        )


# -- 2. same-unit path is untouched ----------------------------------------


def test_same_unit_merge_never_enters_the_rescale_pass(tmp_path, monkeypatch):
    """The bitwise parity gate for single-unit federations: with every
    constituent already at the target unit the rescale helper is not
    called at all, and the bytes match a run with it disabled."""
    a = _bundle("geom_box.ifc", tmp_path / "box.bundle")
    b = _bundle("hotswap_body.ifc", tmp_path / "body.bundle")

    plain = tmp_path / "fed_plain"
    sidecar = ifcfast.federate([a, b], plain)
    assert sidecar["unit_factors"] == {"box.bundle": 1.0, "body.bundle": 1.0}
    assert sidecar["unit_scale"] == "1"

    def _boom(*_a, **_kw):  # pragma: no cover - the point is it never runs
        raise AssertionError("rescale pass ran on a single-unit federation")

    monkeypatch.setattr(fedmod, "_rescale_table", _boom)
    disabled = tmp_path / "fed_disabled"
    ifcfast.federate([a, b], disabled)

    for table in ("instances", "representations"):
        assert (plain / f"{table}.parquet").read_bytes() == (
            disabled / f"{table}.parquet"
        ).read_bytes(), f"{table}: single-unit merge is not byte-identical"
        # And the metadata is the untouched first-constituent stamp.
        assert pq.read_schema(plain / f"{table}.parquet").metadata == pq.read_schema(
            a / f"{table}.parquet"
        ).metadata


def test_federation_cache_key_is_versioned(tmp_path, monkeypatch):
    """A change in unit handling must move the federated cache key —
    otherwise a pre-#169 cached merge is served for a mixed-unit list."""
    a = _bundle("geom_box.ifc", tmp_path / "box.bundle")
    b = _bundle("hotswap_roundtrip.ifc", tmp_path / "mm.bundle")
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    before = fedmod.federation_cache_dir([a, b], "warn")
    monkeypatch.setattr(fedmod, "_FEDERATION_VERSION", fedmod._FEDERATION_VERSION + 1)
    assert fedmod.federation_cache_dir([a, b], "warn") != before


# -- 3. clash across mixed units -------------------------------------------


def test_clash_list_sugar_federates_mixed_units(metre_and_mm, tmp_path, monkeypatch):
    m, mm = metre_and_mm
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    df = ifcfast.clash([m, mm], write_parquet=False)
    assert df is not None
    fed_dir = Path(df.attrs["federated_dir"])
    assert (fed_dir / "federation.json").is_file()
    assert df.attrs["federation"]["unit_scale"] == "0.001"
    assert df.attrs["federation"]["unit_factors"]["metre.bundle"] == 1000.0

    # Geometry sanity: after the merge both sources measure in the SAME
    # unit, so the metre source's boxes are now thousands of units wide,
    # not single digits.
    t = pq.read_table(fed_dir / "instances.parquet")
    mins = _fsl(t, "bbox_min_xyz")
    maxs = _fsl(t, "bbox_max_xyz")
    extent = np.abs(maxs - mins).max(axis=1)
    src_model = np.asarray(t.column("source_model").to_pylist())
    assert extent[src_model == "metre.bundle"].max() > 100.0


# -- 4. loud failures -------------------------------------------------------


def _rewrite_unit_scale(bundle_dir: Path, value: bytes | None, tables=("instances",)):
    for table in tables:
        f = bundle_dir / f"{table}.parquet"
        t = pq.read_table(f)
        meta = dict(t.schema.metadata or {})
        if value is None:
            meta.pop(b"ifcfast.unit_scale", None)
        else:
            meta[b"ifcfast.unit_scale"] = value
        pq.write_table(t.replace_schema_metadata(meta), f)


def test_missing_unit_scale_still_raises(metre_and_mm, tmp_path):
    m, mm = metre_and_mm
    _rewrite_unit_scale(m, None)
    with pytest.raises(ValueError, match="ifcfast.unit_scale"):
        ifcfast.federate([m, mm], tmp_path / "fed")


def test_non_numeric_unit_scale_still_raises(metre_and_mm, tmp_path):
    m, mm = metre_and_mm
    _rewrite_unit_scale(m, b"metres")
    with pytest.raises(ValueError, match="non-numeric"):
        ifcfast.federate([m, mm], tmp_path / "fed")


def test_non_positive_unit_scale_raises(metre_and_mm, tmp_path):
    m, mm = metre_and_mm
    _rewrite_unit_scale(m, b"0", tables=("instances", "representations"))
    with pytest.raises(ValueError, match="positive"):
        ifcfast.federate([m, mm], tmp_path / "fed")


def test_bundle_with_self_inconsistent_unit_scale_raises(metre_and_mm, tmp_path):
    """instances and representations disagreeing is a corrupt bundle,
    not a mixed-unit federation — it must not be silently averaged."""
    m, mm = metre_and_mm
    _rewrite_unit_scale(m, b"0.001", tables=("instances",))
    with pytest.raises(ValueError, match="disagree"):
        ifcfast.federate([m, mm], tmp_path / "fed")


def test_three_way_mixed_units_pick_the_global_minimum(tmp_path):
    m = _bundle("geom_box.ifc", tmp_path / "metre.bundle")
    mm = _bundle("hotswap_roundtrip.ifc", tmp_path / "mm.bundle")
    cm = tmp_path / "cm.bundle"
    shutil.copytree(m, cm)
    _rewrite_unit_scale(cm, b"0.01", tables=("instances", "representations"))

    fed = tmp_path / "fed"
    with pytest.warns(UserWarning, match="guid"):
        sidecar = ifcfast.federate([m, cm, mm], fed, on_collision="warn")
    assert sidecar["unit_scale"] == "0.001"
    assert sidecar["unit_factors"] == {
        "metre.bundle": 1000.0,
        "cm.bundle": 10.0,
        "mm.bundle": 1.0,
    }
