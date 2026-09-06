"""GH #50 parity gate: ``ifcfast.federate`` vs the frozen oracle spec.

``tests/oracle/federate.py`` is the hand-merge recipe the clash oracle
harness was built on; the product ``ifcfast.federate`` is that module
promoted. This gate keeps them equal:

1. Product merge == oracle merge, table-for-table, INCLUDING arrow
   schema flags (the strict Rust substrate reader rejects silently
   widened types) and schema metadata (``ifcfast.unit_scale``).
2. ``clash([a, b])`` == ``clash(oracle_merged_dir)`` row-for-row.
3. Collision policies (``warn`` / ``fail`` / ``dedup``) behave as
   documented; the oracle only knows the keep-everything behaviour,
   which must match ``warn``.

Everything runs on committed tiny fixtures (bundling the SAME fixture
into two dirs guarantees both guid collisions and cross-model hard
clashes) — no corpus, no network.
"""

from __future__ import annotations

import shutil
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import ifcfast
from ifcfast.federate import federation_cache_dir

try:
    from tests.oracle.federate import federate_bundles as oracle_federate
except ImportError:  # bare `pytest` puts tests/, not the repo root, on sys.path
    from oracle.federate import federate_bundles as oracle_federate

FIXTURES = Path(__file__).parent / "fixtures"

_TABLES = ("instances", "representations")


def _table_bytes(t: pa.Table) -> bytes:
    """Deterministic byte serialization for bit-exact table comparison.

    ``pa.Table.equals`` is NaN-hostile — a float column containing NaN
    never equals ANY column, including itself (the substrate's
    ``volume_prism_bound_m3`` uses NaN where no prism bound exists).
    IPC-stream bytes compare buffers bitwise instead: NaN == NaN when
    the bit patterns match, which is exactly the parity claim we want
    against the oracle. Chunking is normalized first."""
    t = t.combine_chunks()
    sink = pa.BufferOutputStream()
    with pa.ipc.new_stream(sink, t.schema) as writer:
        writer.write_table(t)
    return sink.getvalue().to_pybytes()


def _bundle_fixture(fixture: str, out_dir: Path) -> Path:
    src = FIXTURES / fixture
    assert src.is_file(), f"missing committed fixture {src}"
    ifcfast.bundle(str(src), out_dir=str(out_dir))
    return out_dir


@pytest.fixture()
def two_box_bundles(tmp_path: Path) -> tuple[Path, Path]:
    """The same solid fixture bundled into two differently-named dirs:
    guaranteed guid collisions AND overlapping cross-model geometry."""
    a = _bundle_fixture("geom_box.ifc", tmp_path / "box_a.bundle")
    b = _bundle_fixture("geom_box.ifc", tmp_path / "box_b.bundle")
    return a, b


@pytest.fixture()
def disjoint_bundles(tmp_path: Path) -> tuple[Path, Path]:
    """Two different fixtures — disjoint guids, the clean-merge path.
    Both metre-scale (hotswap_roundtrip.ifc is mm-authored and feeds
    the GH #169 mixed-unit rescale test instead)."""
    a = _bundle_fixture("geom_box.ifc", tmp_path / "box.bundle")
    b = _bundle_fixture("hotswap_body.ifc", tmp_path / "body.bundle")
    return a, b


# -- 0. bundle() stamps source_model -------------------------------------


def test_bundle_stamps_source_model_with_ifc_stem(tmp_path):
    out = _bundle_fixture("geom_box.ifc", tmp_path / "anything.bundle")
    t = pq.read_table(out / "instances.parquet")
    field = t.schema.field("source_model")
    assert not field.nullable
    values = set(t.column("source_model").to_pylist())
    assert values == {"geom_box"}, "bundle() stamps the source IFC's file stem"


# -- 1. table parity product vs oracle ------------------------------------


def _assert_bundle_dirs_equal(product: Path, oracle: Path) -> None:
    for table in _TABLES:
        tp = pq.read_table(product / f"{table}.parquet")
        to = pq.read_table(oracle / f"{table}.parquet")
        # Schema equality first (field-by-field including nullability
        # and metadata) for a readable failure before the bitwise check.
        assert tp.schema == to.schema, (
            f"{table}: product schema diverged from oracle\n"
            f"product: {tp.schema}\noracle:  {to.schema}"
        )
        assert tp.schema.metadata == to.schema.metadata, f"{table}: metadata diverged"
        assert _table_bytes(tp) == _table_bytes(to), (
            f"{table}: merged rows diverged from oracle (bitwise)"
        )


def test_product_merge_equals_oracle_merge_disjoint(disjoint_bundles, tmp_path):
    a, b = disjoint_bundles
    product_dir = tmp_path / "fed_product"
    oracle_dir = tmp_path / "fed_oracle"

    sidecar_p = ifcfast.federate([a, b], product_dir)
    sidecar_o = oracle_federate([a, b], oracle_dir)

    _assert_bundle_dirs_equal(product_dir, oracle_dir)
    assert sidecar_p["rep_id_offsets"] == sidecar_o["rep_id_offsets"]
    assert sidecar_p["guid_source"] == sidecar_o["guid_source"]
    assert sidecar_p["guid_collisions"] == [] == sidecar_o["guid_collisions"]
    assert sidecar_p["unit_scale"] == sidecar_o["unit_scale"]

    # federate() also ships the substrate's view.sql like bundle() does.
    assert (product_dir / "view.sql").is_file()
    assert (
        product_dir / "view.sql"
    ).read_bytes() == (a / "view.sql").read_bytes()


def test_product_merge_equals_oracle_merge_with_collisions(two_box_bundles, tmp_path):
    a, b = two_box_bundles
    product_dir = tmp_path / "fed_product"
    oracle_dir = tmp_path / "fed_oracle"

    with pytest.warns(UserWarning, match="guid"):
        sidecar_p = ifcfast.federate([a, b], product_dir, on_collision="warn")
    sidecar_o = oracle_federate([a, b], oracle_dir)

    # warn == oracle's keep-everything behaviour, table-for-table.
    _assert_bundle_dirs_equal(product_dir, oracle_dir)
    assert sidecar_p["guid_collisions"] == sidecar_o["guid_collisions"]
    assert sidecar_p["guid_collisions"], "same fixture twice must collide"


def test_federated_source_model_is_bundle_dir_name(disjoint_bundles, tmp_path):
    a, b = disjoint_bundles
    fed = tmp_path / "fed"
    ifcfast.federate([a, b], fed)
    t = pq.read_table(fed / "instances.parquet")
    assert set(t.column("source_model").to_pylist()) == {a.name, b.name}


# -- 2. clash([a, b]) == clash(oracle-merged dir) --------------------------


def _sorted_rows(df):
    cols = [
        "ifc_id_a",
        "ifc_id_b",
        "guid_a",
        "guid_b",
        "class_a",
        "class_b",
        "source_model_a",
        "source_model_b",
        "kind",
        "category",
        "min_distance_m",
    ]
    return df[cols].sort_values(cols).reset_index(drop=True)


def test_clash_list_sugar_matches_clash_on_oracle_merge(
    two_box_bundles, tmp_path, monkeypatch
):
    a, b = two_box_bundles
    # Point the federation cache into the test sandbox.
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))

    oracle_dir = tmp_path / "fed_oracle"
    oracle_federate([a, b], oracle_dir)
    df_oracle = ifcfast.clash(oracle_dir, write_parquet=False)

    with pytest.warns(UserWarning, match="guid"):
        df_sugar = ifcfast.clash([a, b], write_parquet=False)

    assert len(df_sugar) == len(df_oracle) > 0, "identical boxes must hard-clash"
    assert _sorted_rows(df_sugar).equals(_sorted_rows(df_oracle))

    # The federated substrate is cached and attributed on df.attrs.
    fed_dir = Path(df_sugar.attrs["federated_dir"])
    assert fed_dir == federation_cache_dir([a, b], "warn")
    assert (fed_dir / "federation.json").is_file()
    assert df_sugar.attrs["federation"]["guid_collisions"]

    # Cross-model attribution: the two copies live in different models.
    cross = df_sugar[df_sugar.source_model_a != df_sugar.source_model_b]
    assert len(cross) > 0, "the box-vs-box clash is cross-model"

    # Cache hit: a second run must reuse the merged dir (federate would
    # re-warn; the cached path re-checks the sidecar silently).
    df_again = ifcfast.clash([a, b], write_parquet=False)
    assert _sorted_rows(df_again).equals(_sorted_rows(df_sugar))


def test_clash_single_element_list_equals_scalar(two_box_bundles):
    a, _ = two_box_bundles
    df_list = ifcfast.clash([a], write_parquet=False)
    df_scalar = ifcfast.clash(a, write_parquet=False)
    assert df_list.equals(df_scalar)


def test_clash_reference_only_drops_both_sides_reference(
    two_box_bundles, tmp_path, monkeypatch
):
    a, b = two_box_bundles
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))

    with pytest.warns(UserWarning, match="guid"):
        df_all = ifcfast.clash([a, b], write_parquet=False)
    assert len(df_all) > 0

    # One reference model: cross pairs (ref vs active) survive.
    df_one_ref = ifcfast.clash([a, b], write_parquet=False, reference_only=(a.name,))
    kept = df_one_ref[
        (df_one_ref.source_model_a == a.name) & (df_one_ref.source_model_b == a.name)
    ]
    assert kept.empty, "no pair may have BOTH sides in the reference set"
    assert len(df_one_ref) > 0, "ref-vs-active pairs must survive"

    # Every model reference: nothing can clash.
    df_all_ref = ifcfast.clash(
        [a, b], write_parquet=False, reference_only=(a.name, b.name)
    )
    assert df_all_ref.empty


# -- 3. collision policies -------------------------------------------------


def test_on_collision_fail_raises(two_box_bundles, tmp_path):
    a, b = two_box_bundles
    with pytest.raises(ValueError, match="guid"):
        ifcfast.federate([a, b], tmp_path / "fed", on_collision="fail")


def test_on_collision_fail_raises_on_cached_federation(
    two_box_bundles, tmp_path, monkeypatch
):
    a, b = two_box_bundles
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    with pytest.warns(UserWarning, match="guid"):
        ifcfast.clash([a, b], write_parquet=False)  # populate warn/fail-keyed cache
    with pytest.raises(ValueError, match="guid"):
        ifcfast.clash([a, b], write_parquet=False, on_collision="fail")


def test_on_collision_dedup_keeps_first_source_rows(two_box_bundles, tmp_path):
    a, b = two_box_bundles
    fed = tmp_path / "fed"
    sidecar = ifcfast.federate([a, b], fed, on_collision="dedup")

    n_a = pq.read_table(a / "instances.parquet").num_rows
    t = pq.read_table(fed / "instances.parquet")
    assert t.num_rows == n_a, "dedup drops the later source's duplicate rows"
    assert set(t.column("source_model").to_pylist()) == {a.name}
    assert sidecar["guid_collisions"], "collisions are still reported"
    # Representations are intentionally left whole (orphans are inert).
    n_reps = sum(
        pq.read_table(d / "representations.parquet").num_rows for d in (a, b)
    )
    assert pq.read_table(fed / "representations.parquet").num_rows == n_reps


# -- 4. loud failure modes ---------------------------------------------------


def test_federate_rejects_single_bundle(disjoint_bundles, tmp_path):
    a, _ = disjoint_bundles
    with pytest.raises(ValueError, match="at least two"):
        ifcfast.federate([a], tmp_path / "fed")


def test_federate_rejects_duplicate_dir_names(disjoint_bundles, tmp_path):
    a, _ = disjoint_bundles
    twin = tmp_path / "elsewhere" / a.name
    twin.parent.mkdir()
    shutil.copytree(a, twin)
    with pytest.raises(ValueError, match="unique"):
        ifcfast.federate([a, twin], tmp_path / "fed")


def test_federate_rejects_unknown_reference_only(disjoint_bundles, tmp_path):
    a, b = disjoint_bundles
    with pytest.raises(ValueError, match="reference_only"):
        ifcfast.federate([a, b], tmp_path / "fed", reference_only=("nope",))


def test_federate_rescales_mixed_unit_scale(disjoint_bundles, tmp_path):
    """GH #169: mixed units are converted, not refused. The oracle spec
    predates the rule and still raises, so this case is deliberately
    OUTSIDE the parity gate — the full contract lives in
    ``tests/test_federate_mixed_units.py``."""
    a, _ = disjoint_bundles  # metre-scale
    mm = _bundle_fixture("hotswap_roundtrip.ifc", tmp_path / "mm.bundle")
    fed = tmp_path / "fed"
    sidecar = ifcfast.federate([a, mm], fed)
    assert sidecar["unit_scale"] == "0.001", "target is the FINEST unit"
    assert sidecar["unit_factors"] == {a.name: 1000.0, "mm.bundle": 1.0}
    assert (
        pq.read_schema(fed / "instances.parquet").metadata[b"ifcfast.unit_scale"]
        == b"0.001"
    )


def test_federate_rejects_non_bundle_dir(disjoint_bundles, tmp_path):
    a, _ = disjoint_bundles
    empty = tmp_path / "not_a_bundle"
    empty.mkdir()
    with pytest.raises(FileNotFoundError):
        ifcfast.federate([a, empty], tmp_path / "fed")


def test_clash_scalar_rejects_on_collision(disjoint_bundles):
    """A BARE path carries no federation intent — `on_collision` there is
    a caller mistake and stays loud."""
    a, _ = disjoint_bundles
    with pytest.raises(ValueError, match="on_collision"):
        ifcfast.clash(a, write_parquet=False, on_collision="fail")


def test_clash_single_element_list_accepts_on_collision(disjoint_bundles):
    """GH #162: the list form carries federation intent, and the
    docstring promises `[a]` behaves like `a`.

    With one source there is no second bundle for a guid to collide
    with, so every policy is vacuously satisfied — the policy is
    APPLIED and never fires, which is not the same as being swallowed
    (`on_collision="fail"` on a two-bundle federation with no collisions
    doesn't raise either). Pre-GH #162 this raised, so a caller
    federating a variable-length list had to special-case len == 1.
    """
    a, _ = disjoint_bundles
    for policy in ("warn", "fail", "dedup"):
        df = ifcfast.clash([a], write_parquet=False, on_collision=policy)
        assert df is not None
    # The bare-path form of the same bundle agrees on the result.
    assert len(ifcfast.clash([a], write_parquet=False)) == len(
        ifcfast.clash(a, write_parquet=False)
    )


def test_clash_rejects_unknown_or_malformed_reference_only(
    two_box_bundles, tmp_path, monkeypatch
):
    a, b = two_box_bundles
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    # Typo'd name: silently matching nothing would disable the filter
    # (a quiet clash false-negative) — must fail loudly instead.
    with pytest.raises(ValueError, match="reference_only"):
        ifcfast.clash([a, b], write_parquet=False, reference_only=("typo",))
    with pytest.raises(ValueError, match="reference_only"):
        ifcfast.clash(a, write_parquet=False, reference_only=("typo",))
    # Bare string: tuple("ark") == ('a','r','k').
    with pytest.raises(TypeError, match="bare string"):
        ifcfast.clash([a, b], write_parquet=False, reference_only=a.name)
    with pytest.raises(TypeError, match="bare string"):
        ifcfast.federate([a, b], tmp_path / "fed", reference_only=a.name)


def test_clashes_parquet_carries_source_model_columns(two_box_bundles, tmp_path):
    a, b = two_box_bundles
    fed = tmp_path / "fed"
    with pytest.warns(UserWarning, match="guid"):
        ifcfast.federate([a, b], fed)
    df = ifcfast.clash(fed, write_parquet=True)
    t = pq.read_table(fed / "clashes.parquet")
    assert {"source_model_a", "source_model_b"} <= set(t.schema.names)
    assert t.num_rows == len(df)
    assert set(t.column("source_model_a").to_pylist()) <= {a.name, b.name}
