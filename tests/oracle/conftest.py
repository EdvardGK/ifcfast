"""Shared fixtures/markers for the oracle suite (GH #59 M1).

Centralises the two things every oracle test needs:

1. **The skip-if-no-ifcopenshell gate** — ``pytest.importorskip`` is
   called *once* here, which makes *this conftest module* skip cleanly
   when the dev-only ``ifcopenshell`` extra is absent. NOTE: the
   importorskip guards only the conftest itself; sibling
   ``test_*_oracle.py`` files therefore keep their ``ifcopenshell``
   imports *inside* their test helpers (mirroring
   ``tests/test_quantities.py``) so the main ``pytest -q`` run over the
   whole tree skips rather than ERRORs at collection.

2. **A corpus opener** — ``corpus`` yields a helper that opens any
   committed tiny fixture in **both** libraries, returning an
   ``OpenedFixture(name, path, fast, ios)`` pair so an adapter writes
   ``fast = corpus("quantities.ifc").fast`` / ``.ios`` without
   re-deriving paths or duplicating the dual-open boilerplate.

Hard rule for M1: only the committed tiny fixtures under
``tests/fixtures/`` are used here — never Duplex/Sannergata (RAM).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

import pytest

import ifcfast

# --- the single import gate --------------------------------------------------
# importorskip here skips *this conftest module* (and, by extension, the
# corpus fixture / dual-open below) when the dev-only oracle extra is absent.
# It does NOT guard sibling test modules at collection — those import
# ifcopenshell inside their test helpers for that reason.
ifcopenshell = pytest.importorskip("ifcopenshell")


FIXTURES_DIR = Path(__file__).resolve().parents[1] / "fixtures"


@dataclass(frozen=True)
class OpenedFixture:
    """A tiny fixture opened in both libraries.

    ``fast`` is the :class:`ifcfast.Model`; ``ios`` is the
    ``ifcopenshell.file``. ``name`` / ``path`` identify the source so a
    :class:`tests.oracle.report.DisagreementRecord` can be tagged with
    the originating fixture.
    """

    name: str
    path: Path
    fast: Any
    ios: Any


@pytest.fixture(scope="session")
def ifcopenshell_module():
    """The imported ``ifcopenshell`` module (already import-gated)."""
    return ifcopenshell


@pytest.fixture(scope="session")
def corpus() -> Callable[[str], OpenedFixture]:
    """Return an opener: ``corpus("quantities.ifc") -> OpenedFixture``.

    Opens the named fixture in ifcfast (no cache) and ifcopenshell.
    Results are memoised within the session so repeated calls across
    surface adapters don't re-parse the same tiny file.
    """
    cache: dict[str, OpenedFixture] = {}

    def _open(name: str) -> OpenedFixture:
        if name in cache:
            return cache[name]
        path = FIXTURES_DIR / name
        if not path.exists():
            pytest.skip(f"fixture {name!r} not committed at {path}")
        fast = ifcfast.open(path, use_cache=False, write_cache=False)
        ios = ifcopenshell.open(str(path))
        opened = OpenedFixture(name=name, path=path, fast=fast, ios=ios)
        cache[name] = opened
        return opened

    return _open
