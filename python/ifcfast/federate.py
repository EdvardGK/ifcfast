"""First-class federation of ifcfast substrate bundles (GH #50).

``ifcfast.clash()`` runs against ONE bundle dir; cross-discipline clash
therefore needs the constituent bundles merged into a single substrate.
:func:`federate` is that merge as a product surface — promoted from the
oracle reference ``tests/oracle/federate.py``, which stays frozen in
``tests/`` as the differential spec (``tests/test_federate_parity.py``
gates table-level equality between the two).

The merge is pure columnar surgery, and two edges are load-bearing:

1. ``rep_id`` is numbered per bundle — every source after the first has
   its rep_ids offset by the running maximum, in BOTH ``instances`` and
   ``representations``, so geometry never cross-links.
2. The Rust substrate reader is strict on arrow types — tables are
   merged with pyarrow preserving the EXACT source schema (a pandas
   round-trip silently widens ``string`` to ``large_string`` and gets
   rejected).

Identity in a federated bundle: ``ifc_id`` (STEP entity id) and even
``guid`` may collide across sources (the same element exported into two
models, or the same model federated twice). Every instance row is
therefore re-stamped with ``source_model`` = the constituent bundle
directory's name, making ``(guid, source_model)`` — and
``(ifc_id, source_model)`` — the unique join keys. ``clashes.parquet``
carries ``source_model_a`` / ``source_model_b`` for the same reason.

Reference-only models are recorded here but ENFORCED at clash time
(``ifcfast.clash(..., reference_only=...)``): one federated bundle
serves every reference-set choice without re-merging.
"""

from __future__ import annotations

import hashlib
import json
import warnings
from os import PathLike
from pathlib import Path
from typing import Iterable, Literal

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

_TABLES = ("instances", "representations")

_SOURCE_MODEL_FIELD = pa.field("source_model", pa.string(), nullable=False)


def _read(bundle_dir: Path, table: str) -> pa.Table:
    f = bundle_dir / f"{table}.parquet"
    if not f.is_file():
        raise FileNotFoundError(
            f"{bundle_dir} is not a substrate bundle: missing {f.name}"
        )
    return pq.read_table(f)


def _unit_scale(t: pa.Table) -> str:
    meta = t.schema.metadata or {}
    scale = meta.get(b"ifcfast.unit_scale")
    if scale is None:
        raise ValueError("bundle parquet missing ifcfast.unit_scale metadata")
    return scale.decode()


def _offset_rep_id(t: pa.Table, offset: int) -> pa.Table:
    if offset == 0:
        return t
    idx = t.schema.get_field_index("rep_id")
    col = pc.add(t.column("rep_id"), pa.scalar(offset, pa.uint64()))
    # add() may widen nullability but keeps uint64; re-assert the field type
    return t.set_column(idx, t.schema.field(idx), col.cast(pa.uint64()))


def _stamp_source_model(t: pa.Table, stem: str) -> pa.Table:
    """Re-stamp every row's ``source_model`` with the bundle dir name.

    ``bundle()`` writes the source IFC's file stem; federation replaces
    it with the constituent DIRECTORY name so the column always matches
    the sidecar's ``rep_id_offsets`` / ``guid_source`` keys. Bundles
    from before cache schema v29 lack the column — it is appended (at
    the end, where the v29 writer puts it) so a federated bundle always
    carries it.
    """
    col = pa.array([stem] * t.num_rows, pa.string())
    idx = t.schema.get_field_index("source_model")
    if idx == -1:
        return t.append_column(_SOURCE_MODEL_FIELD, col)
    return t.set_column(idx, t.schema.field(idx), col)


def federate(
    bundles: Iterable[Path | str],
    out_dir: Path | str,
    *,
    on_collision: Literal["warn", "fail", "dedup"] = "warn",
    reference_only: Iterable[str] = (),
) -> dict:
    """Merge N substrate bundles into one clash-able bundle at ``out_dir``.

    Args:
        bundles: two or more bundle directories (each holding
            ``instances.parquet`` + ``representations.parquet``, the
            output of :func:`ifcfast.bundle`). Directory NAMES must be
            unique — they become the ``source_model`` stamp.
        out_dir: destination directory (created if missing). Receives
            the merged ``instances.parquet`` / ``representations.parquet``,
            a ``federation.json`` sidecar, and a copy of ``view.sql``
            from the first constituent that has one.
        on_collision: policy for guids appearing in more than one
            source (the same element exported into two models would
            self-clash as noise):

            * ``"warn"`` (default) — keep all rows, emit a warning,
              report the guids in the sidecar.
            * ``"fail"`` — raise :class:`ValueError`.
            * ``"dedup"`` — keep the FIRST source's instance row and
              drop later duplicates. Their representations are left in
              place as orphans (the clash engine only meshes reps that
              an instance references).
        reference_only: ``source_model`` names (constituent dir names)
            to record as pure reference geometry. Recorded in the
            sidecar only — enforcement happens at clash time via
            ``ifcfast.clash(..., reference_only=...)``, so one
            federated bundle serves every reference-set choice.

    Returns:
        The sidecar dict (also written to ``out_dir/federation.json``):
        ``{"sources": [...], "unit_scale": str, "rep_id_offsets":
        {stem: int}, "n_instances": int, "n_representations": int,
        "guid_collisions": [...], "guid_source": {guid: stem},
        "on_collision": str, "reference_only": [...]}``.

    Raises:
        ValueError: fewer than two bundles, duplicate bundle dir names,
            mismatched schemas (re-bundle all sources with the same
            ifcfast version), mixed ``unit_scale`` across sources,
            unknown ``on_collision`` / ``reference_only`` values, or
            guid collisions under ``on_collision="fail"``.
        FileNotFoundError: a source dir is missing its parquet files.
    """
    bundle_dirs = [Path(d) for d in bundles]
    if len(bundle_dirs) < 2:
        raise ValueError("federation needs at least two bundles")
    if on_collision not in ("warn", "fail", "dedup"):
        raise ValueError(
            f"on_collision must be 'warn', 'fail' or 'dedup', got {on_collision!r}"
        )
    stems = [d.name for d in bundle_dirs]
    if len(set(stems)) != len(stems):
        dupes = sorted({s for s in stems if stems.count(s) > 1})
        raise ValueError(
            f"bundle directory names must be unique (they become source_model): "
            f"duplicated {dupes} — rename or re-bundle into distinct dirs"
        )
    if isinstance(reference_only, (str, bytes)):
        # tuple("ark") == ('a', 'r', 'k') — a bare string would produce
        # a baffling unknown-names error instead of a clear one.
        raise TypeError(
            "reference_only must be an iterable of source_model names, "
            "not a bare string — pass ('name',) or ['name']"
        )
    reference_only = tuple(reference_only)
    unknown_refs = sorted(set(reference_only) - set(stems))
    if unknown_refs:
        raise ValueError(
            f"reference_only names {unknown_refs} are not among the "
            f"federated bundles {stems}"
        )
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    inst_parts: list[pa.Table] = []
    rep_parts: list[pa.Table] = []
    guid_source: dict[str, str] = {}
    collisions: list[str] = []
    offsets: dict[str, int] = {}
    next_offset = 0
    scales: set[str] = set()
    schema0 = None

    for d in bundle_dirs:
        stem = d.name
        inst = _read(d, "instances")
        rep = _read(d, "representations")
        scales.add(_unit_scale(inst))
        scales.add(_unit_scale(rep))
        if schema0 is None:
            schema0 = inst.schema
        elif inst.schema != schema0:
            raise ValueError(
                f"{d}: instances schema differs from {bundle_dirs[0]} — "
                "re-bundle all sources with the same ifcfast version"
            )

        keep_mask: list[bool] = []
        for g in inst.column("guid").to_pylist():
            if g in guid_source:
                collisions.append(g)
                keep_mask.append(False)
            else:
                guid_source[g] = stem
                keep_mask.append(True)
        if on_collision == "dedup" and not all(keep_mask):
            inst = inst.filter(pa.array(keep_mask, pa.bool_()))

        offsets[stem] = next_offset
        inst_parts.append(_stamp_source_model(_offset_rep_id(inst, next_offset), stem))
        rep_parts.append(_offset_rep_id(rep, next_offset))
        max_rep = pc.max(rep.column("rep_id")).as_py()
        next_offset += (max_rep if max_rep is not None else 0) + 1

    if len(scales) != 1:
        raise ValueError(
            f"unit_scale differs across bundles ({sorted(scales)}); "
            "cannot federate mixed-unit substrates"
        )
    if collisions:
        if on_collision == "fail":
            sample = sorted(set(collisions))[:5]
            raise ValueError(
                f"{len(set(collisions))} guid(s) appear in more than one "
                f"source (e.g. {sample}); same-element duplicates would "
                f"self-clash as noise. Re-export, or pass "
                f"on_collision='warn' to keep or 'dedup' to drop later copies."
            )
        if on_collision == "warn":
            warnings.warn(
                f"federate: {len(set(collisions))} guid(s) appear in more "
                f"than one source bundle — duplicate elements will "
                f"self-clash. See federation.json guid_collisions.",
                stacklevel=2,
            )

    merged_inst = pa.concat_tables(inst_parts)
    merged_rep = pa.concat_tables(rep_parts)
    pq.write_table(merged_inst, out_dir / "instances.parquet")
    pq.write_table(merged_rep, out_dir / "representations.parquet")

    # Same substrate convenience as bundle(): ship the DuckDB view next
    # to the parquets. Constituents all carry identical view.sql (it is
    # version-static), so the first one found is authoritative.
    for d in bundle_dirs:
        src_view = d / "view.sql"
        if src_view.is_file():
            (out_dir / "view.sql").write_bytes(src_view.read_bytes())
            break

    sidecar = {
        "sources": [str(d) for d in bundle_dirs],
        "unit_scale": next(iter(scales)),
        "rep_id_offsets": offsets,
        "n_instances": merged_inst.num_rows,
        "n_representations": merged_rep.num_rows,
        "guid_collisions": sorted(set(collisions)),
        "guid_source": guid_source,
        "on_collision": on_collision,
        "reference_only": sorted(reference_only),
    }
    # Written LAST: its presence marks the merge complete (the clash()
    # federation cache treats a dir without it as a torn write).
    (out_dir / "federation.json").write_text(json.dumps(sidecar, indent=1))
    return sidecar


# -- federation cache (used by the clash([a, b]) sugar) -----------------

_FP_SAMPLE_BYTES = 4 * 1024 * 1024  # mirror header._compute_cache_key


def _bundle_fingerprint(h, d: Path) -> None:
    """Feed one constituent's identity into ``h``.

    Content-keyed the same way the parse cache keys IFCs
    (``header._compute_cache_key``): size + head/tail samples of both
    parquets — robust to ``touch``, cheap on multi-GB substrates. The
    dir NAME is included because it becomes the ``source_model`` stamp:
    same bytes under a different name is a different federated table.
    """
    h.update(d.name.encode())
    for table in _TABLES:
        f = d / f"{table}.parquet"
        size = f.stat().st_size
        h.update(size.to_bytes(8, "little"))
        with open(f, "rb") as fh:
            h.update(fh.read(_FP_SAMPLE_BYTES))
            if size > _FP_SAMPLE_BYTES:
                fh.seek(max(size - _FP_SAMPLE_BYTES, _FP_SAMPLE_BYTES))
                h.update(fh.read(_FP_SAMPLE_BYTES))


def federation_cache_dir(
    bundle_dirs: list[Path], on_collision: str = "warn"
) -> Path:
    """Cache location for a federation of ``bundle_dirs``, in order.

    Keyed on constituent bundle identity (name + content fingerprint),
    the cache schema version, and — only when it changes the merged
    tables — the collision policy. Order matters: rep_id offsets depend
    on it. Lives under ``ifcfast.cache.cache_root()/federated/``.
    """
    from .cache import cache_root
    from .header import _CACHE_SCHEMA_VERSION

    h = hashlib.sha256()
    h.update(_CACHE_SCHEMA_VERSION.to_bytes(4, "little"))
    # warn/fail produce identical tables; only dedup changes content.
    h.update((b"dedup" if on_collision == "dedup" else b"keep"))
    for d in bundle_dirs:
        _bundle_fingerprint(h, d)
    return cache_root() / "federated" / h.hexdigest()[:16]


__all__ = ["federate", "federation_cache_dir"]
