"""Drift gate for ``scripts/gen_schema_supertypes.py`` (GH #201).

The generator writes two committed files from ONE schema walk:
``python/ifcfast/data/schema_supertypes.py`` (classifier tables) and
``crates/core/src/schema_products.rs`` (the tier-1 product whitelist, the
Tag position table and the entity spelling table). This test regenerates
both into a temp dir and asserts byte identity, so a hand edit of either
fails CI.

Skips unless ifcopenshell at the pinned version is importable — it is a
generation-time dependency only.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
GEN = REPO / "scripts" / "gen_schema_supertypes.py"
PINNED = "0.8.5"

ifcopenshell = pytest.importorskip("ifcopenshell")
if ifcopenshell.version != PINNED:
    pytest.skip(
        f"ifcopenshell {ifcopenshell.version} != pinned {PINNED}",
        allow_module_level=True,
    )


def _load_generator():
    spec = importlib.util.spec_from_file_location("gen_schema_supertypes", GEN)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


def test_generated_files_match_generator(tmp_path):
    gen = _load_generator()
    py_out = tmp_path / "schema_supertypes.py"
    rust_out = tmp_path / "schema_products.rs"
    gen.main(["--py-out", str(py_out), "--rust-out", str(rust_out)])
    for fresh, committed in ((py_out, gen.PY_OUT), (rust_out, gen.RUST_OUT)):
        assert committed.exists(), f"{committed} missing — run {gen.GEN_COMMAND}"
        assert fresh.read_bytes() == committed.read_bytes(), (
            f"{committed.relative_to(REPO)} drifted from the generator; "
            f"re-run `{gen.GEN_COMMAND}` and commit the result"
        )
