"""GH #162 — cluster of small Python-layer defects.

Each test names the concrete failure mode it locks down:

* `federate(out_dir=…)` pointed at a constituent destroyed that bundle.
* Only `instances` was schema-checked; a `representations` mismatch
  surfaced as an opaque `concat_tables` error naming no bundle.
* `unit_scale` was compared as a metadata STRING, so `"0.001"` vs
  `"1e-3"` false-positived as a mixed-unit federation.
* The federation cache had no `(size, mtime)` revalidation, unlike the
  parse cache.
* The CLI did not catch `ifcfast.IfcfastError` — the class AGENTS.md
  tells agents to catch.
* `Model.products_df` assigned `_products_df` on a COLD-PARSE model,
  silently switching `filter()` / `__iter__` to the `iterrows()` path.
* `preview("drift")` returned `[]` for both "no rows" and "this build
  cannot produce drift".
"""

from __future__ import annotations

import json
import shutil
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import ifcfast
# NB: `ifcfast.federate` is the FUNCTION (re-exported in __init__),
# so the module's helpers are imported by name.
from ifcfast.federate import federation_cache_stale

FIXTURES = Path(__file__).parent / "fixtures"


def _bundle(fixture: str, out_dir: Path) -> Path:
    ifcfast.bundle(str(FIXTURES / fixture), out_dir=str(out_dir))
    return out_dir


@pytest.fixture()
def two_bundles(tmp_path: Path) -> tuple[Path, Path]:
    a = _bundle("geom_box.ifc", tmp_path / "box.bundle")
    b = _bundle("hotswap_body.ifc", tmp_path / "body.bundle")
    return a, b


# ----------------------------------------------------------------------
# federate()
# ----------------------------------------------------------------------


def test_federate_refuses_out_dir_equal_to_an_input(two_bundles):
    """Writing the merge into a constituent would destroy that source."""
    a, b = two_bundles
    before = (a / "instances.parquet").read_bytes()

    with pytest.raises(ValueError, match="one of the input bundles"):
        ifcfast.federate([a, b], a)

    assert (a / "instances.parquet").read_bytes() == before, (
        "the guard must fire BEFORE any write"
    )


def test_federate_refuses_out_dir_that_resolves_to_an_input(two_bundles):
    """Path spelling doesn't matter — the check resolves both sides."""
    a, b = two_bundles
    sneaky = a.parent / a.name / ".." / a.name
    with pytest.raises(ValueError, match="one of the input bundles"):
        ifcfast.federate([a, b], sneaky)


def test_federate_checks_representations_schema(two_bundles, tmp_path):
    """A representations mismatch names the bundle, not `concat_tables`."""
    a, b = two_bundles
    rep_path = b / "representations.parquet"
    t = pq.read_table(rep_path)
    widened = t.append_column(
        pa.field("bogus_extra_column", pa.int64()),
        pa.array([0] * t.num_rows, pa.int64()),
    )
    pq.write_table(widened, rep_path)

    with pytest.raises(ValueError, match="representations schema differs"):
        ifcfast.federate([a, b], tmp_path / "fed")


def test_unit_scale_compared_numerically(two_bundles, tmp_path):
    """`"1e-3"` and `"0.001"` are the same substrate, not mixed units."""
    a, b = two_bundles
    for d in (a, b):
        for table in ("instances", "representations"):
            f = d / f"{table}.parquet"
            t = pq.read_table(f)
            meta = dict(t.schema.metadata or {})
            # Same value, two spellings — one per bundle.
            meta[b"ifcfast.unit_scale"] = b"1.0" if d is a else b"1e0"
            pq.write_table(t.replace_schema_metadata(meta), f)

    sidecar = ifcfast.federate([a, b], tmp_path / "fed")
    assert float(sidecar["unit_scale"]) == 1.0


def test_unit_scale_still_rejects_genuinely_mixed_units(two_bundles, tmp_path):
    a, b = two_bundles
    for table in ("instances", "representations"):
        f = b / f"{table}.parquet"
        t = pq.read_table(f)
        meta = dict(t.schema.metadata or {})
        meta[b"ifcfast.unit_scale"] = b"0.001"
        pq.write_table(t.replace_schema_metadata(meta), f)

    with pytest.raises(ValueError, match="unit_scale differs"):
        ifcfast.federate([a, b], tmp_path / "fed")


# ----------------------------------------------------------------------
# federation cache revalidation
# ----------------------------------------------------------------------


def test_sidecar_records_source_stats(two_bundles, tmp_path):
    a, b = two_bundles
    sidecar = ifcfast.federate([a, b], tmp_path / "fed")
    stats = sidecar["source_stats"]
    assert set(stats) == {a.name, b.name}
    for d in (a, b):
        for table in ("instances", "representations"):
            size, mtime = stats[d.name][table]
            st = (d / f"{table}.parquet").stat()
            assert size == st.st_size
            assert mtime == st.st_mtime_ns
    # Round-trips through JSON as written to disk.
    on_disk = json.loads((tmp_path / "fed" / "federation.json").read_text())
    assert on_disk["source_stats"] == stats


def test_federation_cache_stale_detects_a_same_size_edit(two_bundles, tmp_path):
    """The exact hole the content key cannot see."""
    a, b = two_bundles
    sidecar = ifcfast.federate([a, b], tmp_path / "fed")
    assert not federation_cache_stale(sidecar, [a, b])

    # Same size, new mtime — a mid-file in-place edit looks like this.
    f = b / "instances.parquet"
    data = bytearray(f.read_bytes())
    f.write_bytes(bytes(data))
    assert federation_cache_stale(sidecar, [a, b])


def test_federation_cache_stale_on_a_sidecar_without_stats(two_bundles):
    """A pre-GH #162 sidecar carries no evidence → re-merge, don't trust."""
    a, b = two_bundles
    assert federation_cache_stale({"sources": []}, [a, b])


def test_federation_cache_stale_on_a_different_constituent_set(
    two_bundles, tmp_path
):
    a, b = two_bundles
    sidecar = ifcfast.federate([a, b], tmp_path / "fed")
    c = _bundle("geom_box.ifc", tmp_path / "third.bundle")
    assert federation_cache_stale(sidecar, [a, c])


# ----------------------------------------------------------------------
# CLI
# ----------------------------------------------------------------------


def test_cli_catches_ifcfast_error(monkeypatch, capsys, tmp_path):
    """`IfcfastError` exits 1 with `ifcfast: <msg>`, not a traceback."""
    from ifcfast import cli

    src = tmp_path / "x.ifc"
    shutil.copy(FIXTURES / "minimal.ifc", src)

    def _boom(*a, **kw):
        raise ifcfast.IfcfastError("native core exploded")

    monkeypatch.setattr(ifcfast, "open", _boom)
    rc = cli.main(["index", str(src)])
    assert rc == 1
    err = capsys.readouterr().err
    assert "ifcfast: native core exploded" in err
    assert "Traceback" not in err


# ----------------------------------------------------------------------
# Model.products_df must not switch the traversal path
# ----------------------------------------------------------------------


def test_products_df_does_not_downgrade_a_cold_parse_model():
    """GH #162: one `products_df` access used to pin the model to the
    row-wise `iterrows()` path for the rest of its life."""
    m = ifcfast.open(
        ifcfast.example_path(), use_cache=False, write_cache=False
    )
    assert m._products_df is None
    assert m._products_list, "cold parse populates the row list"

    df = m.products_df
    assert len(df) == len(m._products_list)
    assert m._products_df is None, (
        "the derived frame must not become the tier-1 source of truth"
    )
    assert m._products_df_view is not None, "and it must be memoised"
    assert m.products_df is df, "second access reuses the memoised frame"


def test_spaces_df_does_not_downgrade_a_cold_parse_model():
    """`spaces_df` reaches through `products_df` (model.py:1577)."""
    m = ifcfast.open(
        ifcfast.example_path(), use_cache=False, write_cache=False
    )
    m.spaces_df
    assert m._products_df is None


def test_iteration_matches_after_products_df_access():
    m = ifcfast.open(
        ifcfast.example_path(), use_cache=False, write_cache=False
    )
    before = [p.guid for p in m]
    m.products_df
    assert [p.guid for p in m] == before
    assert [p.guid for p in m.filter(entity="IfcWall")] == [
        p.guid for p in m.products if p.entity == "IfcWall"
    ]


# ----------------------------------------------------------------------
# preview(): empty is not the same as unavailable
# ----------------------------------------------------------------------


def test_preview_distinguishes_empty_from_unavailable(monkeypatch):
    m = ifcfast.open(
        ifcfast.example_path(), use_cache=False, write_cache=False
    )
    # Populated / empty layer → a list, however short.
    assert isinstance(m.preview("psets", n=2), list)

    # Layer that cannot be produced (no `mesh` Cargo feature) → loud.
    monkeypatch.setattr(type(m), "drift", property(lambda self: None))
    with pytest.raises(ValueError, match="unavailable on this build"):
        m.preview("drift", n=2)

    monkeypatch.setattr(type(m), "segments", property(lambda self: None))
    with pytest.raises(ValueError, match="unavailable on this build"):
        m.preview("segments", n=2)


# ----------------------------------------------------------------------
# MCP: bounded model cache + capped tree tools
# ----------------------------------------------------------------------


mcp_server = pytest.importorskip(
    "ifcfast.mcp_server", reason="mcp extra not installed"
)


def test_mcp_model_cache_is_bounded(tmp_path, monkeypatch):
    monkeypatch.setattr(mcp_server, "_MAX_OPEN_MODELS", 3)
    mcp_server._open_models.clear()

    paths = []
    for i in range(5):
        p = tmp_path / f"m{i}.ifc"
        shutil.copy(FIXTURES / "minimal.ifc", p)
        paths.append(p)
        mcp_server._resolve(str(p))

    assert len(mcp_server._open_models) == 3
    # Oldest two evicted, newest three retained.
    kept = set(mcp_server._open_models)
    assert {str(p.resolve()) for p in paths[2:]} == kept
    mcp_server._open_models.clear()


def test_mcp_model_cache_evicts_by_last_use(tmp_path, monkeypatch):
    monkeypatch.setattr(mcp_server, "_MAX_OPEN_MODELS", 2)
    mcp_server._open_models.clear()

    a, b, c = (tmp_path / f"{n}.ifc" for n in "abc")
    for p in (a, b, c):
        shutil.copy(FIXTURES / "minimal.ifc", p)

    mcp_server._resolve(str(a))
    mcp_server._resolve(str(b))
    mcp_server._resolve(str(a))   # touch a — b is now the LRU victim
    mcp_server._resolve(str(c))

    kept = set(mcp_server._open_models)
    assert str(a.resolve()) in kept
    assert str(c.resolve()) in kept
    assert str(b.resolve()) not in kept
    mcp_server._open_models.clear()


@pytest.mark.parametrize(
    "tool", ["children", "descendants", "products_in"]
)
def test_mcp_tree_tools_take_a_limit(tool):
    """Every row-returning MCP tool caps output (GH #162)."""
    import inspect

    fn = getattr(mcp_server, tool)
    # FastMCP wraps the callable; the signature is preserved on .fn.
    target = getattr(fn, "fn", fn)
    params = inspect.signature(target).parameters
    assert "limit" in params, f"{tool} has no limit"
    assert params["limit"].default == 200


def test_mcp_descendants_respects_limit():
    guide_path = str(ifcfast.example_path())
    m = ifcfast.open(guide_path)
    root = m.ancestors(m.products[0].guid)[-1]
    fn = getattr(mcp_server.descendants, "fn", mcp_server.descendants)
    assert len(fn(guide_path, root, limit=1)) <= 1
    mcp_server._open_models.clear()


def test_mcp_exposes_a_classifications_tool():
    fn = getattr(mcp_server.classifications, "fn", mcp_server.classifications)
    rows = fn(str(ifcfast.example_path()))
    assert isinstance(rows, list)
    mcp_server._open_models.clear()
