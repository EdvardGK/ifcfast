"""Tests for federated floor synthesis.

The module is project-agnostic; these tests cover the generic clustering,
the identity rule, the configurable `make_prefix_rule` factory, and the
YAML loader. No project-specific tables.
"""

from __future__ import annotations

import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from ifcfast.federated_floors import (
    ClusterInfo,
    StoreyInfo,
    default_rule,
    drop_leading_zero,
    get_rule,
    known_rules,
    load_rule_from_yaml,
    make_prefix_rule,
    register_rule,
    synthesise_federated_floors,
)


# ----------------------------------------------------------------------
# Clustering
# ----------------------------------------------------------------------


def test_basic_clustering_same_elevation_three_models():
    """3 models, identical elevation -> 1 cluster, 1 label."""
    storeys = [
        StoreyInfo("ARK", "Plan 01", 0.0),
        StoreyInfo("RIB", "Plan 01", 0.0),
        StoreyInfo("RIV", "Plan 01", 0.0),
    ]
    out = synthesise_federated_floors(storeys)
    assert len(set(out.values())) == 1, f"expected single cluster, got {out}"
    assert out[("ARK", "Plan 01")] == "Plan 01"


def test_tolerance_splits_close_but_distinct_elevations():
    """Elevations 0 / 50 / 200 with tolerance 100 -> 2 clusters."""
    storeys = [
        StoreyInfo("M1", "A", 0.0),
        StoreyInfo("M2", "B", 50.0),
        StoreyInfo("M3", "C", 200.0),
    ]
    out = synthesise_federated_floors(storeys, tolerance_mm=100.0)
    labels = {out[("M1", "A")], out[("M2", "B")], out[("M3", "C")]}
    assert len(labels) == 2, f"expected 2 distinct labels, got {labels}"
    assert out[("M1", "A")] == out[("M2", "B")]
    assert out[("M3", "C")] != out[("M1", "A")]


def test_tolerance_zero_separates_everything():
    """Tolerance 0 with non-equal elevations -> N clusters."""
    storeys = [
        StoreyInfo("M1", "A", 0.0),
        StoreyInfo("M2", "B", 1.0),
        StoreyInfo("M3", "C", 2.0),
    ]
    out = synthesise_federated_floors(storeys, tolerance_mm=0.0)
    assert len(set(out.values())) == 3


def test_empty_input_returns_empty_mapping():
    assert synthesise_federated_floors([]) == {}


# ----------------------------------------------------------------------
# default_rule (identity)
# ----------------------------------------------------------------------


def test_default_rule_picks_most_common_name():
    """Identity rule returns the most common storey name in the cluster."""
    storeys = [
        StoreyInfo("M1", "Plan 1", 0.0),
        StoreyInfo("M2", "Plan 1", 30.0),
        StoreyInfo("M3", "Plan 01", 60.0),  # outlier name; minority
    ]
    out = synthesise_federated_floors(storeys, rule=default_rule)
    # All three are in one cluster (spread = 60 mm < 100 mm tolerance).
    assert set(out.values()) == {"Plan 1"}


# ----------------------------------------------------------------------
# drop_leading_zero (generic helper)
# ----------------------------------------------------------------------


def test_drop_leading_zero_numeric_tokens():
    assert drop_leading_zero("Plan 01") == "Plan 1"
    assert drop_leading_zero("Plan 010") == "Plan 10"
    assert drop_leading_zero("Plan U01") == "Plan U1"
    assert drop_leading_zero("Hav") == "Hav"  # no numeric token
    assert drop_leading_zero("Level 0") == "Level 0"  # zero stays


# ----------------------------------------------------------------------
# make_prefix_rule — the generic building-prefix-with-overrides shape
# ----------------------------------------------------------------------


def test_make_prefix_rule_adds_prefix_to_normalised_name():
    rule = make_prefix_rule(prefix="C - ")
    storeys = [
        StoreyInfo("ARK", "Plan 01", 0.0),
        StoreyInfo("RIB", "Plan 1",  50.0),
    ]
    out = synthesise_federated_floors(storeys, rule=rule, tolerance_mm=100.0)
    assert out[("ARK", "Plan 01")] == "C - Plan 1"
    assert out[("RIB", "Plan 1")] == "C - Plan 1"


def test_make_prefix_rule_override_bypasses_prefix():
    rule = make_prefix_rule(prefix="C - ", overrides={"Plan U1": "Hav"})
    storeys = [StoreyInfo("ARK", "Plan U1", -3500.0)]
    out = synthesise_federated_floors(storeys, rule=rule)
    assert out[("ARK", "Plan U1")] == "Hav"


def test_make_prefix_rule_idempotent_label_not_re_prefixed():
    rule = make_prefix_rule(prefix="C - ", idempotent_labels=["Hav"])
    storeys = [StoreyInfo("ARK", "Hav", -3500.0)]
    out = synthesise_federated_floors(storeys, rule=rule)
    assert out[("ARK", "Hav")] == "Hav"


def test_make_prefix_rule_pre_prefixed_storey_not_doubled():
    rule = make_prefix_rule(prefix="C - ")
    storeys = [StoreyInfo("ARK", "C - Plan 3", 6500.0)]
    out = synthesise_federated_floors(storeys, rule=rule)
    assert out[("ARK", "C - Plan 3")] == "C - Plan 3"


def test_make_prefix_rule_any_member_override_resolves_cluster():
    """Override on any cluster member wins, not just the most-common name.

    Real-world case from the LBK project: cluster at ~600 mm contains
    `Plan U1` (1 model) and `C - U1` (3 models). Most-common is `C - U1`
    but the override key is `Plan U1`. The whole cluster should resolve
    to the override value.
    """
    rule = make_prefix_rule(
        prefix="C - ",
        overrides={"Plan U1": "Hav"},
    )
    storeys = [
        StoreyInfo("RIE",  "Plan U1",   660.0),
        StoreyInfo("ARK",  "C - U1",    610.0),
        StoreyInfo("RIVr", "C - U1",    600.0),
        StoreyInfo("RIVv", "C - U1",    610.0),
    ]
    out = synthesise_federated_floors(storeys, rule=rule, tolerance_mm=100.0)
    assert out[("RIE", "Plan U1")] == "Hav"
    assert out[("ARK", "C - U1")] == "Hav"
    assert out[("RIVr", "C - U1")] == "Hav"


def test_make_prefix_rule_no_prefix_acts_like_identity():
    """An empty prefix with no overrides is effectively `default_rule`
    plus optional drop-leading-zero normalisation."""
    rule = make_prefix_rule(prefix="")
    storeys = [
        StoreyInfo("ARK", "Plan 01", 0.0),
        StoreyInfo("RIB", "Plan 1",  50.0),
    ]
    out = synthesise_federated_floors(storeys, rule=rule, tolerance_mm=100.0)
    assert out[("ARK", "Plan 01")] == "Plan 1"
    assert out[("RIB", "Plan 1")] == "Plan 1"


def test_make_prefix_rule_disable_drop_leading_zero():
    rule = make_prefix_rule(prefix="C - ", apply_drop_leading_zero=False)
    storeys = [StoreyInfo("ARK", "Plan 01", 0.0)]
    out = synthesise_federated_floors(storeys, rule=rule)
    assert out[("ARK", "Plan 01")] == "C - Plan 01"


# ----------------------------------------------------------------------
# YAML loader
# ----------------------------------------------------------------------


def test_load_rule_from_yaml_round_trip():
    yaml_content = """
    prefix: "C - "
    overrides:
      Plan U1: Hav
      C - U1: Hav
    idempotent_labels:
      - Hav
    apply_drop_leading_zero: true
    """
    with tempfile.NamedTemporaryFile(mode="w", suffix=".yaml", delete=False) as f:
        f.write(yaml_content)
        path = Path(f.name)
    try:
        rule = load_rule_from_yaml(path)
        storeys = [
            StoreyInfo("ARK", "Plan 01", 0.0),
            StoreyInfo("ARK", "Plan U1", -3500.0),
            StoreyInfo("ARK", "Hav", -3500.0),
        ]
        out = synthesise_federated_floors(storeys, rule=rule)
        assert out[("ARK", "Plan 01")] == "C - Plan 1"
        assert out[("ARK", "Plan U1")] == "Hav"
        assert out[("ARK", "Hav")] == "Hav"
    finally:
        path.unlink()


def test_load_rule_from_yaml_minimal_config():
    """A YAML with just a prefix works without overrides / idempotent_labels."""
    yaml_content = """
    prefix: "D - "
    """
    with tempfile.NamedTemporaryFile(mode="w", suffix=".yaml", delete=False) as f:
        f.write(yaml_content)
        path = Path(f.name)
    try:
        rule = load_rule_from_yaml(path)
        out = synthesise_federated_floors([StoreyInfo("M", "Plan 02", 0.0)], rule=rule)
        assert out[("M", "Plan 02")] == "D - Plan 2"
    finally:
        path.unlink()


# ----------------------------------------------------------------------
# Registry
# ----------------------------------------------------------------------


def test_register_rule_roundtrip():
    """Custom rule can be registered and retrieved by name."""

    def shouty(cluster: ClusterInfo) -> str:
        return cluster.most_common_name.upper()

    register_rule("SHOUTY", shouty)
    assert "SHOUTY" in known_rules()
    assert get_rule("SHOUTY") is shouty

    out = synthesise_federated_floors(
        [StoreyInfo("M1", "plan 1", 0.0)], rule=get_rule("SHOUTY")
    )
    assert out[("M1", "plan 1")] == "PLAN 1"


def test_default_rule_in_registry():
    """`default` is the only rule shipped by the module."""
    assert "default" in known_rules()
    assert get_rule("default") is default_rule


def test_public_export_present():
    """`synthesise_federated_floors` is importable from the module."""
    from ifcfast.federated_floors import (
        synthesise_federated_floors as exported,
    )

    assert exported is synthesise_federated_floors


# ----------------------------------------------------------------------
# Project-config integration test — uses the shipped LBK example
# ----------------------------------------------------------------------


def test_lbk_project_config_loads_and_resolves():
    """The shipped data/projects/lbk-building-c.yaml is parseable and
    produces the expected labels for the canonical LBK fixture.
    """
    repo_root = Path(__file__).resolve().parents[1]
    config_path = repo_root / "data" / "projects" / "lbk-building-c.yaml"
    if not config_path.exists():
        # Not all checkouts ship the example config (e.g. shallow CI).
        return
    rule = load_rule_from_yaml(config_path)
    storeys = [
        StoreyInfo("LBK_ARK_C", "Plan U1", -3500.0),
        StoreyInfo("LBK_RIE_C", "Plan U1", 660.0),
        StoreyInfo("LBK_ARK_C", "C - U1",  610.0),
        StoreyInfo("LBK_ARK_C", "Plan 01", 0.0),
        StoreyInfo("LBK_RIB_C", "Plan 1",  50.0),
        StoreyInfo("LBK_ARK_C", "Hav",     -3500.0),
        StoreyInfo("LBK_C_RIVr", "C - bunnarmering", 285.0),
    ]
    out = synthesise_federated_floors(storeys, rule=rule, tolerance_mm=100.0)
    assert out[("LBK_ARK_C", "Plan U1")] == "Hav"
    assert out[("LBK_RIE_C", "Plan U1")] == "Hav"
    assert out[("LBK_ARK_C", "C - U1")] == "Hav"
    assert out[("LBK_ARK_C", "Plan 01")] == "C - Plan 1"
    assert out[("LBK_RIB_C", "Plan 1")] == "C - Plan 1"
    assert out[("LBK_ARK_C", "Hav")] == "Hav"
    assert out[("LBK_C_RIVr", "C - bunnarmering")] == "Hav"


if __name__ == "__main__":
    import traceback

    failed = 0
    for name, fn in list(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"PASS  {name}")
            except Exception:
                failed += 1
                print(f"FAIL  {name}")
                traceback.print_exc()
    sys.exit(1 if failed else 0)
