"""ifcfast — the agent-first IFC parser.

A native (Rust) IFC parser with a Python API: reads IFC data and
geometry into pandas DataFrames, triangle meshes, and point clouds —
no geometry kernel on the hot path. A spatial-relationship graph,
parquet cache, and a stable, introspectable API. Designed for AI
agents, RPA, and analytics pipelines that need to ask questions of an
IFC without loading a heavy geometry kernel.

Early and under active development — not yet verified against
established tools. Cross-check output against ``ifcopenshell`` before
relying on it. Complements ``ifcopenshell`` rather than replacing it.

Quick start::

    import ifcfast

    # Bundled demo file works without any external IFC.
    m = ifcfast.open(ifcfast.example_path())
    m.summary()          # dict — schema, counts, available tables, samples
    m.schemas            # dict — column-level introspection of every table

    # Or open your own.
    m = ifcfast.open("model.ifc")
    print(len(m), "products,", len(m.storeys), "storeys")
    print(m.types())     # {'IfcWall': 1234, 'IfcDoor': 45, ...}

    # Long-format data layers (pandas DataFrames, lazy).
    m.psets             # property sets
    m.quantities        # base quantities
    m.materials         # material assignments
    m.classifications   # classification references
    m.drift             # placement-vs-mesh drift report

    # Relationship graph (3 DataFrames + 7 helpers).
    m.contained_in        # product → storey edges
    m.aggregates          # child → parent edges (with parent_kind)
    m.storey_building     # storey → building edges
    m.parent(g); m.children(g); m.ancestors(g); m.descendants(g)
    m.storey_of(g); m.building_of(g); m.products_in(parent_g)

CLI (all subcommands take ``--json``)::

    ifcfast demo                  # showcase against bundled IFC
    ifcfast index   FILE          # tier-1 parse + counts
    ifcfast schema  FILE          # full schema introspection (JSON)
    ifcfast extract FILE          # extract data layers
    ifcfast drift   FILE          # placement-vs-mesh drift report
    ifcfast cache   FILE          # inspect / clear cache

Agents: paste :func:`ifcfast.system_prompt` output into your system
prompt to ramp instantly. See ``AGENTS.md`` for the full agent guide.

Public API:

* :func:`open` — open and index an IFC file (returns :class:`Model`).
* :func:`header` — parse only the STEP header (no full index).
* :func:`example_path` — path to the bundled minimal IFC fixture.
* :func:`system_prompt` — paste-into-agent description of the library.
* :class:`Model` — parsed index, lazy data layers, spatial-graph helpers.
* :class:`ProductRow`, :class:`StoreyRow` — row dataclasses.
* :mod:`ifcfast.classify` — element-mode policy.
* :mod:`ifcfast.cache` — parquet cache layer.
"""

from __future__ import annotations

from pathlib import Path as _Path

from .header import IFCHeader, header
from .model import Model, ProductRow, StoreyRow, open_ifc as open
from .bundle import bundle
from .clash import clash
from . import cache, classify

# Re-export the Rust-side IfcfastError so callers can
# `from ifcfast import IfcfastError` without reaching into `_core`.
# The native module is only present in built wheels — fall back to a
# Python-defined placeholder during source-only imports (tooling, type
# checkers) so the import itself doesn't fail.
try:
    from ._core import IfcfastError  # type: ignore[attr-defined]
except ImportError:  # pragma: no cover
    class IfcfastError(Exception):
        """Raised when the Rust core hits a recoverable failure (e.g.
        a panic caught at the PyO3 boundary). Provided as a Python
        fallback when the native `_core` extension is unavailable."""

__all__ = [
    "IFCHeader",
    "IfcfastError",
    "Model",
    "ProductRow",
    "StoreyRow",
    "bundle",
    "cache",
    "classify",
    "clash",
    "example_path",
    "header",
    "open",
    "system_prompt",
]

# Single source of truth: read the version from the installed package
# metadata (populated by maturin from pyproject.toml at wheel-build
# time). Editable installs via `maturin develop` work the same way —
# the `.dist-info/` dir is written next to the wheel and
# `importlib.metadata` reads it. Falls back to "0.0.0+unknown" only on
# a source-only import without any install (tooling / type-check
# runs); in practice the live path always resolves.
#
# GH #46. Pre-fix `__version__` was hardcoded here and silently
# drifted out of sync with pyproject.toml / Cargo.toml — every release
# required four manual edits and a step you'd forget. Now the only
# spots that carry the version string are `pyproject.toml`,
# `Cargo.toml`, and `crates/core/Cargo.toml` (the Cargo files exist
# because the maturin build needs them at compile time, before the
# wheel exists; we accept that as a Rust-side fact).
from importlib.metadata import PackageNotFoundError, version as _pkg_version

try:
    __version__ = _pkg_version("ifcfast")
except PackageNotFoundError:  # pragma: no cover
    __version__ = "0.0.0+unknown"
del _pkg_version, PackageNotFoundError

# Namespace hygiene (GH #71): the __future__ import binds a module
# attribute; agents introspect dir(ifcfast), so keep it clean. The
# compile-time directive is unaffected by deleting the name.
del annotations


def example_path() -> _Path:
    """Path to the bundled minimal IFC4 fixture.

    Lets agents and CI runs demo ``ifcfast`` without sourcing an IFC
    separately::

        import ifcfast
        m = ifcfast.open(ifcfast.example_path())
        print(m.summary())

    The fixture is intentionally tiny (~2 KB) — one IfcBuilding, one
    IfcBuildingStorey, one IfcWall — so parse cost is sub-millisecond.
    For realistic benchmarks, use your own model.
    """
    return _Path(__file__).parent / "data" / "minimal.ifc"


def system_prompt() -> str:
    """Paste-into-agent description of the ``ifcfast`` library.

    Returns a compact, copy-pasteable paragraph that an LLM agent can
    drop into its own system prompt to ramp on ``ifcfast`` without
    reading source or PyPI. Stable across releases — additions only,
    never reorganisations.
    """
    return _SYSTEM_PROMPT


_SYSTEM_PROMPT = """\
You have access to ifcfast, an agent-first IFC parser. It reads IFC
files into pandas DataFrames, triangle meshes, and point clouds via a
native (Rust) core, with no geometry kernel on the hot path.

It is EXPERIMENTAL and under active development, NOT verified against
established tools, and the maintainers take no responsibility for
incorrect output. Do not take its numbers for granted: benchmark and
verify, especially geometric quantities (mesh_qto volumes / areas) —
the highest-risk surface, which can be silently wrong on edge cases
(open shells, non-watertight surface models, complex booleans). Before
relying on a value, cross-check it against an authoritative tool such
as ifcopenshell or Solibri. When you hit a discrepancy, bug, blind
spot, or edge case, report it in detail (file, GUID, expected vs.
actual, and the tool you compared against) at
https://github.com/EdvardGK/ifcfast/issues — detailed reproducers are
how these get fixed.

ifcfast is a speed-first companion to ifcopenshell, not a competitor:
it goes fast and flags what it can't do reliably; ifcopenshell owns the
heavy geometry kernel. Best practice is the hybrid pattern — run ifcfast
on everything, then escalate only the rows it flags as unreliable
(|volume_m3| > aabb_volume_m3) to ifcopenshell. The flagged set is tiny
(~0.3% on a real model), so you keep the speed and get kernel-grade
numbers exactly where needed. See examples/hybrid_qto_routing.py.

Open and inspect:
    import ifcfast
    m = ifcfast.open(path)                # strict=True raises on unresolvable units
    m.summary()                           # dict: schema, counts, tables, samples
    m.schemas                             # dict: column-level introspection
    m.preview("psets", n=5)               # sample rows from any table
    m.types()                             # {entity_name: count}
    m.by_type("IfcWall")                  # rows for a type (+subtypes, case-insensitive)
    m.unit_scale / m.length_unit          # source-unit → metres factor + unit name

Data layers (long-format pandas, lazy on first access):
    m.psets / m.quantities / m.materials / m.classifications / m.drift

Geometry (no CAD kernel required):
    m.meshes()                  # per-product triangles: (guid, entity, vertices, faces)
    m.mesh(guid)                # one product's mesh without tessellating the model
    m.iter_meshes() / m.iter_point_cloud()   # streaming variants (bounded memory)
    m.point_cloud(per_m2=1000)  # area-weighted surface samples + normals
    m.mesh_qto()                # -> (products_df, per_surface_df); volume/area/orientation + planar surfaces
    m.to_gltf("out.glb")        # viewer-ready glTF (cut_openings=True by default)
    # meshes() / point_cloud() take unit="m"|"mm"|"cm"|"ft"|"in" (default metres)

Writing (surgical, round-trippable):
    m.subset([guid, ...])                 # valid standalone IFC of those elements -> bytes
    m.hotswap(guid, vertices, triangles)  # swap one element's Body mesh -> bytes
    # both take out_path=... to write a file and return a stats dict
    # hotswap expects LOCAL-frame coords; m.meshes() vertices are WORLD-frame

Substrate + clash (GeoParquet, model-scale analysis):
    ifcfast.bundle(path, out_dir)         # representations/instances parquet substrate
    ifcfast.clash(bundle_dir)             # broad+narrow clash pass -> clashes.parquet
    # mesh_qto volume contract: SUM(volume_m3) only over volume_reliable rows

Spatial-relationship graph:
    m.contained_in / m.aggregates / m.storey_building   # DataFrames
    m.parent(g) / m.children(g) / m.ancestors(g) / m.descendants(g)
    m.storey_of(g) / m.building_of(g) / m.products_in(parent_g)

All traversal methods return None / [] on unknown guids — they never
raise. Filter ProductRow iteration via m.filter(entity=..., mode=...,
storey_guid=...). Compare two models with m.diff(other_path).

CLI (all subcommands accept --json for machine output):
    ifcfast demo                  # showcase against the bundled IFC
    ifcfast index FILE            # tier-1 parse + counts
    ifcfast schema FILE           # full schema introspection
    ifcfast types FILE            # type-first extraction (TypeBank shape)
    ifcfast extract FILE          # data layers
    ifcfast drift FILE            # placement-vs-mesh drift report
    ifcfast cache DIR             # inspect/clear the parse cache
    ifcfast bundle FILE OUT_DIR   # write the parquet substrate

For zero-network demos: ifcfast.open(ifcfast.example_path()).
"""
