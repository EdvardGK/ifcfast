"""GH #185: the sidecar generator's per-product `materials` rollup covers
every role the extractor emits (direct / list / layer / constituent /
profile). The wasm port (`crates/wasm/src/analysis.rs::roll_up_materials`)
is pinned to this function by `crates/wasm/test/parity.mjs`."""
import importlib.util
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "tests" / "fixtures" / "materials_roles_185.ifc"


def _generator():
    spec = importlib.util.spec_from_file_location(
        "generate_sample_sidecars", ROOT / "scripts" / "generate_sample_sidecars.py"
    )
    mod = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(mod)
    except ImportError as exc:  # pragma: no cover - optional deps
        pytest.skip(f"generator not importable: {exc}")
    return mod


def test_rollup_covers_every_role():
    import ifcfast

    gen = _generator()
    m = ifcfast.open(str(FIXTURE))
    rows = gen._df_to_records(m.materials)
    assert {r["role"] for r in rows} >= {"direct", "layer", "profile", "constituent", "list"}
    got = gen._materials_by_guid(rows)
    assert got == {
        "7WALL1UIDgUIDgUIDgUID0": ["Concrete"],
        # material_name is the IfcMaterial's Name for every role (GH #219)
        "9WALL2UIDgUIDgUIDgUID0": ["Gypsum", "Insulation"],
        "BBEAM1UIDgUIDgUIDgUID0": ["Steel"],
        # three constituents over two materials: deduped, first-seen order
        "DWALL4UIDgUIDgUIDgUID0": ["Concrete", "Steel"],
        "FCOLM1UIDgUIDgUIDgUID0": ["Concrete", "Gypsum"],
    }


def test_rollup_ignores_unknown_and_nameless_rows():
    gen = _generator()
    rows = [
        {"guid": "a", "role": "unknown", "material_name": None},
        {"guid": "b", "role": "direct", "material_name": None},
        {"guid": "c", "role": "direct", "material_name": "X"},
        {"guid": "c", "role": "layer", "material_name": "X"},
    ]
    assert gen._materials_by_guid(rows) == {"b": [], "c": ["X"]}


def test_qto_aggregates_split_clip_unapplied_volume():
    gen = _generator()
    products = [
        {"entity": "IfcWall", "guid": "g1", "mesh_stats": {"volume_abs_m3": 2.0}},
        {"entity": "IfcWall", "guid": "g2", "mesh_stats": {"volume_abs_m3": 5.0}},
        {"entity": "IfcSlab", "guid": "g3", "mesh_stats": None},
    ]
    rows = {r["entity"]: r for r in gen._qto_aggregates(products, {"g2"})}
    w = rows["IfcWall"]
    assert (w["volume_m3"], w["volume_reliable_m3"], w["volume_unreliable_m3"]) == (7.0, 2.0, 5.0)
    assert w["products_clip_unapplied"] == 1
    s = rows["IfcSlab"]
    assert s["volume_m3"] is None and s["volume_unreliable_m3"] is None


def test_graph_layer_set_from_set_name_213():
    """GH #213: `layer_set` / `material_layer_sets` come from the
    materials table's `set_name`. Pinned to the same values as
    `crates/wasm/src/analysis.rs::graph_materials_carry_every_role`."""
    import ifcfast

    gen = _generator()
    m = ifcfast.open(str(FIXTURE), use_cache=False, write_cache=False)
    g = gen._build_graph(m, [], {}, {}, {}, {})
    by_name = {p["name"]: p["layer_set"] for p in g["products"]}
    assert by_name["Wall-Layered"] == "WallBuildup"
    assert all(v is None for k, v in by_name.items() if k != "Wall-Layered")
    assert g["material_layer_sets"] == {
        "WallBuildup": {
            "layers": [
                {"material": "Gypsum", "thickness_mm": 13.0},
                {"material": "Insulation", "thickness_mm": 100.0},
            ],
            "total_thickness_mm": 113.0,
        }
    }
