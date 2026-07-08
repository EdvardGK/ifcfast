"""Schema-exact federation of ifcfast substrate bundles (the GH #50 hand-merge).

``ifcfast.clash()`` takes ONE bundle dir; cross-discipline clash therefore
needs the bundles merged. GH #50 documented the two sharp edges of doing
this by hand, and this module is that recipe done right:

1. ``rep_id`` is numbered per bundle — every source after the first gets
   its rep_ids offset by the running maximum, in BOTH ``instances`` and
   ``representations``, so geometry never cross-links.
2. The Rust substrate reader is strict on arrow types — tables are merged
   with pyarrow preserving the EXACT source schema (a pandas round-trip
   silently widens ``string`` to ``large_string`` and gets rejected).

Additional guards:

- ``ifcfast.unit_scale`` metadata must agree across sources; vertex/bbox
  columns are in source units and the clash engine applies one scale per
  bundle, so merging mixed units would silently misplace geometry. Fail
  loudly instead.
- guid collisions across sources are counted and reported (same element
  exported into two models would self-clash as noise).

``ifc_id`` (STEP entity id) is NOT remapped: it is per-source and only
used for reporting; ``guid`` is the cross-source key. The returned sidecar
maps guid -> source stem so callers can split intra- from cross-model
pairs. #50 has shipped first-class federation (``ifcfast.federate``);
this module stays frozen as the differential spec —
``tests/test_federate_parity.py`` gates table-level equality between
the two. Only post-#50 change: instance rows are re-stamped with
``source_model`` = bundle dir name (cache schema v29), exactly as the
product does, so the schemas stay comparable.
"""

from __future__ import annotations

import json
from pathlib import Path

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

_TABLES = ("instances", "representations")


def _read(bundle_dir: Path, table: str) -> pa.Table:
    f = bundle_dir / f"{table}.parquet"
    if not f.is_file():
        raise FileNotFoundError(f"{bundle_dir} is not a substrate bundle: missing {f.name}")
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
    """Re-stamp ``source_model`` = bundle dir name (cache schema v29),
    mirroring ``ifcfast.federate``. Pre-v29 bundles lack the column —
    append it at the end, where the v29 writer puts it."""
    col = pa.array([stem] * t.num_rows, pa.string())
    idx = t.schema.get_field_index("source_model")
    if idx == -1:
        return t.append_column(
            pa.field("source_model", pa.string(), nullable=False), col
        )
    return t.set_column(idx, t.schema.field(idx), col)


def federate_bundles(bundle_dirs: list[Path | str], out_dir: Path | str) -> dict:
    """Merge N substrate bundles into one clash-able bundle at ``out_dir``.

    Returns a sidecar dict (also written to ``out_dir/federation.json``):
    ``{"sources": [...], "guid_source": {guid: stem}, "guid_collisions":
    [...], "rep_id_offsets": {stem: int}}``.
    """
    bundle_dirs = [Path(d) for d in bundle_dirs]
    if len(bundle_dirs) < 2:
        raise ValueError("federation needs at least two bundles")
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
        offsets[stem] = next_offset
        inst_parts.append(_stamp_source_model(_offset_rep_id(inst, next_offset), stem))
        rep_parts.append(_offset_rep_id(rep, next_offset))
        max_rep = pc.max(rep.column("rep_id")).as_py()
        next_offset += (max_rep if max_rep is not None else 0) + 1

        for g in inst.column("guid").to_pylist():
            if g in guid_source:
                collisions.append(g)
            else:
                guid_source[g] = stem

    if len(scales) != 1:
        raise ValueError(
            f"unit_scale differs across bundles ({sorted(scales)}); "
            "cannot federate mixed-unit substrates"
        )

    merged_inst = pa.concat_tables(inst_parts)
    merged_rep = pa.concat_tables(rep_parts)
    pq.write_table(merged_inst, out_dir / "instances.parquet")
    pq.write_table(merged_rep, out_dir / "representations.parquet")

    sidecar = {
        "sources": [str(d) for d in bundle_dirs],
        "unit_scale": next(iter(scales)),
        "rep_id_offsets": offsets,
        "n_instances": merged_inst.num_rows,
        "n_representations": merged_rep.num_rows,
        "guid_collisions": sorted(set(collisions)),
        "guid_source": guid_source,
    }
    (out_dir / "federation.json").write_text(json.dumps(sidecar, indent=1))
    return sidecar
