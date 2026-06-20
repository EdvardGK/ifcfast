"""GH #73 / #72 — strict-mode loud-failure pass.

`open_ifc(strict=True)` (the default) raises on data anomalies that
would silently corrupt NUMBERS — chiefly a LENGTHUNIT that was declared
but couldn't be resolved (a broken IfcConversionBasedUnit chain, which
masks a real unit behind `unit_scale=None`). `strict=False` downgrades
the same anomalies to a capturable `UserWarning`.

The unit fix (#73) is the highest-value case: a truly unit-less file
(no LENGTHUNIT) is treated as an *explicit* metres assumption and stays
silent, while a declared-but-unresolved unit is loud.
"""

from __future__ import annotations

import warnings
from pathlib import Path

import pytest

import ifcfast
from ifcfast.header import header as ifc_header
from ifcfast.model import _lengthunit_declared, _strict_signal

FIXTURES = Path(__file__).parent / "fixtures"
NO_UNIT = FIXTURES / "no_lengthunit.ifc"          # no LENGTHUNIT at all
BROKEN = FIXTURES / "broken_conversion_unit.ifc"  # LENGTHUNIT, unresolvable
MINIMAL = FIXTURES / "minimal.ifc"                # clean metres


@pytest.fixture
def fresh_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "cache"))
    yield


def _open(p, **kw):
    # Bypass the parquet cache so each assertion sees a cold classify.
    kw.setdefault("use_cache", False)
    kw.setdefault("write_cache", False)
    return ifcfast.open(p, **kw)


# ----------------------------------------------------------------------
# _strict_signal — the single raise/warn channel.
# ----------------------------------------------------------------------

def test_strict_signal_raises_when_strict():
    with pytest.raises(ValueError, match="boom"):
        _strict_signal("boom", strict=True)


def test_strict_signal_warns_when_not_strict():
    with pytest.warns(UserWarning, match="boom"):
        _strict_signal("boom", strict=False)


# ----------------------------------------------------------------------
# #73 — the unit fix (highest value: silently-wrong numbers).
# ----------------------------------------------------------------------

def test_broken_conversion_unit_raises_under_strict(fresh_cache):
    with pytest.raises(ValueError, match="LENGTHUNIT"):
        _open(BROKEN, strict=True)


def test_broken_conversion_unit_warns_under_non_strict(fresh_cache):
    with pytest.warns(UserWarning, match="LENGTHUNIT"):
        m = _open(BROKEN, strict=False)
    assert m.unit_scale is None
    assert m.unit_resolved is False
    # length_unit must say "unknown", never the silently-wrong "m".
    assert m.length_unit == "unknown"


def test_unitless_file_is_silent_and_resolved(fresh_cache):
    # No LENGTHUNIT declared at all -> explicit metres assumption, not a
    # masked unit. Must NOT raise even under strict, and must NOT warn.
    with warnings.catch_warnings():
        warnings.simplefilter("error")  # any UserWarning would fail here
        m = _open(NO_UNIT, strict=True)
    assert m.unit_scale is None
    assert m.unit_resolved is True
    assert m.length_unit == "unknown"


def test_length_unit_unknown_not_metres_when_scale_none(fresh_cache):
    m = _open(NO_UNIT, strict=False)
    assert m.length_unit == "unknown"
    assert m.length_unit != "m"


def test_clean_metres_file_opens_under_strict(fresh_cache):
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        m = _open(MINIMAL, strict=True)
    assert m.unit_scale == 1.0
    assert m.unit_resolved is True
    assert m.length_unit == "m"


def test_bundled_example_opens_clean_under_strict(fresh_cache):
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        m = _open(ifcfast.example_path(), strict=True)
    assert m.unit_resolved is True
    assert m.length_unit == "m"


# ----------------------------------------------------------------------
# summary() carries the loud signal (mirrored into MCP summary).
# ----------------------------------------------------------------------

def test_summary_carries_unit_resolved(fresh_cache):
    m = _open(MINIMAL, strict=True)
    s = m.summary()
    assert s["unit_resolved"] is True
    assert s["length_unit"] == "m"

    mb = _open(BROKEN, strict=False)
    sb = mb.summary()
    assert sb["unit_resolved"] is False
    assert sb["length_unit"] == "unknown"


# ----------------------------------------------------------------------
# _lengthunit_declared discriminator — declared-but-unresolved vs absent.
# ----------------------------------------------------------------------

def test_lengthunit_declared_discriminates():
    assert _lengthunit_declared(BROKEN) is True
    assert _lengthunit_declared(MINIMAL) is True
    # No LENGTHUNIT in DATA, and the FILE_NAME containing the literal
    # token must NOT false-positive.
    assert _lengthunit_declared(NO_UNIT) is False


# ----------------------------------------------------------------------
# strict default is True.
# ----------------------------------------------------------------------

def test_strict_defaults_to_true(fresh_cache):
    # No explicit strict= -> the broken-unit file should raise.
    with pytest.raises(ValueError, match="LENGTHUNIT"):
        ifcfast.open(BROKEN, use_cache=False, write_cache=False)


def test_model_carries_strict_flag(fresh_cache):
    m = _open(MINIMAL, strict=False)
    assert m._strict is False
    m2 = _open(MINIMAL, strict=True)
    assert m2._strict is True


# ----------------------------------------------------------------------
# header(strict=) gates soft anomalies; defaults stay lenient.
# ----------------------------------------------------------------------

def _write_ifc(path: Path, *, schema_line: str) -> Path:
    path.write_text(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('x'),'2;1');\n"
        "FILE_NAME('t.ifc','2026-06-20T00:00:00',('t'),('t'),'t','t','');\n"
        f"{schema_line}\n"
        "ENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n"
    )
    return path


def test_header_strict_raises_on_unknown_schema(tmp_path):
    bad = _write_ifc(tmp_path / "noschema.ifc", schema_line="FILE_SCHEMA(());")
    with pytest.raises(ValueError, match="UNKNOWN|FILE_SCHEMA"):
        ifc_header(bad, strict=True)
    # Lenient default: no raise.
    h = ifc_header(bad, strict=False)
    assert h.schema in ("UNKNOWN", "")


def test_header_default_is_lenient(tmp_path):
    good = _write_ifc(tmp_path / "ok.ifc", schema_line="FILE_SCHEMA(('IFC4'));")
    h = ifc_header(good)  # no strict= -> lenient, must not raise
    assert h.schema == "IFC4"


# ----------------------------------------------------------------------
# Review fix: encoding_lossy is WARN-only, even under the strict default.
# A valid-but-Latin-1 IFC (Norwegian æ/ø/å in a Name) must NOT raise.
# ----------------------------------------------------------------------

LATIN1 = FIXTURES / "latin1_encoded.ifc"  # valid IFC4, ISO-8859-1 'ø'


def test_latin1_header_warns_not_raises_under_strict():
    # Encoding lossiness corrupts STRINGS not NUMBERS, so it stays a
    # capturable warning even when strict raises everything else.
    with pytest.warns(UserWarning):
        h = ifc_header(LATIN1, strict=True)
    assert h.encoding_lossy is True
    assert h.schema == "IFC4"


def test_latin1_open_under_strict_default_does_not_raise(fresh_cache):
    # The bar from the reviewer: opening a valid Latin-1 file under the
    # strict=True DEFAULT must NOT raise, warns at most, returns a usable
    # Model. This is why default_flip_safe=False could now flip True.
    with warnings.catch_warnings():
        warnings.simplefilter("always")
        m = ifcfast.open(LATIN1, use_cache=False, write_cache=False)  # strict default
    assert len(m) >= 1  # a usable, populated Model
    assert m.schema == "IFC4"
    assert m.unit_resolved is True
