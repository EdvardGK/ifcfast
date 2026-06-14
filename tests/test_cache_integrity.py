"""Cache-integrity regression tests for GH #80.

Two parquet-cache integrity gaps are covered:

  1. A same-size mid-file edit must NOT serve a stale cached model. The
     cache key only hashes the head/tail 4 MB windows, so an in-place
     edit confined to the middle of a >8 MB file keeps the same key; the
     fix compares the manifest's recorded (size_bytes, mtime_ns) against
     the live stat on every cache read and treats a mismatch as a miss.

  2. A partial / empty cache must NOT read back as a valid hit. The fix
     writes parquets + manifest atomically (temp + os.replace) and only
     honours an index whose manifest carries `has_index` AND whose
     index.parquet is present and non-empty; flagged-but-missing
     relationship tables are treated as corruption → clean re-parse.

These run against the prebuilt local `_core` (Python-only fix), using a
private IFCFAST_CACHE dir per test so they never touch the user cache.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

import ifcfast
from ifcfast import cache as _cache
from ifcfast.header import (
    _HASH_HEAD_BYTES,
    _HASH_TAIL_BYTES,
    header as _header,
)

FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"

# The wall whose name we flip lives between the head and tail hash
# windows, so a same-size edit to it is invisible to the cache key.
MID_WALL_GUID = "1midWALLr0kugbFTf53O9z"


@pytest.fixture(autouse=True)
def _isolated_cache(tmp_path, monkeypatch):
    """Point the cache at a throwaway dir so tests are hermetic."""
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "ifcfast-cache"))
    yield


def _build_large_ifc(dest: Path, mid_wall_name: str) -> int:
    """Write a >8 MB IFC. A wall named `mid_wall_name` is placed in the
    dead-centre of the file (past the 4 MB head window, before the 4 MB
    tail window) behind a large whitespace pad, so editing its name in
    place is invisible to the head/tail cache key. Returns the byte
    offset of the name's first character (for a same-size in-place flip).

    STEP permits arbitrary whitespace between tokens, so a multi-MB run
    of spaces is a valid, parser-transparent way to grow the file past
    the 8 MB head+tail hash budget without inflating the entity count.
    """
    head = (
        "ISO-10303-21;\n"
        "HEADER;\n"
        "FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');\n"
        "FILE_NAME('large.ifc','2026-06-13T00:00:00',('ifcfast tests'),"
        "('Skiplum'),'ifcfast','ifcfast-tests','');\n"
        "FILE_SCHEMA(('IFC4'));\n"
        "ENDSEC;\n"
        "DATA;\n"
        "#1=IFCPROJECT('0YvctVUKr0kugbFTf53O9L',$,'Large Project',$,$,$,$,"
        "(#5),#2);\n"
        "#2=IFCUNITASSIGNMENT((#3,#4));\n"
        "#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n"
        "#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);\n"
        "#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);\n"
        "#6=IFCAXIS2PLACEMENT3D(#7,$,$);\n"
        "#7=IFCCARTESIANPOINT((0.,0.,0.));\n"
        "#11=IFCBUILDING('2XvctVUKr0kugbFTf53O9L',$,'Building A',$,$,$,$,$,"
        ".ELEMENT.,$,$,$);\n"
        "#12=IFCBUILDINGSTOREY('3XvctVUKr0kugbFTf53O9L',$,'Plan 01',$,$,$,$,"
        "'Plan 01',.ELEMENT.,0.0);\n"
        "#20=IFCRELAGGREGATES('4XvctVUKr0kugbFTf53O9L',$,$,$,#1,(#11));\n"
        "#21=IFCRELAGGREGATES('5XvctVUKr0kugbFTf53O9L',$,$,$,#11,(#12));\n"
    )

    # Whitespace pad to push the mid-file wall past the 4 MB head window.
    pad = "\n" + " " * (_HASH_HEAD_BYTES + 1_000_000) + "\n"

    mid_wall_prefix = f"#30=IFCWALL('{MID_WALL_GUID}',$,'"
    mid_wall_suffix = "',$,$,$,$,'tag-mid',.STANDARD.);\n"
    mid_wall = mid_wall_prefix + mid_wall_name + mid_wall_suffix

    # Tail pad so the head/tail windows do not jointly cover the whole
    # file (i.e. the file is genuinely >8 MB with a gap around the wall).
    tail_pad = " " * (_HASH_TAIL_BYTES + 1_000_000) + "\n"

    footer = "ENDSEC;\nEND-ISO-10303-21;\n"

    content = head + pad + mid_wall + tail_pad + footer
    data = content.encode("utf-8")
    dest.write_bytes(data)

    name_offset = len(
        (head + pad + mid_wall_prefix).encode("utf-8")
    )
    return name_offset


def _wall_name_for(model, guid: str):
    df = model.products_df
    row = df[df["guid"] == guid]
    if row.empty:
        return None
    return row.iloc[0]["name"]


def test_same_size_mid_file_edit_not_stale(tmp_path):
    """GH #80 (1): flipping a byte in a mid-file wall name, keeping the
    byte length identical, must surface on reopen — not the stale cached
    name. Verifies the cache key is unchanged (so the bug *would* fire
    without the stat check) and that the model still reflects the edit.
    """
    ifc = tmp_path / "large.ifc"
    name_offset = _build_large_ifc(ifc, "WallAAA")

    # Cold open → writes cache.
    m1 = ifcfast.open(ifc)
    assert _wall_name_for(m1, MID_WALL_GUID) == "WallAAA"

    key_before = _header(ifc).cache_key

    # Same-size in-place edit: 'W' -> 'Z' at the first name byte.
    raw = bytearray(ifc.read_bytes())
    assert raw[name_offset:name_offset + 1] == b"W"
    raw[name_offset] = ord("Z")
    assert len(raw) == ifc.stat().st_size  # size preserved
    ifc.write_bytes(raw)
    # Advance mtime explicitly so the test is robust even on coarse
    # filesystem timestamp resolution (the byte rewrite normally bumps
    # it, but make the intent unambiguous).
    st = ifc.stat()
    os.utime(ifc, ns=(st.st_atime_ns, st.st_mtime_ns + 1_000_000))

    key_after = _header(ifc).cache_key
    assert key_after == key_before, (
        "precondition: a mid-file same-size edit keeps the cache key; "
        "if this fails the test no longer exercises the staleness bug"
    )

    # Reopen — must reflect the edit, not the stale cached model.
    m2 = ifcfast.open(ifc)
    assert _wall_name_for(m2, MID_WALL_GUID) == "ZallAAA", (
        "stale cache served after a same-size mid-file edit (GH #80)"
    )

    # And the cache should now have been rewritten so a third open is a
    # clean hit reflecting the new content.
    assert _cache.is_index_cached(_header(ifc))
    m3 = ifcfast.open(ifc)
    assert _wall_name_for(m3, MID_WALL_GUID) == "ZallAAA"


def test_truncated_index_parquet_is_not_a_hit(tmp_path):
    """GH #80 (2): an empty/truncated index.parquet must not read back as
    a valid cache hit. Simulate a crash that left a zero-byte
    index.parquet behind a valid-looking manifest.
    """
    m1 = ifcfast.open(FIXTURE)
    hdr = _header(FIXTURE)
    assert _cache.is_index_cached(hdr)

    cdir = _cache.cache_dir_for(hdr)
    idx = cdir / "index.parquet"
    assert idx.exists()

    # Truncate to zero bytes — the partial-write residue.
    idx.write_bytes(b"")

    assert not _cache.is_index_cached(hdr), (
        "empty index.parquet was treated as a valid cache hit (GH #80)"
    )
    assert _cache.read_index(hdr) is None

    # A fresh open must recover by re-parsing (no exception, real data).
    m2 = ifcfast.open(FIXTURE)
    assert len(m2.products_df) == len(m1.products_df)
    assert idx.stat().st_size > 0  # rewritten atomically


def test_data_only_manifest_does_not_fake_index(tmp_path):
    """GH #80 (2): a manifest created by a data-layer extract (no index
    written) must not be served as an index hit. Pre-fix, the manifest
    carried a valid cache_version/cache_key and `is_index_cached` only
    checked that index.parquet existed — here it never does, but the
    `has_index` flag is the real gate.
    """
    # Data layers only — writes psets/etc + a manifest, but no index.
    _cache.extract_data_layers(FIXTURE)
    hdr = _header(FIXTURE)
    cdir = _cache.cache_dir_for(hdr)
    manifest = _cache._read_manifest(cdir)

    assert manifest is not None
    assert not manifest.get("has_index")
    # No index.parquet was written by the data path.
    assert not (cdir / "index.parquet").exists()
    assert not _cache.is_index_cached(hdr)


def test_flagged_but_missing_relationship_table_is_corruption(tmp_path):
    """GH #80 (2): if the manifest flags a relationship table that is not
    on disk, the read must fail to a clean re-parse rather than fabricate
    empty edge DataFrames (which would silently return None/[] for
    storey_of / children / products_in).
    """
    ifcfast.open(FIXTURE)
    hdr = _header(FIXTURE)
    cdir = _cache.cache_dir_for(hdr)
    manifest = _cache._read_manifest(cdir)
    assert manifest.get("has_aggregates")  # minimal.ifc has aggregates

    # Simulate a crash between index.parquet and the aggregates parquet.
    (cdir / _cache.AGGREGATES_FILE).unlink()

    assert _cache.read_index(hdr) is None, (
        "flagged-but-missing relationship table served as a hit (GH #80)"
    )
    assert not _cache.is_index_cached(hdr) or _cache.read_index(hdr) is None
