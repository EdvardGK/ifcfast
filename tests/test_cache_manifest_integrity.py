"""Manifest-integrity regressions for GH #158.

Three separate ways the parquet cache could serve — or refuse to serve —
the wrong thing:

  1. `_patch_data_manifest` reused whatever manifest was already on disk
     and never refreshed `(size_bytes, mtime_ns)` from the live source.
     `_source_matches` then stayed False forever for the data-layer
     path, so `extract_data_layers()` / `ifcfast extract` re-parsed on
     every call. (`open_ifc` masked it: `write_index` refreshes the same
     keys on the tier-1 path.)

  2. `_classifier_signature` hashed only four entity frozensets. The
     inheritance-rule parent sets and the generated per-schema
     SUPERTYPE map — both of which decide an element's `mode` — were
     invisible, so regenerating `data/schema_supertypes.py` left stale
     `mode` rows cached indefinitely.

  3. The manifest read-modify-write was unserialised across processes:
     two workers on one file could lose each other's `has_*` flags.

Hermetic: each test points IFCFAST_CACHE at a throwaway dir.
"""

from __future__ import annotations

import multiprocessing as mp
import shutil
import sys
from pathlib import Path

import pytest

import ifcfast
from ifcfast import cache as _cache
from ifcfast.header import header as _header

FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"


@pytest.fixture(autouse=True)
def _isolated_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("IFCFAST_CACHE", str(tmp_path / "ifcfast-cache"))
    # The classifier signature is memoised per process; tests that
    # monkeypatch classify inputs need a clean slate.
    monkeypatch.setattr(_cache, "_CLASSIFIER_SIGNATURE", None)
    yield


# ----------------------------------------------------------------------
# (1) data-path manifest must carry the LIVE source stat
# ----------------------------------------------------------------------


def test_data_manifest_refreshes_source_stat(tmp_path):
    """A copy with a new mtime must re-validate after one re-extract.

    Pre-fix, `_patch_data_manifest` inherited the stale
    `(size_bytes, mtime_ns)` from the manifest it merged into, so the
    freshly-written data layers were immediately unusable — every
    subsequent `extract_data_layers()` missed the cache and re-parsed.
    """
    src = tmp_path / "a.ifc"
    shutil.copy(FIXTURE, src)

    # First extract: cold, writes parquets + manifest.
    first = _cache.extract_data_layers(src, use_cache=True, write_cache=True)
    assert first.psets is not None

    hdr = _header(src)
    cache_dir = _cache.cache_dir_for(hdr)
    manifest = _cache._read_manifest(cache_dir)
    assert manifest["size_bytes"] == hdr.size_bytes
    assert manifest["mtime_ns"] == hdr.mtime_ns
    assert _cache._source_matches(hdr, manifest)

    # Now simulate the masked bug directly: age the recorded stat, then
    # re-extract. The rewritten manifest must describe the LIVE file.
    manifest["mtime_ns"] = 1
    manifest["size_bytes"] = 1
    _cache._write_manifest(cache_dir, manifest)
    assert not _cache._source_matches(hdr, _cache._read_manifest(cache_dir))

    _cache.extract_data_layers(src, use_cache=True, write_cache=True)
    refreshed = _cache._read_manifest(cache_dir)
    assert refreshed["size_bytes"] == hdr.size_bytes
    assert refreshed["mtime_ns"] == hdr.mtime_ns
    assert _cache._source_matches(hdr, refreshed)


def test_data_manifest_write_is_a_cache_hit_on_second_call(tmp_path):
    """End-to-end: extract twice, the second call reads parquet."""
    src = tmp_path / "b.ifc"
    shutil.copy(FIXTURE, src)

    first = _cache.extract_data_layers(src, use_cache=True, write_cache=True)
    assert first.timing_ms["cold_parse"] is True
    second = _cache.extract_data_layers(src, use_cache=True, write_cache=True)
    # `cold_parse=False` is the decode path — the manifest the first
    # call wrote validated against the live source stat.
    assert second.timing_ms["cold_parse"] is False, second.timing_ms


def test_data_manifest_does_not_inherit_a_foreign_cache_version(tmp_path):
    """A manifest from a different on-disk layout is dropped, not merged.

    Merging into it would carry its `has_*` flags forward and stamp them
    with the current CACHE_VERSION — blessing old-layout parquets that
    every read gate would otherwise reject.
    """
    src = tmp_path / "c.ifc"
    shutil.copy(FIXTURE, src)
    hdr = _header(src)
    cache_dir = _cache.cache_dir_for(hdr)
    cache_dir.mkdir(parents=True, exist_ok=True)
    _cache._write_manifest(
        cache_dir,
        {
            "cache_version": _cache.CACHE_VERSION - 1,
            "has_index": True,
            "has_spaces": True,
            "bogus_key_from_an_old_layout": True,
        },
    )

    _cache.extract_data_layers(src, use_cache=False, write_cache=True)
    m = _cache._read_manifest(cache_dir)
    assert m["cache_version"] == _cache.CACHE_VERSION
    assert "bogus_key_from_an_old_layout" not in m
    assert "has_index" not in m  # the data path never claims an index


# ----------------------------------------------------------------------
# (2) classifier signature covers everything classify_by_name reads
# ----------------------------------------------------------------------


def _sig_after(monkeypatch, **attrs) -> str:
    from ifcfast import classify

    monkeypatch.setattr(_cache, "_CLASSIFIER_SIGNATURE", None)
    for k, v in attrs.items():
        monkeypatch.setattr(classify, k, v)
    return _cache._classifier_signature()


def test_signature_changes_with_count_parent_types(monkeypatch):
    from ifcfast import classify

    base = _sig_after(monkeypatch)
    moved = _sig_after(
        monkeypatch,
        _COUNT_PARENT_TYPES=frozenset(classify._COUNT_PARENT_TYPES | {"IfcWall"}),
    )
    assert moved != base


def test_signature_changes_with_builtelement_rule(monkeypatch):
    """The IfcBuildingElement / IfcBuiltElement MEASURE rule is an input."""
    base = _sig_after(monkeypatch)
    narrowed = _sig_after(
        monkeypatch, _MEASURE_PARENT_TYPES=frozenset({"IfcBuildingElement"})
    )
    assert narrowed != base

    monkeypatch.setattr(_cache, "_CLASSIFIER_SIGNATURE", None)
    linear = _sig_after(monkeypatch, _LINEAR_PARENT_TYPES=frozenset())
    assert linear != base


def test_signature_changes_with_supertype_map(monkeypatch):
    """Regenerating schema_supertypes.py must orphan cached `mode` rows.

    A re-parented entity changes what `classify_by_name` returns with no
    change to any explicit entity set — the exact case that stayed
    cached forever before GH #158.
    """
    from ifcfast.data import schema_supertypes

    base = _cache._classifier_signature()

    mutated = {
        schema: dict(parents)
        for schema, parents in schema_supertypes.SUPERTYPE.items()
    }
    some_schema = sorted(mutated)[0]
    mutated[some_schema]["IfcTotallyMadeUpEntity"] = "IfcBuiltElement"

    monkeypatch.setattr(schema_supertypes, "SUPERTYPE", mutated)
    monkeypatch.setattr(_cache, "_CLASSIFIER_SIGNATURE", None)
    assert _cache._classifier_signature() != base


def test_signature_is_stable_and_memoised():
    a = _cache._classifier_signature()
    b = _cache._classifier_signature()
    assert a == b
    assert len(a) == 12


# ----------------------------------------------------------------------
# (3) manifest read-modify-write is serialised
# ----------------------------------------------------------------------


def _hold_lock_and_patch(cache_dir_str: str, key: str, ready, go) -> None:  # pragma: no cover
    """Child process: take the lock, wait, then merge its own key in."""
    from pathlib import Path as _P

    from ifcfast import cache as c

    d = _P(cache_dir_str)
    with c._manifest_lock(d):
        ready.set()
        go.wait(10)
        m = c._existing_manifest_or_empty(d)
        m[key] = True
        c._write_manifest(d, m)


def test_manifest_lock_serialises_concurrent_patches(tmp_path):
    """Two processes patching the same manifest keep BOTH keys.

    The GH #158 failure: B reads before A writes, B's write drops A's
    `has_index`, and `is_index_cached` never honours the index parquet
    that is sitting right there.
    """
    if sys.platform.startswith("win"):  # pragma: no cover
        pytest.skip("spawn semantics differ; POSIX flock is the gated path")
    d = tmp_path / "cache-dir"
    d.mkdir()
    _cache._write_manifest(d, {"cache_version": _cache.CACHE_VERSION})

    ctx = mp.get_context("fork")
    ready, go = ctx.Event(), ctx.Event()
    child = ctx.Process(
        target=_hold_lock_and_patch, args=(str(d), "has_index", ready, go)
    )
    child.start()
    try:
        assert ready.wait(10), "child never acquired the lock"
        # The child is holding the lock. Release it and race our own
        # read-modify-write against the child's.
        go.set()
        with _cache._manifest_lock(d):
            m = _cache._existing_manifest_or_empty(d)
            m["has_psets"] = True
            _cache._write_manifest(d, m)
    finally:
        child.join(15)

    final = _cache._read_manifest(d)
    assert final["has_index"] is True, final
    assert final["has_psets"] is True, final


def test_manifest_lock_is_reentrant_across_sequential_writers(tmp_path):
    """Sanity: the lock releases, so a second acquire in-process works."""
    d = tmp_path / "seq"
    for key in ("a", "b", "c"):
        with _cache._manifest_lock(d):
            m = _cache._existing_manifest_or_empty(d)
            m["cache_version"] = _cache.CACHE_VERSION
            m[key] = True
            _cache._write_manifest(d, m)
    m = _cache._read_manifest(d)
    assert m["a"] and m["b"] and m["c"]
    # The lock file is left in place on purpose — unlinking it would
    # race the next acquirer onto a fresh inode.
    assert (d / "meta.json.lock").exists()


def test_write_index_and_data_patch_do_not_clobber_each_other(tmp_path):
    """The real pairing: index flags survive a later data-layer patch."""
    src = tmp_path / "d.ifc"
    shutil.copy(FIXTURE, src)

    m = ifcfast.open(src)                     # write_index
    hdr = _header(src)
    cache_dir = _cache.cache_dir_for(hdr)
    assert _cache._read_manifest(cache_dir)["has_index"] is True

    _cache.extract_data_layers(src, use_cache=False, write_cache=True)
    after = _cache._read_manifest(cache_dir)
    assert after["has_index"] is True, "data patch dropped the index flag"
    assert after["has_psets"] is True
    assert _cache.is_index_cached(hdr) is True
