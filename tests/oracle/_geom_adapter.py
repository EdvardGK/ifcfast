"""Geometry oracle adapter — STUB, not yet implemented (GH #59 M1 / GH #58 W11).

Once GH #58 W11 lands (the pure-Rust brep path that retires the last
manifold-csg dependency from the default mesh surface), this module
will wrap ``ifcopenshell.geom.iterator`` configured with
``USE_WORLD_COORDS`` to produce per-product world-space meshes, and
diff them against ``ifcfast`` ``m.mesh_qto`` / ``m.meshes`` output
through the same :mod:`tests.oracle.normalize` /
:mod:`tests.oracle.report` machinery the quantities/psets/materials
adapters use.

Why it's deferred
-----------------
A geometry diff is only meaningful once both sides agree on the
*kernel contract*. Until W11, ifcfast's default mesh path still routes
some breps through manifold-csg, whose tessellation differs from
ifcopenshell's OCC kernel in ways that are kernel drift, not bugs —
diffing now would produce noise the M1 gate can't classify. The
intended comparison axes (deliberately loose vs the quantities gate):

- **Volume / surface area** per GUID via the lifted-quantities oracle's
  tolerance DSL, but with geometry-grade ``rel_tol`` (kernel rounding
  on tessellated volumes is far larger than authored QTO rounding).
- **AABB / centroid** per GUID — cheap, order-independent, catches
  placement-chain and rebase regressions (cf. far-origin precision).
- **Triangle / vertex counts** as a coarse structural signal only
  (kernels legitimately disagree on tessellation density, so this is a
  ``expected_drift`` candidate, not a hard gate).

Planned surface
---------------
``iter_world_meshes(ios_file, *, deflection=...) ->``
``    dict[str, WorldMesh]``
    Run a single ``ifcopenshell.geom.iterator`` pass with
    ``settings.set("use-world-coords", True)`` (never per-element
    ``create_shape`` — see CLAUDE.md geometry-extraction rule), keyed
    by GlobalId.

``diff_geometry(fast, ios, *, surface="geometry", fixture=...) ->``
``    list[GroupDiff]``
    Project both sides onto ``{guid: {metric: value}}`` (metric in
    {volume, area, aabb_*, centroid_*, tri_count, vert_count}) and reuse
    :func:`tests.oracle.normalize.diff_grouped` with a geometry-grade
    tolerance profile.

DO NOT implement until GH #58 W11 is merged. This file exists so the
adapter's shape is agreed and its imports/location are stable for the
M-series milestones.
"""

from __future__ import annotations

# Intentionally no implementation. See module docstring.
