"""Constraint-aware MEP rerouting (GH #63).

Voxelize discipline solids (and optional per-space keep-out prisms) into a
single go/nogo occupancy field, then route each requested start→goal
segment through the free voxels under its system's MEP discipline (bend
≤ 90° and gentler-is-cheaper, per-system Z discipline, stay-at-elevation
by default).

Engine vs policy — same split as :func:`ifcfast.clash`. This produces
*routes as facts*: given these obstacles and these endpoints, here is a
collision-free polyline that obeys the MEP rules. It does NOT decide
*which* clash to reroute, *which* element moves, what the clearance/
insulation envelope is, or whether the result is constructable. Those are
policy and live in the layer above.

Geometry is the caller's, in **world metres** (placement + ``unit_scale``
already baked). That is deliberate: it lets a memory-bounded extractor
feed this without ever meshing the whole model. On a large model, mesh
*one storey at a time* with ifcopenshell's geometry ``iterator`` and an
``include=`` filter — do **not** call :meth:`ifcfast.Model.meshes`, which
meshes the entire model eagerly and will OOM on a big file.

Example::

    import ifcfast

    # A wall (obstacle) and a duct that must get from one side to the
    # other. Geometry already in world metres.
    wall = (wall_vertices, wall_indices)        # closed triangle mesh
    df = ifcfast.reroute(
        obstacles=[wall],
        requests=[((0.0, 1.0, 2.7), (4.0, 1.0, 2.7), "duct")],
        cell_m=0.1,
    )
    df.loc[0, "found"]      # True
    df.loc[0, "polyline"]   # (N, 3) float32 array of world-metre points
"""

from __future__ import annotations

from typing import Iterable, Optional, Sequence, Tuple

import numpy as np
import pandas as pd

from . import _core

# Type aliases for the public signature.
Vec3 = Tuple[float, float, float]
Mesh = Tuple[Sequence[float], Sequence[int]]
Request = Tuple[Vec3, Vec3, str]
Keepout = Tuple[Sequence[Tuple[float, float]], float, Optional[float]]


def _norm_mesh(mesh: Mesh) -> Tuple[list, list]:
    """Coerce a (vertices, indices) pair to flat float / int lists.

    Accepts Python lists or numpy arrays of any shape — vertices are
    flattened to ``[x0,y0,z0, x1,…]`` and indices to a flat triangle
    list. Fails loudly on a vertex count that is not a multiple of 3.
    """
    verts, idx = mesh
    v = np.asarray(verts, dtype=np.float32).ravel()
    i = np.asarray(idx, dtype=np.uint32).ravel()
    if v.size % 3 != 0:
        raise ValueError(
            f"obstacle vertices must be a multiple of 3 (got {v.size})"
        )
    if i.size % 3 != 0:
        raise ValueError(
            f"obstacle indices must be a multiple of 3 (triangles), got {i.size}"
        )
    return v.tolist(), i.tolist()


def _norm_pt(p: Vec3) -> Tuple[float, float, float]:
    a = np.asarray(p, dtype=np.float64).ravel()
    if a.size != 3:
        raise ValueError(f"point must have 3 coordinates, got {a.size}")
    return float(a[0]), float(a[1]), float(a[2])


def reroute(
    obstacles: Iterable[Mesh],
    requests: Iterable[Request],
    *,
    clearance_m: float | Sequence[float] = 0.0,
    bounds: Optional[Tuple[Vec3, Vec3]] = None,
    cell_m: float = 0.1,
    keepouts: Optional[Iterable[Keepout]] = None,
    default_keepout_m: float = 2.4,
    snap_voxels: int = 2,
) -> pd.DataFrame:
    """Route MEP segments around obstacles through a voxel go/nogo field.

    Args:
        obstacles: closed discipline solids — an iterable of
            ``(vertices, indices)``. ``vertices`` is a flat or ``(N, 3)``
            array of world-metre coordinates; ``indices`` a flat or
            ``(M, 3)`` triangle list. numpy arrays or Python lists both
            work.
        requests: the segments to route — an iterable of
            ``(start_xyz, goal_xyz, system)``. ``system`` is a free-form
            string matched case-insensitively: ``"pressure"`` →
            pressurised pipe (stays planar), ``"drain"`` / ``"gravity"``
            → gravity drainage (monotone descent), ``"duct"`` /
            ``"vent"`` → ducting (level changes penalised), anything else
            → ``"unknown"`` (stay at one elevation). IFC values like
            ``"GravityDrainage"`` map correctly.
        clearance_m: room the routed object's centreline keeps from every
            obstacle, in metres — i.e. ``object_radius + insulation +
            safety``. For a round duct of diameter ``D``, pass ``D / 2``
            (plus insulation). This is the C-space inflation: the path is
            feasible for the real cross-section, not an infinitely-thin
            line. A scalar applies to all requests; a sequence gives one
            clearance per request (so a thin pipe and a fat duct route
            through the *same* occupancy build at their own sizes). ``0.0``
            (default) routes a bare centreline. The clearance is evaluated
            against a prebuilt distance field, so varying it costs nothing
            extra — no obstacle dilation or rebuild.
        bounds: ``(min_xyz, max_xyz)`` world-metre AABB of the voxel grid.
            ``None`` (default) auto-fits the grid to the obstacles,
            requests, and keepouts with a small pad.
        cell_m: voxel edge length in metres (default ``0.1``). Drives
            resolution and memory.
        keepouts: optional per-space keep-out prisms — an iterable of
            ``(footprint, floor_z_m, height_m)``. ``footprint`` is a
            world-metre XY ring ``[(x, y), …]``; the prism is swept from
            ``floor_z_m`` up by ``height_m`` (``None`` →
            ``default_keepout_m``) and marked nogo, so routes stay out of
            the occupiable volume but the plenum above stays free.
        default_keepout_m: keep-out height when a keepout omits its own
            (default ``2.4``).
        snap_voxels: when a start/goal lands on a nogo voxel, search this
            Chebyshev radius (in voxels) for the nearest free voxel and
            route from there (default ``2``; ``0`` disables). The element
            being rerouted commonly has endpoints inside its own keep-out
            — without a snap every such request would report no route.

    Returns:
        ``pandas.DataFrame``, one row per request, columns:

        * ``found`` (``bool``) — whether a route was found.
        * ``system`` (``object``) — the resolved system key
          (``"pressure"`` / ``"drain"`` / ``"duct"`` / ``"unknown"``).
        * ``clearance_m`` (``float32``) — the clearance this request was
          routed at.
        * ``length_m`` (``float32``) — geometric route length
          (un-penalised), ``0.0`` when not found.
        * ``bends`` (``int``) — number of direction changes.
        * ``max_bend_deg`` (``float32``) — largest bend angle (≤ 90).
        * ``start_snapped`` / ``goal_snapped`` (``bool``) — whether the
          endpoint had to be snapped off a nogo voxel.
        * ``polyline`` (``object``) — the route as an ``(N, 3)``
          ``float32`` numpy array of world-metre points (voxel centres),
          empty ``(0, 3)`` when not found.

        ``df.attrs`` carries the run metadata: ``grid_dims``,
        ``grid_origin``, ``cell_m``, ``free_voxels``, ``occupied_voxels``,
        ``request_count``, ``occupancy_ms``, ``route_ms``.
    """
    obs = [_norm_mesh(m) for m in obstacles]
    reqs = [(_norm_pt(s), _norm_pt(g), str(sys)) for (s, g, sys) in requests]

    # Clearance: scalar broadcasts, sequence is one-per-request. The Rust
    # side re-validates the length; we normalise to a flat float list here.
    if np.isscalar(clearance_m):
        clear_list = [float(clearance_m)] * len(reqs)
    else:
        clear_list = [float(c) for c in clearance_m]  # type: ignore[union-attr]
        if len(clear_list) not in (1, len(reqs)):
            raise ValueError(
                f"clearance_m sequence must have length 1 or {len(reqs)} "
                f"(one per request), got {len(clear_list)}"
            )

    keep_norm: list = []
    for fp, fz, h in keepouts or []:
        pts = [(float(x), float(y)) for (x, y) in fp]
        keep_norm.append((pts, float(fz), None if h is None else float(h)))

    bmin = None
    bmax = None
    if bounds is not None:
        bmin = _norm_pt(bounds[0])
        bmax = _norm_pt(bounds[1])

    d = _core.reroute(
        obs,
        reqs,
        bmin,
        bmax,
        float(cell_m),
        keep_norm,
        float(default_keepout_m),
        int(snap_voxels),
        clear_list,
    )

    polylines = [
        np.asarray(p, dtype=np.float32).reshape(-1, 3) for p in d["polyline"]
    ]
    df = pd.DataFrame(
        {
            "found": d["found"],
            "system": d["system"],
            "clearance_m": d["clearance_m"],
            "length_m": d["length_m"],
            "bends": d["bends"],
            "max_bend_deg": d["max_bend_deg"],
            "start_snapped": d["start_snapped"],
            "goal_snapped": d["goal_snapped"],
            "polyline": polylines,
        }
    )
    df.attrs["grid_dims"] = tuple(int(x) for x in d["grid_dims"])
    df.attrs["grid_origin"] = tuple(float(x) for x in d["grid_origin"])
    df.attrs["cell_m"] = float(d["cell_m"])
    df.attrs["free_voxels"] = int(d["free_voxels"])
    df.attrs["occupied_voxels"] = int(d["occupied_voxels"])
    df.attrs["request_count"] = int(d["request_count"])
    df.attrs["occupancy_ms"] = float(d["occupancy_ms"])
    df.attrs["route_ms"] = float(d["route_ms"])
    return df


__all__ = ["reroute"]
