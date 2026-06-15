"""Regression tests for the GH #71 minor batch, items 4-8.

Items 1-3 (loud failures on typo'd table / entity / mode + top-level
namespace hygiene) shipped in PR #85 and are covered by
``test_contract_hardening.py``. This file covers what remained:

* (4) ``use_cache=False, write_cache=False`` never resolves the cache
  root / home directory — not even on lazy data-layer access.
* (5) duplicate STEP ids collapse last-wins, keep ``step_id`` unique,
  and surface a ``duplicate_step_ids`` count in ``summary()``.
* (6) empty data layers report canonical column dtypes, not the
  all-``float64`` empty-DataFrame default.
* (7) ``spaces_df`` carries name/storey joined from ``products``.
* (8) a multi-member ifczip warns which member it used.
"""

from __future__ import annotations

import warnings
import zipfile
from pathlib import Path

import pytest

import ifcfast

FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"


# ----------------------------------------------------------------------
# (4) no-cache open must not require a resolvable home directory, even
# when the lazy data layers are later accessed.
# ----------------------------------------------------------------------


def test_no_cache_data_layers_skip_home_dir(tmp_path, monkeypatch):
    """`m.psets` on a no-cache model must not touch the cache root.

    `cache_root()` → `Path.home()` raises `RuntimeError` in an env with
    no resolvable home (stripped CI containers). With cache disabled we
    must never go there — not on open, not on lazy extract.
    """

    def _boom():
        raise RuntimeError("Could not determine home directory")

    monkeypatch.setattr("pathlib.Path.home", staticmethod(_boom))
    # Belt-and-braces: also strip the env override path.
    monkeypatch.delenv("IFCFAST_CACHE", raising=False)

    m = ifcfast.open(FIXTURE, use_cache=False, write_cache=False)
    # The lazy data layers used to eagerly mkdir the cache dir.
    assert m.psets is not None
    assert m.quantities is not None
    assert m.materials is not None
    assert m.classifications is not None


def test_cache_dir_resolved_when_cache_requested(tmp_path, monkeypatch):
    """The home-dir laziness must not break the normal cached path."""
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(FIXTURE)  # default use_cache/write_cache=True
    _ = m.psets
    assert (tmp_path / "cache").exists()


# ----------------------------------------------------------------------
# (5) duplicate STEP ids → last-wins dedup + duplicate_step_ids count.
# ----------------------------------------------------------------------


def _dup_wall_fixture(tmp_path) -> Path:
    src = FIXTURE.read_text()
    wall = (
        "#30=IFCWALL('7XvctVUKr0kugbFTf53O9L',$,'Wall-001',$,$,#16,$,"
        "'tag-001',.STANDARD.);"
    )
    dup = (
        wall
        + "\n#30=IFCWALL('ZZZctVUKr0kugbFTf53O9L',$,'Wall-DUP',$,$,#16,$,"
        "'tag-dup',.STANDARD.);"
    )
    out = tmp_path / "dup_step.ifc"
    out.write_text(src.replace(wall, dup))
    return out


def test_duplicate_step_ids_last_wins(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    p = _dup_wall_fixture(tmp_path)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    assert m.summary()["duplicate_step_ids"] == 1

    # step_id is a unique key column again.
    sids = [pr.step_id for pr in m.products]
    assert len(sids) == len(set(sids))

    # Last declaration wins.
    walls = [pr for pr in m.products if pr.entity == "IfcWall"]
    assert len(walls) == 1
    assert walls[0].guid == "ZZZctVUKr0kugbFTf53O9L"


def test_duplicate_step_ids_persist_through_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    p = _dup_wall_fixture(tmp_path)
    cold = ifcfast.open(p)  # writes cache
    assert cold.summary()["duplicate_step_ids"] == 1
    warm = ifcfast.open(p)  # cache hit
    assert warm.summary()["duplicate_step_ids"] == 1


def test_well_formed_file_reports_zero_duplicates(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(FIXTURE, use_cache=False, write_cache=False)
    assert m.summary()["duplicate_step_ids"] == 0


# ----------------------------------------------------------------------
# (6) empty data layers report canonical dtypes, not all-float64.
# ----------------------------------------------------------------------


def test_empty_layers_have_canonical_dtypes(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(FIXTURE, use_cache=False, write_cache=False)

    # minimal.ifc has no geometry → drift / segments are empty.
    drift = m.drift
    seg = m.segments
    assert drift is not None and len(drift) == 0
    assert seg is not None and len(seg) == 0

    # The empty-DataFrame default would type every column float64.
    assert str(drift.dtypes["guid"]) == "object"
    assert str(drift.dtypes["triangle_count"]) == "int64"
    assert str(drift.dtypes["surface_area_m2"]) == "float64"
    assert str(seg.dtypes["guid"]) == "object"
    assert str(seg.dtypes["product_index"]) == "int64"

    # schemas() must reflect the same canonical dtypes once loaded.
    sch = m.schemas
    assert sch["drift"]["dtypes"]["guid"] == "object"
    assert sch["drift"]["dtypes"]["triangle_count"] == "int64"
    assert sch["quantities"]["dtypes"]["guid"] == "object"
    assert sch["quantities"]["dtypes"]["unit_step_id"] == "float64"


def test_empty_layer_dtypes_survive_cache_roundtrip(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    cold = ifcfast.open(FIXTURE)
    _ = cold.drift  # forces write of empty drift parquet
    warm = ifcfast.open(FIXTURE)
    d = warm.drift
    assert len(d) == 0
    assert str(d.dtypes["guid"]) == "object"
    assert str(d.dtypes["triangle_count"]) == "int64"


# ----------------------------------------------------------------------
# (7) spaces_df carries name/storey joined from products.
# ----------------------------------------------------------------------

_SPACE_FIXTURE = """\
ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('s.ifc','2026-06-13T00:00:00',(''),(''),'ifcfast','t','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0PROJgUIDgUIDgUIDgUID0',$,'P',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#11=IFCBUILDING('2BLDGgUIDgUIDgUIDgUI0',$,'B',$,$,$,$,$,.ELEMENT.,$,$,$);
#12=IFCBUILDINGSTOREY('3STRYgUIDgUIDgUIDgUI0',$,'Plan 01',$,$,$,$,$,.ELEMENT.,0.0);
#20=IFCRELAGGREGATES('4AGGRgUIDgUIDgUIDgUI0',$,$,$,#1,(#11));
#21=IFCRELAGGREGATES('5AGGRgUIDgUIDgUIDgUI0',$,$,$,#11,(#12));
#30=IFCSPACE('6SPACgUIDgUIDgUIDgUI0',$,'Room 101',$,$,$,$,'Office',.ELEMENT.,.INTERNAL.,$);
#31=IFCRELCONTAINEDINSPATIALSTRUCTURE('7CONTgUIDgUIDgUIDgU0',$,$,$,(#30),#12);
ENDSEC;
END-ISO-10303-21;
"""


def test_spaces_df_has_name_and_storey(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    p = tmp_path / "space.ifc"
    p.write_text(_SPACE_FIXTURE)
    m = ifcfast.open(p, use_cache=False, write_cache=False)

    df = m.spaces_df
    for col in ("guid", "step_id", "name", "storey_guid", "storey_name"):
        assert col in df.columns

    if len(df) > 0:
        row = df[df["guid"] == "6SPACgUIDgUIDgUIDgUI0"]
        if len(row) > 0:
            assert row.iloc[0]["name"] == "Room 101"

    # schemas() advertises the enriched column set.
    assert m.schemas["spaces"]["columns"] == [
        "guid", "step_id", "name", "storey_guid", "storey_name"
    ]
    assert m.summary()["tables"]["spaces"]["columns"] == [
        "guid", "step_id", "name", "storey_guid", "storey_name"
    ]


def test_spaces_df_empty_has_join_columns(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(FIXTURE, use_cache=False, write_cache=False)  # no spaces
    df = m.spaces_df
    for col in ("guid", "step_id", "name", "storey_guid", "storey_name"):
        assert col in df.columns


# ----------------------------------------------------------------------
# (8) multi-member ifczip warns which member was used.
# ----------------------------------------------------------------------


def test_multi_member_ifczip_warns(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    body = FIXTURE.read_bytes()
    z = tmp_path / "two_models.ifczip"
    with zipfile.ZipFile(z, "w") as zf:
        zf.writestr("a.ifc", body)
        zf.writestr("b.ifc", body + b"\n/* padding to make b the largest */")

    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        ifcfast.open(z, use_cache=False, write_cache=False)

    msgs = [str(w.message) for w in caught if "STEP members" in str(w.message)]
    assert msgs, "expected a multi-member ifczip warning"
    assert "b.ifc" in msgs[0]  # the largest member
    assert "a.ifc" in msgs[0]  # the ignored member named


def test_single_member_ifczip_does_not_warn(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    body = FIXTURE.read_bytes()
    z = tmp_path / "one_model.ifczip"
    with zipfile.ZipFile(z, "w") as zf:
        zf.writestr("only.ifc", body)

    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        ifcfast.open(z, use_cache=False, write_cache=False)

    msgs = [str(w.message) for w in caught if "STEP members" in str(w.message)]
    assert not msgs
