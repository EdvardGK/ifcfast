"""ifcfast — the agent-first IFC parser.

Fastest open-source tier-1 IFC indexer (20-30× faster than
``ifcopenshell.open``) with a spatial-relationship graph, parquet cache,
and a stable, introspectable API. Designed for AI agents, RPA, and
analytics pipelines that need to ask questions of an IFC without
loading a 200 MB geometry kernel.

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

from pathlib import Path

from .header import IFCHeader, header
from .model import Model, ProductRow, StoreyRow, open_ifc as open
from . import cache, classify

__all__ = [
    "IFCHeader",
    "Model",
    "ProductRow",
    "StoreyRow",
    "cache",
    "classify",
    "example_path",
    "header",
    "open",
    "system_prompt",
]

__version__ = "0.3.0"


def example_path() -> Path:
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
    return Path(__file__).parent / "data" / "minimal.ifc"


def system_prompt() -> str:
    """Paste-into-agent description of the ``ifcfast`` library.

    Returns a compact, copy-pasteable paragraph that an LLM agent can
    drop into its own system prompt to ramp on ``ifcfast`` without
    reading source or PyPI. Stable across releases — additions only,
    never reorganisations.
    """
    return _SYSTEM_PROMPT


_SYSTEM_PROMPT = """\
You have access to ifcfast, the agent-first IFC parser. It's the
fastest open-source tier-1 IFC indexer (20-30× faster than
ifcopenshell.open) with byte-level parity on the audited set.

Open and inspect:
    import ifcfast
    m = ifcfast.open(path)                # ~30 ms hot, ~1 s cold per 100 MB
    m.summary()                           # dict: schema, counts, tables, samples
    m.schemas                             # dict: column-level introspection
    m.preview("psets", n=5)               # sample rows from any table
    m.types()                             # {entity_name: count}

Pandas tables (long format, lazy on first access):
    m.psets / m.quantities / m.materials / m.classifications / m.drift

Spatial-relationship graph:
    m.contained_in / m.aggregates / m.storey_building   # DataFrames
    m.parent(g) / m.children(g) / m.ancestors(g) / m.descendants(g)
    m.storey_of(g) / m.building_of(g) / m.products_in(parent_g)

All traversal methods return None / [] on unknown guids — they never
raise. Filter ProductRow iteration via m.filter(entity=..., mode=...,
storey_guid=...).

CLI (all subcommands accept --json for machine output):
    ifcfast demo                  # showcase against the bundled IFC
    ifcfast index FILE            # tier-1 parse + counts
    ifcfast schema FILE           # full schema introspection
    ifcfast extract FILE          # data layers
    ifcfast drift FILE            # placement-vs-mesh drift report

For zero-network demos: ifcfast.open(ifcfast.example_path()).
"""
