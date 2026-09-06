"""First-class federation of ifcfast substrate bundles (GH #50).

``ifcfast.clash()`` runs against ONE bundle dir; cross-discipline clash
therefore needs the constituent bundles merged into a single substrate.
:func:`federate` is that merge as a product surface — promoted from the
oracle reference ``tests/oracle/federate.py``, which stays frozen in
``tests/`` as the differential spec (``tests/test_federate_parity.py``
gates table-level equality between the two).

The merge is pure columnar surgery, and three edges are load-bearing:

1. ``rep_id`` is numbered per bundle — every source after the first has
   its rep_ids offset by the running maximum, in BOTH ``instances`` and
   ``representations``, so geometry never cross-links.
2. The Rust substrate reader is strict on arrow types — tables are
   merged with pyarrow preserving the EXACT source schema (a pandas
   round-trip silently widens ``string`` to ``large_string`` and gets
   rejected).
3. Constituents may be authored in DIFFERENT length units (GH #169) —
   the normal case across disciplines. The merge converts every
   source-unit length into the FINEST constituent's unit rather than
   refusing; see :func:`federate` and :data:`_LENGTH_FSL_COLUMNS` for
   exactly which columns move and which are unit-independent.

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

import numpy as np
import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

_TABLES = ("instances", "representations")

_SOURCE_MODEL_FIELD = pa.field("source_model", pa.string(), nullable=False)

#: Bumped when the merge ALGORITHM changes in a way that would make a
#: cached federated substrate wrong even though the constituent bytes
#: are unchanged. v2 = GH #169 mixed-unit rescale (a federation of the
#: same bundles now produces a rescaled, single-unit substrate where it
#: previously raised — and, for a cache written by a pre-#169 build, a
#: same-unit merge is unaffected but the key must still move so the two
#: code paths never share a directory).
_FEDERATION_VERSION = 2

#: Every substrate column that is a LENGTH in source units, per table.
#: Enumerated from ``build_representation_schema`` /
#: ``build_instance_schema`` in ``crates/core/src/bundle/parquet_sink.rs``.
#:
#: NOT scaled, deliberately:
#:   * QTO columns (``volume_m3``, ``*_m2``, ``surfaces[].area_m2``,
#:     ``aabb_volume_m3``, ``volume_mesh_m3``, ``volume_prism_bound_m3``)
#:     — already m² / m³, unit-independent by construction.
#:   * ``materials[].thickness_mm`` — the extractor normalises it to
#:     millimetres at parse time (``t * unit_scale * 1000.0``).
#:   * ``representations.indices_le``, ``segments`` (source /
#:     index_start / triangle_count) — topology, no lengths.
#:   * ``quantities[].value`` — raw authored strings, kept verbatim in
#:     the authoring model's own units (they carry ``unit_step_id``).
_LENGTH_FSL_COLUMNS = {
    "instances": (
        "bbox_min_xyz",
        "bbox_max_xyz",
        "centroid_xyz",
        "placement_xyz",
    ),
    "representations": ("local_bbox_min_xyz", "local_bbox_max_xyz"),
}

#: 4x4 column-major transform: only the translation column scales.
_TRANSFORM_TRANSLATION_SLOTS = (12, 13, 14)


def _read(bundle_dir: Path, table: str) -> pa.Table:
    f = bundle_dir / f"{table}.parquet"
    if not f.is_file():
        raise FileNotFoundError(
            f"{bundle_dir} is not a substrate bundle: missing {f.name}"
        )
    return pq.read_table(f)


def _unit_scale(t: pa.Table | pa.Schema) -> tuple[float, str]:
    """``(value, raw)`` for the bundle's ``ifcfast.unit_scale`` metadata.

    The raw string is what the sidecar records; the float is what the
    rescale factor is computed from. Comparing the STRINGS (pre-GH #162)
    rejected ``"0.001"`` against ``"1e-3"`` — the same millimetre
    substrate, written by two ifcfast versions with different float
    formatting — as "mixed units".

    Accepts a table or a bare schema so the pre-pass that picks the
    federation's target unit can read metadata without materialising
    the parquet.
    """
    schema = t if isinstance(t, pa.Schema) else t.schema
    meta = schema.metadata or {}
    scale = meta.get(b"ifcfast.unit_scale")
    if scale is None:
        raise ValueError("bundle parquet missing ifcfast.unit_scale metadata")
    raw = scale.decode()
    try:
        value = float(raw)
    except ValueError:
        raise ValueError(
            f"bundle parquet has a non-numeric ifcfast.unit_scale "
            f"metadata value {raw!r}"
        ) from None
    return value, raw


def _scaled_f32(values: np.ndarray, factor: float) -> np.ndarray:
    """Multiply f32 data by ``factor`` with f64 intermediate rounding.

    ``vertices * np.float32(1000.0)`` rounds twice (once into the f32
    product, once nowhere); promoting to f64 for the multiply and
    rounding ONCE back to f32 gives the correctly-rounded f32 result —
    the same number a re-bundle of the file in the target unit would
    produce, modulo the source's own quantisation.
    """
    return (values.astype(np.float64) * factor).astype(np.float32)


def _rescale_fsl_chunk(chunk: pa.Array, factor: float, slots) -> pa.Array:
    """Scale a ``FixedSizeList<float32>[k]`` chunk.

    ``slots`` is ``None`` to scale every component (xyz columns) or a
    tuple of component indices (the transform's translation column).
    The output is cast back to the input type so the item field's name
    and nullability — which the strict Rust substrate reader checks —
    survive untouched.
    """
    size = chunk.type.list_size
    flat = chunk.flatten().to_numpy(zero_copy_only=False)
    if slots is None:
        flat = _scaled_f32(flat, factor)
    else:
        flat = flat.copy().reshape(-1, size)
        flat[:, list(slots)] = _scaled_f32(flat[:, list(slots)], factor)
        flat = flat.reshape(-1)
    out = pa.FixedSizeListArray.from_arrays(
        pa.array(flat, pa.float32()), size
    )
    return out.cast(chunk.type)


def _rescale_binary_f32_chunk(chunk: pa.Array, factor: float) -> pa.Array:
    """Scale a ``Binary`` chunk whose values are packed LE f32 blobs.

    ``vertices_le`` is xyz triples; every float in the blob is a length,
    so the whole concatenated data buffer can be scaled in one numpy
    pass and the offsets reused verbatim (byte lengths do not change).
    """
    if chunk.null_count or chunk.offset:
        # Defensive: bundle() never writes nulls into vertices_le and
        # parquet chunks arrive unsliced, but a rescale that silently
        # dropped a null would be a geometry bug.
        out = []
        for blob in chunk.to_pylist():
            if blob is None:
                out.append(None)
            else:
                v = np.frombuffer(blob, dtype="<f4")
                out.append(_scaled_f32(v, factor).astype("<f4").tobytes())
        return pa.array(out, type=chunk.type)

    _, offsets_buf, data_buf = chunk.buffers()
    n = len(chunk)
    offsets = np.frombuffer(offsets_buf, dtype=np.int32, count=n + 1)
    start, end = int(offsets[0]), int(offsets[n])
    raw = np.frombuffer(data_buf, dtype=np.uint8)[start:end].copy()
    if raw.nbytes % 4:
        raise ValueError(
            "vertices_le blob is not a whole number of float32 values; "
            "refusing to rescale a malformed substrate"
        )
    scaled = _scaled_f32(raw.view("<f4"), factor).astype("<f4")
    new_offsets = (offsets - start).astype(np.int32)
    return pa.Array.from_buffers(
        chunk.type,
        n,
        [None, pa.py_buffer(new_offsets.tobytes()), pa.py_buffer(scaled.tobytes())],
        null_count=0,
    )


def _rescale_column(col: pa.ChunkedArray, fn) -> pa.ChunkedArray:
    if col.num_chunks == 0:
        return col
    return pa.chunked_array([fn(c) for c in col.chunks], col.type)


def _rescale_table(t: pa.Table, table: str, factor: float) -> pa.Table:
    """Convert every source-unit length in ``t`` by ``factor``.

    Dtypes and field metadata are preserved exactly: f32 stays f32,
    fixed-size-list shapes are unchanged, and only the columns
    enumerated in :data:`_LENGTH_FSL_COLUMNS` (plus ``transform``'s
    translation and ``vertices_le``) are touched.
    """
    for name in _LENGTH_FSL_COLUMNS[table]:
        idx = t.schema.get_field_index(name)
        if idx == -1:
            raise ValueError(
                f"{table}.parquet is missing the length column {name!r} — "
                "re-bundle this source with the current ifcfast version"
            )
        t = t.set_column(
            idx,
            t.schema.field(idx),
            _rescale_column(
                t.column(idx), lambda c: _rescale_fsl_chunk(c, factor, None)
            ),
        )
    if table == "instances":
        idx = t.schema.get_field_index("transform")
        t = t.set_column(
            idx,
            t.schema.field(idx),
            _rescale_column(
                t.column(idx),
                lambda c: _rescale_fsl_chunk(c, factor, _TRANSFORM_TRANSLATION_SLOTS),
            ),
        )
    else:
        idx = t.schema.get_field_index("vertices_le")
        t = t.set_column(
            idx,
            t.schema.field(idx),
            _rescale_column(
                t.column(idx), lambda c: _rescale_binary_f32_chunk(c, factor)
            ),
        )
    return t


def _stamp_unit_scale(t: pa.Table, raw: str) -> pa.Table:
    meta = dict(t.schema.metadata or {})
    meta[b"ifcfast.unit_scale"] = raw.encode()
    return t.replace_schema_metadata(meta)


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
        ``{"sources": [...], "unit_scale": str, "unit_scales":
        {stem: str}, "unit_factors": {stem: float}, "rep_id_offsets":
        {stem: int}, "n_instances": int, "n_representations": int,
        "guid_collisions": [...], "guid_source": {guid: stem},
        "on_collision": str, "reference_only": [...],
        "source_stats": {stem: {table: [size_bytes, mtime_ns]}}}``.

    Mixed units (GH #169): constituents may be authored in different
    length units. The federated substrate adopts the FINEST unit
    (smallest ``ifcfast.unit_scale``) and coarser sources are converted
    into it — every source-unit length column is multiplied by
    ``unit_scale_source / unit_scale_target`` (metres → millimetres is
    ×1000), QTO columns (m² / m³) are left alone. When every source
    already agrees, no rescale pass runs and the merge is bitwise
    identical to a single-unit federation.

    Raises:
        ValueError: ``out_dir`` is one of the input bundles,
            fewer than two bundles, duplicate bundle dir names,
            mismatched schemas (re-bundle all sources with the same
            ifcfast version), an unresolvable / non-positive
            ``unit_scale`` on any source, unknown ``on_collision`` /
            ``reference_only`` values, or guid collisions under
            ``on_collision="fail"``.
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
    # GH #162: `federate([a, b], a)` used to overwrite constituent a's
    # instances/representations parquets with the MERGED tables — the
    # source bundle is destroyed, and every later federation that reads
    # it silently double-counts. Checked before any write, on resolved
    # paths (`a/../a`, symlinks, and `.` all normalise here).
    resolved_out = out_dir.resolve()
    for d in bundle_dirs:
        if d.resolve() == resolved_out:
            raise ValueError(
                f"out_dir {out_dir} is one of the input bundles ({d}); "
                f"federating into a constituent would overwrite its "
                f"parquets with the merged tables. Pick a fresh directory."
            )
    out_dir.mkdir(parents=True, exist_ok=True)

    # GH #169: mixed-unit federation. Real projects mix authoring units
    # across disciplines (the buildingSMART Clinic sample is ARK/STR/EL
    # in metres, HVAC/PL in millimetres), so refusing was the wrong
    # policy — every length column is in source units and the substrate
    # records the unit, so the merge can convert. Target = the FINEST
    # constituent (smallest metres-per-unit); coarser sources scale UP.
    # Scaling up keeps f32 RELATIVE precision, where scaling everything
    # down to metres would quantise far-from-origin site coordinates at
    # centimetre level.
    unit_value: dict[str, float] = {}
    unit_raw: dict[str, str] = {}
    for d in bundle_dirs:
        seen: dict[float, str] = {}
        for table in _TABLES:
            f = d / f"{table}.parquet"
            if not f.is_file():
                raise FileNotFoundError(
                    f"{d} is not a substrate bundle: missing {f.name}"
                )
            value, raw = _unit_scale(pq.read_schema(f))
            seen.setdefault(value, raw)
        if len(seen) != 1:
            raise ValueError(
                f"{d}: instances.parquet and representations.parquet "
                f"disagree on ifcfast.unit_scale ({sorted(seen.values())}) — "
                "corrupt bundle, re-bundle the source IFC"
            )
        unit_value[d.name], unit_raw[d.name] = next(iter(seen.items()))
    target = min(unit_value.values())
    if not target > 0:
        raise ValueError(
            f"ifcfast.unit_scale must be a positive metres-per-unit factor; "
            f"got {sorted(set(unit_raw.values()))}"
        )
    target_raw = next(unit_raw[s] for s in stems if unit_value[s] == target)
    factors = {s: unit_value[s] / target for s in stems}
    rescaled = {s: f for s, f in factors.items() if f != 1.0}

    inst_parts: list[pa.Table] = []
    rep_parts: list[pa.Table] = []
    guid_source: dict[str, str] = {}
    collisions: list[str] = []
    offsets: dict[str, int] = {}
    next_offset = 0
    schemas0: dict[str, pa.Schema] = {}

    for d in bundle_dirs:
        stem = d.name
        inst = _read(d, "instances")
        rep = _read(d, "representations")
        # GH #162: BOTH tables are schema-checked. Checking only
        # `instances` let a representations mismatch through to
        # `pa.concat_tables`, which reports it as an opaque
        # "Schema at index 1 was different" with no bundle named.
        for name, t in (("instances", inst), ("representations", rep)):
            if name not in schemas0:
                schemas0[name] = t.schema
            elif t.schema != schemas0[name]:
                raise ValueError(
                    f"{d}: {name} schema differs from {bundle_dirs[0]} — "
                    "re-bundle all sources with the same ifcfast version"
                )

        # GH #169: convert this source's lengths into the target unit.
        # Untouched (identity table objects) when the source is already
        # the finest unit — the all-same-unit federation must stay
        # bitwise identical to the pre-#169 merge.
        factor = factors[stem]
        if factor != 1.0:
            inst = _rescale_table(inst, "instances", factor)
            rep = _rescale_table(rep, "representations", factor)

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
    if rescaled:
        # `concat_tables` inherits the FIRST constituent's schema
        # metadata, which after a rescale may name the wrong unit.
        # Only stamped when a rescale actually happened, so the
        # all-same-unit merge keeps its bytes byte-for-byte.
        merged_inst = _stamp_unit_scale(merged_inst, target_raw)
        merged_rep = _stamp_unit_scale(merged_rep, target_raw)
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
        # GH #169: the FEDERATED unit — the finest constituent's raw
        # metadata string, which is what the merged parquets are
        # stamped with and what downstream consumers compare against.
        # `unit_scales` / `unit_factors` record what each source was
        # authored in and the factor applied to it (1.0 = untouched).
        "unit_scale": target_raw,
        "unit_scales": {s: unit_raw[s] for s in stems},
        "unit_factors": {s: factors[s] for s in stems},
        "rep_id_offsets": offsets,
        "n_instances": merged_inst.num_rows,
        "n_representations": merged_rep.num_rows,
        "guid_collisions": sorted(set(collisions)),
        "guid_source": guid_source,
        "on_collision": on_collision,
        "reference_only": sorted(reference_only),
        # GH #162: (size, mtime_ns) per constituent parquet, so a cache
        # hit on the content-keyed federation dir can be REVALIDATED the
        # way the parse cache revalidates a manifest. The fingerprint
        # below samples head/tail 4 MB only; a same-size edit confined to
        # the middle of a >8 MB parquet keeps the key and would otherwise
        # serve a merge of bytes that no longer exist.
        "source_stats": {d.name: _bundle_stats(d) for d in bundle_dirs},
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


def _bundle_stats(d: Path) -> dict[str, list[int]]:
    """``{table: [size_bytes, mtime_ns]}`` for one constituent bundle."""
    out: dict[str, list[int]] = {}
    for table in _TABLES:
        st = (d / f"{table}.parquet").stat()
        out[table] = [st.st_size, st.st_mtime_ns]
    return out


def federation_cache_stale(sidecar: dict, bundle_dirs: list[Path]) -> bool:
    """True when a cached federation no longer describes the live bundles.

    The federation cache key is content-derived (size + head/tail 4 MB
    samples), which is deliberately blind to a plain ``touch`` — but
    equally blind to a same-size edit in the MIDDLE of a >8 MB parquet.
    The parse cache solves the identical problem by recording
    ``(size_bytes, mtime_ns)`` in its manifest and re-checking it against
    the live stat (``cache._source_matches``); this is that check for
    federations (GH #162).

    A sidecar written before ``source_stats`` existed is stale by
    definition — it carries no evidence, and the whole point is to stop
    serving merges we cannot vouch for. Cost is one re-federate.
    """
    recorded = sidecar.get("source_stats")
    if not isinstance(recorded, dict):
        return True
    if set(recorded) != {d.name for d in bundle_dirs}:
        return True
    for d in bundle_dirs:
        try:
            live = _bundle_stats(d)
        except OSError:
            return True
        # JSON round-trips the pairs as lists; normalise before compare.
        if {k: list(v) for k, v in recorded[d.name].items()} != live:
            return True
    return False


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
    # GH #169: the merge algorithm itself is part of the key — the same
    # constituent bytes federate differently before/after mixed-unit
    # rescaling, and the target unit is derived from those bytes.
    h.update(_FEDERATION_VERSION.to_bytes(4, "little"))
    # warn/fail produce identical tables; only dedup changes content.
    h.update((b"dedup" if on_collision == "dedup" else b"keep"))
    for d in bundle_dirs:
        _bundle_fingerprint(h, d)
    return cache_root() / "federated" / h.hexdigest()[:16]


__all__ = ["federate", "federation_cache_dir", "federation_cache_stale"]
