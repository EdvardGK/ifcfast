"""Geometry-QTO differential oracle adapter (GH #59 M1 / GH #58).

The automated version of the G55 hand-differential that caught a volume
regression: per-element solid **volume** from ifcfast's ``mesh_qto`` is
diffed against ifcopenshell's OCC-kernel mesh volume, keyed by
``GlobalId``. A divergence past a geometry-grade tolerance is emitted as
an :class:`tests.oracle.report.DisagreementRecord` left ``unknown`` —
a human triages ``ifcfast_bug`` vs ``ifcopenshell_quirk`` afterwards.

Why volume only (for now)
-------------------------
Volume is the one geometry metric where both kernels agree on the
*contract* even when their tessellation density differs: the signed-tetra
divergence integral is exact for any closed triangulation of the same
solid, so two different meshings of one box both integrate to its true
volume. AABB / centroid / triangle-count axes (sketched in the original
stub) are far more tessellation-sensitive and are deferred — this module
is the volume gate the G55 differential actually exercised by hand.

Kernel contract notes
---------------------
- **DEFAULT ifcopenshell settings** — openings are applied (matching
  ``mesh_qto(cut_openings=True)``). We deliberately do **not** set
  ``use-world-coords``: it segfaults on this corpus, and volume is
  placement-invariant anyway, so local coordinates are correct here.
- A single ``ifcopenshell.geom.iterator`` pass (never per-element
  ``create_shape`` — see CLAUDE.md geometry-extraction rule).
- Per-element ``try/except`` on both sides: the OCC kernel can throw on
  individual products, and a single bad element must not abort the sweep.

Public surface
--------------
``diff_geometry_volumes(ifc_path, *, rel_tol=1e-2, abs_tol=1e-3,``
``    surface="geometry") -> list[report.DisagreementRecord]``
    Open ``ifc_path`` in both libraries, project each onto
    ``{guid: volume_m3}``, diff with abs+rel tolerance via
    :mod:`tests.oracle.normalize`, and return one
    :class:`~tests.oracle.report.DisagreementRecord` per disagreeing
    element (classification ``unknown``). Empty list == full agreement.
    Point it at ``G55_RIB.ifc`` by hand to reproduce the differential.

``ios_volumes_by_guid(ifc_path) -> dict[str, float]``
    The ifcopenshell side in isolation (single iterator pass).

``fast_volumes_by_guid(ifc_path) -> dict[str, float]``
    The ifcfast side in isolation (``mesh_qto(cut_openings=True)``).
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

from . import normalize as nz
from .report import Classification, DisagreementRecord

if TYPE_CHECKING:  # pragma: no cover - typing only
    from os import PathLike


# Geometry-grade tolerance: kernel rounding on tessellated mesh volumes is
# far larger than authored-QTO rounding, so this is deliberately looser than
# normalize's DEFAULT_REL_TOL. ~1% relative absorbs tessellation/kernel drift
# while still catching a dropped factor or a unit-scale slip.
GEOM_REL_TOL: float = 1e-2
GEOM_ABS_TOL: float = 1e-3


def _signed_mesh_volume(verts, faces) -> float:
    """Closed-mesh volume via the signed-tetra divergence sum, in float64.

    ``verts`` is a flat ``[x0,y0,z0, x1,y1,z1, ...]`` sequence and
    ``faces`` a flat triangle-index sequence, exactly as ifcopenshell's
    ``shape.geometry.verts`` / ``.faces`` expose them. The absolute value
    of ``sum( (a × b) · c ) / 6`` over every triangle ``(a, b, c)`` is the
    enclosed volume for any consistently-oriented closed triangulation;
    ``abs`` makes the result orientation-independent.

    Pure float64 accumulation (no numpy dependency required at import) so
    the adapter stays importable in a minimal oracle environment.
    """
    v = list(verts)
    total = 0.0
    it = iter(faces)
    for i, j, k in zip(it, it, it):
        ax, ay, az = v[3 * i], v[3 * i + 1], v[3 * i + 2]
        bx, by, bz = v[3 * j], v[3 * j + 1], v[3 * j + 2]
        cx, cy, cz = v[3 * k], v[3 * k + 1], v[3 * k + 2]
        # (a × b) · c
        crx = ay * bz - az * by
        cry = az * bx - ax * bz
        crz = ax * by - ay * bx
        total += crx * cx + cry * cy + crz * cz
    return abs(total) / 6.0


def ios_volumes_by_guid(ifc_path: "str | PathLike[str]") -> dict[str, float]:
    """Per-GlobalId mesh volume from ifcopenshell, single iterator pass.

    DEFAULT settings (openings applied). ``ifcopenshell`` is imported
    *inside* the function so ``tests/oracle`` stays cleanly skippable when
    the dev-only extra is absent (mirrors the sibling adapters). Each
    element is guarded with ``try/except`` because the OCC kernel can
    throw per product.
    """
    import ifcopenshell
    import ifcopenshell.geom

    fil = ifcopenshell.open(str(ifc_path))
    settings = ifcopenshell.geom.settings()  # DEFAULT — do NOT set use-world-coords
    iterator = ifcopenshell.geom.iterator(settings, fil)

    out: dict[str, float] = {}
    if not iterator.initialize():
        return out
    while True:
        shape = iterator.get()
        try:
            geom = shape.geometry
            out[shape.guid] = _signed_mesh_volume(geom.verts, geom.faces)
        except Exception:
            # kernel can throw on a single element — skip it, keep sweeping.
            pass
        if not iterator.next():
            break
    return out


def fast_volumes_by_guid(ifc_path: "str | PathLike[str]") -> dict[str, float]:
    """Per-guid solid volume from ifcfast ``mesh_qto(cut_openings=True)``.

    Reads the ``volume_m3`` column of the mesh-QTO table (``mesh_qto``
    returns a tuple; ``[0]`` is the per-element DataFrame). ``ifcfast`` is
    imported inside the function for symmetry with the ifcopenshell side.
    """
    import ifcfast

    model = ifcfast.open(str(ifc_path), use_cache=False, write_cache=False)
    df = model.mesh_qto(cut_openings=True)[0]
    out: dict[str, float] = {}
    for guid, vol in zip(df["guid"], df["volume_m3"]):
        if vol is None:
            continue
        out[guid] = float(vol)
    return out


def diff_geometry_volumes(
    ifc_path: "str | PathLike[str]",
    *,
    rel_tol: float = GEOM_REL_TOL,
    abs_tol: float = GEOM_ABS_TOL,
    surface: str = "geometry",
) -> list[DisagreementRecord]:
    """Diff per-element solid volume: ifcfast vs ifcopenshell.

    Returns one :class:`~tests.oracle.report.DisagreementRecord` per
    GlobalId where the two kernels disagree past ``abs+rel`` tolerance —
    including a guid present in only one side. Every record is
    ``Classification.unknown`` (hence blocking) so a human labels it
    ``ifcfast_bug`` vs ``ifcopenshell_quirk`` during triage. Empty list
    means the two kernels agree on every element.

    Reuses :func:`tests.oracle.normalize.floats_close` for the scalar
    compare so the geometry gate shares the project's float-compare
    semantics. ``fixture`` on each record is the file's basename.
    """
    fixture = Path(ifc_path).name
    fast = fast_volumes_by_guid(ifc_path)
    truth = ios_volumes_by_guid(ifc_path)

    records: list[DisagreementRecord] = []
    for guid in sorted(set(fast) | set(truth)):
        ours = fast.get(guid)
        theirs = truth.get(guid)

        if ours is None:
            records.append(
                DisagreementRecord(
                    surface=surface,
                    fixture=fixture,
                    guid=guid,
                    group="volume_m3",
                    kind="missing_in_ours",
                    detail=(
                        "ifcopenshell meshed this element; ifcfast mesh_qto "
                        f"has no volume (truth={theirs!r})"
                    ),
                    classification=Classification.unknown,
                    ours=None,
                    truth=theirs,
                    diverging_keys=("volume_m3",),
                )
            )
            continue
        if theirs is None:
            records.append(
                DisagreementRecord(
                    surface=surface,
                    fixture=fixture,
                    guid=guid,
                    group="volume_m3",
                    kind="missing_in_truth",
                    detail=(
                        "ifcfast surfaced a volume; ifcopenshell did not mesh "
                        f"this element (ours={ours!r})"
                    ),
                    classification=Classification.unknown,
                    ours=ours,
                    truth=None,
                    diverging_keys=("volume_m3",),
                )
            )
            continue

        if not nz.floats_close(ours, theirs, rel_tol=rel_tol, abs_tol=abs_tol):
            rel = abs(ours - theirs) / max(abs(theirs), 1e-12)
            records.append(
                DisagreementRecord(
                    surface=surface,
                    fixture=fixture,
                    guid=guid,
                    group="volume_m3",
                    kind="value_mismatch",
                    detail=(
                        f"volume_m3: ours={ours!r} truth={theirs!r} "
                        f"(rel={rel:.4g}, rel_tol={rel_tol}, abs_tol={abs_tol})"
                    ),
                    classification=Classification.unknown,
                    ours=ours,
                    truth=theirs,
                    diverging_keys=("volume_m3",),
                )
            )

    return records
