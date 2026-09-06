"""AGENTS.md ships in the wheel and never silently degrades (GH #157).

`AGENTS.md` is the public contract this project exists for. Two things
must hold:

1. The wheel actually carries it (it used to be sdist-only, so every
   `pip install ifcfast` had a package that could not serve its own
   guide).
2. Asking for it either returns the real thing or FAILS. The old MCP
   resource fell back to the ~60-line `system_prompt()` with no signal,
   which is the worst outcome: a plausible-looking short answer that an
   agent cannot distinguish from the full contract.
"""

from __future__ import annotations

import hashlib
from pathlib import Path

import pytest

import ifcfast


REPO_ROOT = Path(__file__).resolve().parent.parent
ROOT_GUIDE = REPO_ROOT / "AGENTS.md"
PACKAGED_GUIDE = Path(ifcfast.__file__).parent / "data" / "AGENTS.md"


def test_packaged_guide_exists_and_is_package_data():
    """The guide sits INSIDE the package dir, next to minimal.ifc.

    That location is the whole point: it is the only tree maturin
    auto-packages into the wheel, so `agents_guide()` can find it via
    `Path(__file__).parent` in an installed environment.
    """
    assert PACKAGED_GUIDE.is_file(), PACKAGED_GUIDE
    assert PACKAGED_GUIDE.parent.name == "data"
    assert (PACKAGED_GUIDE.parent / "minimal.ifc").is_file()


@pytest.mark.skipif(
    not ROOT_GUIDE.is_file(),
    reason="running against an installed wheel, not the repo",
)
def test_packaged_guide_is_byte_identical_to_repo_root():
    """The copy must never drift from the canonical root file.

    If this fails you edited one and not the other. Fix with:

        cp AGENTS.md python/ifcfast/data/AGENTS.md
    """
    root = ROOT_GUIDE.read_bytes()
    packaged = PACKAGED_GUIDE.read_bytes()
    assert hashlib.sha256(packaged).hexdigest() == hashlib.sha256(root).hexdigest(), (
        "python/ifcfast/data/AGENTS.md is out of sync with the repo-root "
        "AGENTS.md. Run: cp AGENTS.md python/ifcfast/data/AGENTS.md"
    )


def test_agents_guide_returns_the_full_document():
    text = ifcfast.agents_guide()
    assert text == PACKAGED_GUIDE.read_text(encoding="utf-8")
    # Sanity: the full guide, not the summary. `system_prompt()` is
    # ~100 lines; AGENTS.md is an order of magnitude bigger and carries
    # section headings the summary does not.
    assert len(text) > 10 * len(ifcfast.system_prompt())
    assert "## Decision tree for common tasks" in text
    assert "## CLI quick reference" in text


def test_agents_guide_raises_when_missing(monkeypatch, tmp_path):
    """No silent fallback to system_prompt() — the GH #157 defect."""
    fake_pkg = tmp_path / "ifcfast"
    (fake_pkg / "data").mkdir(parents=True)
    monkeypatch.setattr(ifcfast, "__file__", str(fake_pkg / "__init__.py"))
    with pytest.raises(FileNotFoundError, match="packaged agent guide missing"):
        ifcfast.agents_guide()


def test_agents_guide_is_exported():
    assert "agents_guide" in ifcfast.__all__
    assert callable(ifcfast.agents_guide)


# ----------------------------------------------------------------------
# system_prompt() must agree with AGENTS.md on the load-bearing rules
# ----------------------------------------------------------------------


def test_system_prompt_teaches_the_current_routing_rule():
    """GH #157: the prompt hard-coded the pre-#60 escalation rule.

    `|volume_m3| > aabb_volume_m3` was replaced by the self-labelled
    `volume_reliable` flag. An agent pasting the old prompt built its
    QTO pipeline on a tripwire that no longer exists.
    """
    sp = ifcfast.system_prompt()
    assert "volume_reliable" in sp
    assert "aabb_volume_m3" not in sp


def test_system_prompt_covers_the_shipped_surface():
    """Every primitive an agent would otherwise never discover."""
    sp = ifcfast.system_prompt()
    for token in (
        "m.mutate",
        "m.type_summary",
        "m.segments",
        "m.voids",
        'frame="local"',
        "m.product(guid)",
        "ifcfast.agents_guide()",
        "IfcfastError",
    ):
        assert token in sp, token


def test_system_prompt_cache_subcommand_takes_a_file():
    """`ifcfast cache` is keyed on an IFC FILE, not a cache directory."""
    sp = ifcfast.system_prompt()
    assert "ifcfast cache DIR" not in sp
    assert "ifcfast cache FILE" in sp


def test_documented_model_surface_actually_exists():
    """Names promised by system_prompt() resolve on a real Model."""
    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    for attr in (
        "product", "voids", "segments", "spaces", "spaces_df",
        "type_objects", "type_objects_df", "type_summary", "type_bank",
        "mutate", "hotswap", "subset", "length_unit", "unit_scale",
    ):
        assert hasattr(m, attr), attr
