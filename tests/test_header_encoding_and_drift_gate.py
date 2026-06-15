r"""Regression tests for the two leftovers split out of GH #84 into GH #87.

1. Header text decode (`python/ifcfast/header.py`). The Tier-0 header
   parser decoded the 64 KB prefix with `errors="replace"`, so raw cp1252
   æøå in `FILE_NAME` author/organization silently became U+FFFD and
   `\X2\…\X0\` escapes were passed through verbatim. The fix decodes
   strict-UTF-8 → cp1252/latin-1 (lossless, flagged `encoding_lossy`) and
   resolves the STEP string escapes. "Fail loudly" = never silently drop
   bytes to U+FFFD.

2. No-drift cache gate (`python/ifcfast/cache.py`). On a `_core` built
   without the `mesh` feature, `analyse_drift` is absent → drift/segments
   never cache, so `all_cached` stayed False forever and the four good
   data layers re-extracted every process. The fix records
   `drift_unavailable` in the manifest and excludes drift from the gate.

These run against the prebuilt local `_core`, using a private
IFCFAST_CACHE dir per test so they never touch the user cache.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from ifcfast import cache as _cache
from ifcfast.header import (
    _decode_header_prefix,
    _resolve_step_escapes,
    header as _header,
)

FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"


# ----------------------------------------------------------------------
# Item 1 — header text decode
# ----------------------------------------------------------------------


def test_decode_header_prefix_utf8_is_not_lossy():
    text, lossy = _decode_header_prefix("Dør-æå".encode("utf-8"))
    assert text == "Dør-æå"
    assert lossy is False


def test_decode_header_prefix_cp1252_round_trips_not_replaced():
    """Raw cp1252 æøå (invalid UTF-8) must survive losslessly, never the
    silent U+FFFD that `errors="replace"` produced (GH #87)."""
    raw = "Åsmund Ærø".encode("cp1252")
    # Precondition: these bytes are NOT valid UTF-8 (so the old strict
    # decode would have raised and the lenient path produced U+FFFD).
    with pytest.raises(UnicodeDecodeError):
        raw.decode("utf-8")
    text, lossy = _decode_header_prefix(raw)
    assert "�" not in text, "lossy U+FFFD substitution regressed"
    assert text == "Åsmund Ærø"
    assert lossy is True


def test_header_non_ascii_author_decodes_cp1252(tmp_path):
    """A FILE_NAME author written as raw cp1252 must reach `header()`
    intact with `encoding_lossy=True` — not as U+FFFD."""
    step = (
        "ISO-10303-21;\n"
        "HEADER;\n"
        "FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');\n"
        "FILE_NAME('n.ifc','2026-06-13T00:00:00',('{author}'),"
        "('{org}'),'ifcfast','exporter','');\n"
        "FILE_SCHEMA(('IFC4'));\n"
        "ENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n"
    )
    author = "Åsmund Ærø"
    org = "Strømstad Bygg"
    blob = step.format(author=author, org=org).encode("cp1252")
    f = tmp_path / "cp1252.ifc"
    f.write_bytes(blob)

    hdr = _header(f)
    assert hdr.author == [author]
    assert hdr.organization == [org]
    assert hdr.encoding_lossy is True
    assert all("�" not in s for s in hdr.author + hdr.organization)


def test_header_step_escape_x2_resolved(tmp_path):
    r"""`\X2\…\X0\` UTF-16BE escapes in header strings must decode.

    `\X2\00C5\X0\sgaten` → `Åsgaten` (Revit/Tekla Norwegian export form).
    """
    # Åsmund Ærø, each non-ASCII letter as a \X2\HHHH\X0\ UTF-16BE escape.
    author = "\\X2\\00C5\\X0\\smund \\X2\\00C6\\X0\\r\\X2\\00F8\\X0\\"
    step = (
        "ISO-10303-21;\n"
        "HEADER;\n"
        "FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');\n"
        f"FILE_NAME('n.ifc','2026-06-13T00:00:00',('{author}'),"
        "('Org'),'ifcfast','exporter','');\n"
        "FILE_SCHEMA(('IFC4'));\n"
        "ENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n"
    )
    f = tmp_path / "escaped.ifc"
    f.write_bytes(step.encode("ascii"))

    hdr = _header(f)
    assert hdr.author == ["Åsmund Ærø"]
    # Pure-ASCII bytes → strict UTF-8 path → not flagged lossy.
    assert hdr.encoding_lossy is False


def test_resolve_step_escapes_forms():
    # \X2\…\X0\ UTF-16BE
    assert _resolve_step_escapes(r"\X2\00C5\X0\sgaten") == "Åsgaten"
    # \X\HH ISO-8859-1 single byte
    assert _resolve_step_escapes(r"\X\C5sgaten") == "Åsgaten"
    # \S\C Latin-1 short form (C | 0x80)
    assert _resolve_step_escapes("\\S\\E") == "Å"  # 'E'=0x45 -> 0xC5
    # No escapes → untouched
    assert _resolve_step_escapes("plain ascii") == "plain ascii"


# ----------------------------------------------------------------------
# Item 2 — no-drift cache gate
# ----------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _isolated_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "ifcfast-cache"))
    yield


def _strip_analyse_drift(monkeypatch):
    """Simulate a `_core` built without the `mesh` feature.

    `extract_data_layers` does `from . import _core` then calls
    `_core.analyse_drift(...)`; on a no-mesh build that attribute is
    absent and the call raises AttributeError. We delete it from the live
    module so the access raises exactly as a missing PyO3 binding would.
    """
    from ifcfast import _core

    monkeypatch.delattr(_core, "analyse_drift", raising=True)


def test_no_drift_build_satisfies_cache_gate(monkeypatch):
    """GH #87 item 2: on a no-mesh build the four good data layers must
    pass the hot-reload gate (cold_parse False on the second open) instead
    of re-extracting forever because drift can't be produced."""
    _strip_analyse_drift(monkeypatch)

    # Cold extract → writes psets/quantities/materials/classifications +
    # a manifest that records drift_unavailable.
    out1 = _cache.extract_data_layers(FIXTURE, include_drift=True)
    assert out1.timing_ms["cold_parse"] is True
    assert out1.drift is None
    assert out1.drift_unavailable is True

    manifest = _cache._read_manifest(_cache.cache_dir_for(_header(FIXTURE)))
    assert manifest.get("drift_unavailable") is True

    # Second open in a fresh "process" — must be a hot cache hit, NOT a
    # re-extract, even though drift/segments parquet were never written.
    out2 = _cache.extract_data_layers(FIXTURE, include_drift=True)
    assert out2.timing_ms["cold_parse"] is False, (
        "no-drift build never satisfied the data-layer cache gate (GH #87)"
    )
    assert out2.psets is not None
    assert out2.quantities is not None
    assert out2.materials is not None
    assert out2.classifications is not None
    assert out2.drift is None
    assert out2.drift_unavailable is True


def test_mesh_build_still_caches_drift_normally():
    """A normal (mesh-enabled) build must keep producing + caching drift;
    the gate fix must not suppress it."""
    out1 = _cache.extract_data_layers(FIXTURE, include_drift=True)
    assert out1.drift is not None
    assert out1.drift_unavailable is False

    manifest = _cache._read_manifest(_cache.cache_dir_for(_header(FIXTURE)))
    assert manifest.get("has_drift") is True
    assert not manifest.get("drift_unavailable")

    out2 = _cache.extract_data_layers(FIXTURE, include_drift=True)
    assert out2.timing_ms["cold_parse"] is False
    assert out2.drift is not None


def test_include_drift_false_does_not_block_later_drift_reader():
    """A writer that opted out of drift (CLI `ifcfast extract`) must NOT
    persist drift_unavailable — a later include_drift=True reader has to be
    free to cold-parse drift, not be served a drift-less hit."""
    out1 = _cache.extract_data_layers(FIXTURE, include_drift=False)
    assert out1.timing_ms["cold_parse"] is True

    manifest = _cache._read_manifest(_cache.cache_dir_for(_header(FIXTURE)))
    assert not manifest.get("drift_unavailable")

    # Reader now wants drift; since it was never cached and the build can
    # produce it, this must cold-parse (and now cache) drift.
    out2 = _cache.extract_data_layers(FIXTURE, include_drift=True)
    assert out2.timing_ms["cold_parse"] is True
    assert out2.drift is not None
