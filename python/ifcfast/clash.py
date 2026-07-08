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

    # Cross-discipline: pass a LIST of bundles — they are federated
    # (ifcfast.federate) into a content-keyed cache dir and clashed as
    # one substrate. source_model_a/b attribute each side.
    df = ifcfast.clash(["ark.bundle/", "rib.bundle/"])
    cross = df[df.source_model_a != df.source_model_b]

Engine vs policy: this is the engine layer. It produces per-pair
geometric facts ("do they intersect, by how much, how far apart"). It
does NOT do connectivity dismissal (wall-meets-slab is normally not a
real clash), space attribution, discipline routing, or BCF emit. Those
are policy and live in the layer above — agents query ``clashes.parquet``
joined to ``instances.parquet`` to apply them.
"""

from __future__ import annotations

import json
import os
import shutil
from os import PathLike
from pathlib import Path
from typing import Iterable, Literal, Sequence

import pandas as pd

from . import _core


def clash(
    bundle_dir: str | PathLike[str] | Sequence[str | PathLike[str]],
    *,
    tolerance_m: float = 0.0,
    write_parquet: bool = True,
    include_classes: Iterable[str] | None = None,
    exclude_self_class: Iterable[str] | None = None,
    reference_only: Iterable[str] = (),
    on_collision: Literal["warn", "fail", "dedup"] = "warn",
) -> pd.DataFrame:
    """Run clash detection against a bundle (or a federation of bundles).

    Args:
        bundle_dir: directory containing ``instances.parquet`` and
            ``representations.parquet`` (the output of
            ``ifcfast-bundle``) — or a LIST of such directories. A
            list is federated via :func:`ifcfast.federate` into a
            content-keyed dir under the ifcfast cache
            (``cache_root()/federated/<key>``, reused while the
            constituent parquets are unchanged) and clashed as one
            substrate. A single-element list behaves exactly like
            passing that directory.
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
        reference_only: ``source_model`` names treated as pure
            reference geometry — pairs where BOTH sides come from a
            reference model are dropped before narrow-phase (reference
            models clash against active models, never among
            themselves). Names are the constituent bundle dir names
            for a federation, or the source IFC's file stem for a
            single bundle. Enforced engine-side so the parquet and the
            DataFrame agree, and so one cached federation serves every
            reference-set choice.
        on_collision: guid-collision policy handed to
            :func:`ifcfast.federate` when ``bundle_dir`` is a list of
            two or more dirs (``"warn"`` / ``"fail"`` / ``"dedup"``).
            Rejected otherwise — there is nothing to federate.

    Returns:
        ``pandas.DataFrame`` with columns:

        * ``ifc_id_a`` / ``ifc_id_b`` (``uint64``) — STEP entity ids
          of the two instances. Always ``ifc_id_a < ifc_id_b`` is NOT
          guaranteed; ordering follows broad-phase pair emission.
        * ``guid_a`` / ``guid_b`` (``object``) — IFC GUIDs.
        * ``class_a`` / ``class_b`` (``object``) — normalised classes.
        * ``source_model_a`` / ``source_model_b`` (``object``) — each
          side's substrate ``source_model`` (empty string on bundles
          written before cache schema v29). On a federated substrate
          ``ifc_id`` / ``guid`` may collide across constituent models:
          join back to ``instances.parquet`` on
          ``(ifc_id, source_model)`` / ``(guid, source_model)``, and
          split cross-model pairs with
          ``df.source_model_a != df.source_model_b``.
        * ``kind`` (``object``) — ``"hard"`` for intersecting meshes
          (zero minimum distance), ``"clearance"`` for pairs within the
          tolerance band.
        * ``category`` (``object``) — semantic bucket assigned from
          the substrate classes alone. One of ``"clash"`` (actionable
          cross-system overlap — the default), ``"insulation"``
          (either side is ``Covering``), ``"connection"`` (same-family
          ``XFitting``/``XSegment`` MEP joint — fittings meeting their
          own run), or ``"non_physical"`` (either side is
          ``Grid``/``Annotation``/``Space``/``OpeningElement``/
          ``VirtualElement``). Engine *categorises*, never drops —
          filter with e.g. ``df[df.category == "clash"]`` to triage
          a noisy MEP run. See GH #49.
        * ``min_distance_m`` (``float32``) — minimum mesh-to-mesh
          distance in metres. ``0.0`` for hard clashes.

        The DataFrame also carries the run's metadata on
        ``df.attrs``: ``geometryless_skipped``, ``narrow_phase_residuals``,
        ``pair_count``, ``tolerance_m``, ``clash_ms``, and (when
        ``write_parquet=True``) ``clashes_parquet`` — the absolute path
        of the written file. For a federated run, additionally
        ``federated_dir`` (the cache dir holding the merged substrate;
        ``clashes.parquet`` is written there) and ``federation`` (the
        :func:`ifcfast.federate` sidecar dict).
    """
    if isinstance(reference_only, (str, bytes)):
        # tuple("ark") == ('a', 'r', 'k') — a bare string would silently
        # match nothing and disable the reference filter.
        raise TypeError(
            "reference_only must be an iterable of source_model names, "
            "not a bare string — pass ('name',) or ['name']"
        )
    reference_only = tuple(reference_only)

    federation_sidecar = None
    if isinstance(bundle_dir, (str, PathLike)):
        dirs = None
    elif isinstance(bundle_dir, Sequence):
        dirs = [Path(d) for d in bundle_dir]
        if not dirs:
            raise ValueError("clash() got an empty list of bundle dirs")
    else:
        raise TypeError(
            f"bundle_dir must be a path or a sequence of paths, "
            f"got {type(bundle_dir).__name__}"
        )

    if dirs is not None and len(dirs) >= 2:
        from .federate import federate, federation_cache_dir

        fed_dir = federation_cache_dir(dirs, on_collision)
        sidecar_path = fed_dir / "federation.json"
        if sidecar_path.is_file():
            # Cache hit — the key is content-derived, so the merge is
            # current. Re-apply the collision policy from the recorded
            # facts (a "fail" run must still fail on a dir cached by an
            # earlier "warn"-equivalent merge).
            federation_sidecar = json.loads(sidecar_path.read_text())
            if on_collision == "fail" and federation_sidecar["guid_collisions"]:
                n = len(federation_sidecar["guid_collisions"])
                raise ValueError(
                    f"{n} guid(s) appear in more than one source "
                    f"bundle; see {sidecar_path}"
                )
        else:
            # Federate into a private temp dir and publish by rename so
            # parallel processes missing the cache simultaneously never
            # interleave writes into the shared key dir.
            tmp = fed_dir.parent / f"{fed_dir.name}.tmp-{os.getpid()}"
            try:
                federation_sidecar = federate(
                    dirs, tmp, on_collision=on_collision
                )
                os.replace(tmp, fed_dir)
            except OSError:
                if not sidecar_path.is_file():
                    raise
                # Lost a concurrent-publish race — the same content key
                # was federated by another process. Adopt its merge.
                federation_sidecar = json.loads(sidecar_path.read_text())
            finally:
                shutil.rmtree(tmp, ignore_errors=True)
        bundle_dir = fed_dir
    else:
        if dirs is not None:
            bundle_dir = dirs[0]
        if on_collision != "warn":
            raise ValueError(
                "on_collision only applies when federating a list of "
                "two or more bundles; got a single bundle dir"
            )

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

    if reference_only:
        # A name that matches nothing silently disables the reference
        # filter — the dangerous direction for a clash tool is a quiet
        # false-negative, so validate against the substrate's actual
        # source_model values and fail loudly instead.
        import pyarrow.parquet as pq

        if "source_model" not in pq.read_schema(inst_path).names:
            raise ValueError(
                f"{bundle_dir} predates the source_model column "
                f"(cache schema v29) — re-bundle before using "
                f"reference_only"
            )
        available = set(
            pq.read_table(inst_path, columns=["source_model"])
            .column("source_model")
            .to_pylist()
        )
        unknown = sorted(set(reference_only) - available)
        if unknown:
            raise ValueError(
                f"reference_only names {unknown} are not among this "
                f"substrate's source_model values {sorted(available)}"
            )

    d = _core.clash(
        str(bundle_dir),
        float(tolerance_m),
        bool(write_parquet),
        list(include_classes or []),
        list(exclude_self_class or []),
        list(reference_only),
    )
    df = pd.DataFrame(
        {
            "ifc_id_a": d["ifc_id_a"],
            "ifc_id_b": d["ifc_id_b"],
            "guid_a": d["guid_a"],
            "guid_b": d["guid_b"],
            "class_a": d["class_a"],
            "class_b": d["class_b"],
            "source_model_a": d["source_model_a"],
            "source_model_b": d["source_model_b"],
            "kind": d["kind"],
            "category": d["category"],
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
    if federation_sidecar is not None:
        df.attrs["federated_dir"] = str(bundle_dir)
        df.attrs["federation"] = federation_sidecar
    return df


__all__ = ["clash"]
