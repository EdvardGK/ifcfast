"""Substrate-aware clash detection.

Runs broad-phase AABB overlap plus narrow-phase mesh-mesh intersection
against a bundle (the parquet substrate emitted by ``ifcfast-bundle``).
Writes ``clashes.parquet`` next to ``instances.parquet`` and returns a
:class:`pandas.DataFrame` of the same rows.

Why substrate-driven rather than IFC-driven: the bundle already carries
the per-instance world-coord AABBs, the rep_id foreign keys, and the
triangle buffers (in ``representations.parquet``). Running clash from
the parquet skips a second parse of the source IFC and produces an
output keyed in the same row coordinates agents are already querying
for types / quantities / materials. Join ``clashes.parquet`` back to
``instances.parquet`` on ``ifc_id_a`` / ``ifc_id_b`` (or ``guid_*``)
to enrich with storey, type, or pset.

Example::

    import ifcfast
    df = ifcfast.clash("path/to/model.bundle/")
    df.head()
    #    ifc_id_a  ifc_id_b           guid_a           guid_b class_a class_b   kind  min_distance_m
    # 0      1234      5678  3Wall000000…001  4Pipe000000…002    Wall    Pipe   hard             0.0
    # 1      1235      5679  3Wall000000…003  4Pipe000000…004    Wall    Pipe   hard             0.0

Engine vs policy: this is the engine layer. It produces per-pair
geometric facts ("do they intersect, by how much, how far apart"). It
does NOT do connectivity dismissal (wall-meets-slab is normally not a
real clash), space attribution, discipline routing, or BCF emit. Those
are policy and live in the layer above — agents query ``clashes.parquet``
joined to ``instances.parquet`` to apply them.
"""

from __future__ import annotations

from os import PathLike
from pathlib import Path
from typing import Iterable

import pandas as pd

from . import _core


def clash(
    bundle_dir: str | PathLike[str],
    *,
    tolerance_m: float = 0.0,
    write_parquet: bool = True,
    include_classes: Iterable[str] | None = None,
    exclude_self_class: Iterable[str] | None = None,
) -> pd.DataFrame:
    """Run clash detection against a bundle.

    Args:
        bundle_dir: directory containing ``instances.parquet`` and
            ``representations.parquet`` (the output of
            ``ifcfast-bundle``).
        tolerance_m: clearance band, in metres. ``0.0`` (default) means
            "hard clashes only" — pairs whose meshes actually intersect.
            A positive value also emits ``kind="clearance"`` rows for
            pairs whose minimum mesh-to-mesh distance is ``<= tolerance_m``.
        write_parquet: when ``True`` (default), also writes
            ``clashes.parquet`` inside ``bundle_dir``. The DataFrame
            return value is identical to the parquet's contents — set
            this to ``False`` if you only want the in-memory frame.
        include_classes: if given, only emit pairs where at least one
            side's normalised ``class`` is in the set (e.g.
            ``{"Pipe", "Duct"}``). The substrate's ``class`` column
            is normalised — pass ``"Pipe"``, not ``"IfcPipe"``.
        exclude_self_class: classes that should never clash against
            themselves (e.g. ``{"Wall"}`` to suppress wall-vs-wall
            noise when you only care about cross-discipline clashes).

    Returns:
        ``pandas.DataFrame`` with columns:

        * ``ifc_id_a`` / ``ifc_id_b`` (``uint64``) — STEP entity ids
          of the two instances. Always ``ifc_id_a < ifc_id_b`` is NOT
          guaranteed; ordering follows broad-phase pair emission.
        * ``guid_a`` / ``guid_b`` (``object``) — IFC GUIDs.
        * ``class_a`` / ``class_b`` (``object``) — normalised classes.
        * ``kind`` (``object``) — ``"hard"`` for intersecting meshes
          (zero minimum distance), ``"clearance"`` for pairs within the
          tolerance band.
        * ``min_distance_m`` (``float32``) — minimum mesh-to-mesh
          distance in metres. ``0.0`` for hard clashes.

        The DataFrame also carries the run's metadata on
        ``df.attrs``: ``geometryless_skipped``, ``narrow_phase_residuals``,
        ``pair_count``, ``tolerance_m``, ``clash_ms``, and (when
        ``write_parquet=True``) ``clashes_parquet`` — the absolute path
        of the written file.
    """
    bundle_dir = Path(bundle_dir)
    if not bundle_dir.is_dir():
        raise FileNotFoundError(f"bundle directory not found: {bundle_dir}")

    inst_path = bundle_dir / "instances.parquet"
    rep_path = bundle_dir / "representations.parquet"
    if not inst_path.exists() or not rep_path.exists():
        missing = ", ".join(
            p.name for p in (inst_path, rep_path) if not p.exists()
        )
        raise FileNotFoundError(
            f"{bundle_dir} is missing substrate file(s): {missing}. "
            f"Run `ifcfast-bundle` against your IFC first."
        )

    d = _core.clash(
        str(bundle_dir),
        float(tolerance_m),
        bool(write_parquet),
        list(include_classes or []),
        list(exclude_self_class or []),
    )
    df = pd.DataFrame(
        {
            "ifc_id_a": d["ifc_id_a"],
            "ifc_id_b": d["ifc_id_b"],
            "guid_a": d["guid_a"],
            "guid_b": d["guid_b"],
            "class_a": d["class_a"],
            "class_b": d["class_b"],
            "kind": d["kind"],
            "min_distance_m": d["min_distance_m"],
        }
    )
    df.attrs["geometryless_skipped"] = int(d["geometryless_skipped"])
    df.attrs["narrow_phase_residuals"] = int(d["narrow_phase_residuals"])
    df.attrs["pair_count"] = int(d["pair_count"])
    df.attrs["tolerance_m"] = float(d["tolerance_m"])
    df.attrs["clash_ms"] = float(d["clash_ms"])
    if "clashes_parquet" in d:
        df.attrs["clashes_parquet"] = d["clashes_parquet"]
    return df


__all__ = ["clash"]
