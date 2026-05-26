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
