"""Agent-friendliness surface — bundled fixture, summary/schemas/preview,
CLI ``--json`` shape, ``system_prompt`` stability."""

from __future__ import annotations

import json
import subprocess
import sys

import ifcfast


def test_example_path_resolves_and_parses():
    path = ifcfast.example_path()
    assert path.exists(), f"bundled fixture missing: {path}"
    assert path.suffix == ".ifc"
    m = ifcfast.open(path, use_cache=False, write_cache=False)
    assert len(m) >= 1
    assert m.schema  # non-empty


def test_system_prompt_is_stable_text():
    p = ifcfast.system_prompt()
    assert isinstance(p, str) and len(p) > 100
    # Core promises that future versions must keep mentioning.
    for needle in (
        "ifcfast.open", "m.summary()", "m.schemas",
        "m.contained_in", "m.aggregates", "m.parent", "m.ancestors",
        # Geometry surface + agent-first framing.
        "m.meshes()", "m.point_cloud", "m.mesh_qto",
        "agent-first",
    ):
        assert needle in p, f"system_prompt missing: {needle}"


def test_system_prompt_makes_no_unverified_claims():
    """The agent helper feeds straight into an LLM's context. It must
    NOT assert performance or parity claims ifcfast can't back — those
    would be repeated as fact. ifcfast is untested; the prompt says so
    and points at cross-checking instead.
    """
    p = ifcfast.system_prompt().lower()
    for forbidden in (
        "20-30", "20–30", "fastest", "byte-level", "parity",
        "× faster", "x faster",
    ):
        assert forbidden not in p, (
            f"system_prompt makes an unverified claim: {forbidden!r}"
        )
    # It should set honest expectations instead.
    assert "provisional" in p or "cross-check" in p


def test_summary_shape(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    s = m.summary()
    # Required keys.
    for k in (
        "path", "schema", "products", "storeys", "tables",
        "top_types", "parse_seconds",
    ):
        assert k in s, f"summary() missing key: {k}"
    # Every table entry has rows + columns + loaded.
    for name, meta in s["tables"].items():
        assert {"rows", "columns", "loaded"} <= set(meta.keys()), name
    # JSON-serialisable.
    json.dumps(s, default=str)


def test_schemas_has_dtypes_per_column(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    sch = m.schemas
    for name in (
        "products", "storeys",
        "contained_in", "aggregates", "storey_building",
        "psets", "quantities", "materials", "classifications", "drift", "segments",
    ):
        assert name in sch, f"schemas missing: {name}"
        entry = sch[name]
        assert "columns" in entry and "dtypes" in entry
        assert "loaded" in entry
        for col in entry["columns"]:
            assert col in entry["dtypes"], f"{name}.{col} missing dtype"


def test_preview_returns_listed_dicts(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    rows = m.preview("products", n=1)
    assert isinstance(rows, list) and len(rows) == 1
    assert "guid" in rows[0] and "entity" in rows[0]
    # Relationship table — should not trigger extract.
    assert isinstance(m.preview("aggregates", n=2), list)
    # Unknown table is loud (GH #71).
    import pytest
    with pytest.raises(ValueError, match="Unknown table"):
        m.preview("nope")


def test_cli_demo_json_is_valid_json():
    out = subprocess.check_output(
        [sys.executable, "-m", "ifcfast.cli", "demo", "--json"],
        text=True,
    )
    payload = json.loads(out)
    assert payload["schema"] == "IFC4"
    assert "tables" in payload


def test_cli_schema_json_for_bundled_file(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    out = subprocess.check_output(
        [sys.executable, "-m", "ifcfast.cli", "schema",
         str(ifcfast.example_path()), "--json", "--no-cache"],
        text=True,
        env={**__import__("os").environ, "IFCFAST_CACHE": str(tmp_path / "cache")},
    )
    payload = json.loads(out)
    assert "schemas" in payload
    assert "contained_in" in payload["schemas"]


def test_cli_index_json_shape(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    out = subprocess.check_output(
        [sys.executable, "-m", "ifcfast.cli", "index",
         str(ifcfast.example_path()), "--json", "--no-cache"],
        text=True,
        env={**__import__("os").environ, "IFCFAST_CACHE": str(tmp_path / "cache")},
    )
    payload = json.loads(out)
    for k in ("path", "schema", "products", "tables"):
        assert k in payload
