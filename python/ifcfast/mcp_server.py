"""``ifcfast-mcp`` — Model Context Protocol server for ``ifcfast``.

Exposes ifcfast's parse + spatial-graph + drift surface as MCP tools so
any MCP-aware agent (Claude Desktop, Cursor, ChatGPT via MCP, etc.) can
drive IFC files directly. No custom integration code on the agent's
side — drop the server into the client's config and the agent can
open IFCs, walk the spatial graph, run drift, and extract type
catalogues.

Quick start::

    pip install 'ifcfast[mcp]'
    ifcfast-mcp                 # stdio transport (Claude Desktop default)

Or wire into Claude Desktop / Cursor:

    {
      "mcpServers": {
        "ifcfast": { "command": "ifcfast-mcp" }
      }
    }

The server keeps an in-process cache of opened models keyed by path.
Every tool call stats the file and transparently reopens it when the
size or mtime changed (GH #83) — so "open model → re-export from the
authoring tool → query again" always answers from the *current* file.
Opening the same unchanged file twice within a session is free; the
underlying parquet cache makes hot reloads cheap across sessions too.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Optional

from mcp.server.fastmcp import FastMCP

import ifcfast


mcp = FastMCP(
    "ifcfast",
    instructions=(
        "ifcfast is the agent-first IFC parser. Use open_ifc() to load "
        "a file, then summary/schemas/preview/types/by_type/ancestors/"
        "descendants/diff to inspect it. All paths are filesystem paths "
        "on the host running this server."
    ),
)


# In-process model cache so repeated tool calls don't re-parse.
_open_models: dict[str, ifcfast.Model] = {}


def _resolve(path: str) -> ifcfast.Model:
    """Open ``path`` (or return cached). Empty / "example" → bundled fixture.

    Staleness-checked (GH #83): if the file's size or mtime changed
    since the cached Model was opened, the stale entry is dropped and
    the file reopened — the parquet cache absorbs the reopen cost, and
    the agent never silently queries a pre-re-export model.
    """
    if not path or path == "example":
        path = str(ifcfast.example_path())
    p = str(Path(path).expanduser().resolve())
    m = _open_models.get(p)
    if m is not None:
        st = os.stat(p)
        if (
            st.st_size != m.header.size_bytes
            or st.st_mtime_ns != m.header.mtime_ns
        ):
            del _open_models[p]
            m = None
    if m is None:
        m = ifcfast.open(p)
        _open_models[p] = m
    return m


def _records(df, limit: int) -> list[dict]:
    """First ``limit`` DataFrame rows as JSON-safe dicts (NaN → None)."""
    out = []
    for row in df.head(limit).to_dict(orient="records"):
        out.append(
            {
                k: (None if isinstance(v, float) and v != v else v)
                for k, v in row.items()
            }
        )
    return out


# ----------------------------------------------------------------------
# Tools
# ----------------------------------------------------------------------


@mcp.tool()
def system_prompt() -> str:
    """Return ifcfast's paste-into-agent system prompt — every method,
    every CLI subcommand, the conventions you can rely on."""
    return ifcfast.system_prompt()


@mcp.tool()
def example_path() -> str:
    """Path to the bundled minimal IFC4 fixture for offline demos."""
    return str(ifcfast.example_path())


@mcp.tool()
def open_ifc(path: str) -> dict:
    """Open an IFC and return its summary.

    Pass ``"example"`` to use the bundled fixture. The model stays
    cached for follow-up tool calls.
    """
    m = _resolve(path)
    return m.summary()


@mcp.tool()
def summary(path: str) -> dict:
    """Cheap snapshot of an opened IFC — schema, counts, available
    tables with shape + loaded-state. Does not trigger extracts."""
    return _resolve(path).summary()


@mcp.tool()
def schemas(path: str) -> dict:
    """Column-level dtype introspection of every table on the model."""
    return _resolve(path).schemas


@mcp.tool()
def preview(path: str, table: str, n: int = 5) -> list[dict]:
    """Sample rows from any table as plain list-of-dicts.

    Tables: ``products`` / ``storeys`` / ``contained_in`` /
    ``aggregates`` / ``storey_building`` / ``psets`` / ``quantities``
    / ``materials`` / ``classifications`` / ``drift``.
    """
    return _resolve(path).preview(table, n=n)


@mcp.tool()
def types(path: str, with_data: bool = False, samples: int = 3) -> list[dict]:
    """Type-first extraction: one record per IFC entity type.

    Fields: ``entity``, ``count``, ``storeys``, ``predefined_types``,
    ``object_types``, ``sample_guids``. With ``with_data=True`` also
    includes ``materials`` and ``classifications`` (triggers extract).
    """
    m = _resolve(path)
    return m.type_bank(sample_guids=samples) if with_data else m.type_summary(
        sample_guids=samples
    )


@mcp.tool()
def by_type(path: str, entity: str, limit: int = 100) -> list[dict]:
    """All products of a given entity type (e.g. ``"IfcWall"``).

    Returns up to ``limit`` rows as plain dicts. Mirrors
    ``ifcopenshell.file.by_type(entity)``.
    """
    from dataclasses import asdict

    m = _resolve(path)
    rows = [asdict(p) for p in m.by_type(entity)[:limit]]
    return rows


@mcp.tool()
def parent(path: str, guid: str) -> Optional[str]:
    """Unified parent guid of ``guid`` (aggregate, else spatial storey)."""
    return _resolve(path).parent(guid)


@mcp.tool()
def children(path: str, guid: str) -> list[str]:
    """Unified direct children of ``guid``."""
    return _resolve(path).children(guid)


@mcp.tool()
def ancestors(path: str, guid: str) -> list[str]:
    """Chain from ``guid`` to root (e.g. wall → storey → building → site → project)."""
    return _resolve(path).ancestors(guid)


@mcp.tool()
def descendants(path: str, guid: str) -> list[str]:
    """BFS over the unified-children tree under ``guid``."""
    return _resolve(path).descendants(guid)


@mcp.tool()
def storey_of(path: str, guid: str) -> Optional[str]:
    """Spatial container (storey guid) for a product."""
    return _resolve(path).storey_of(guid)


@mcp.tool()
def building_of(path: str, guid: str) -> Optional[str]:
    """Building guid that hosts the storey of ``guid``."""
    return _resolve(path).building_of(guid)


@mcp.tool()
def products_in(path: str, parent_guid: str) -> list[str]:
    """All product guids under ``parent_guid`` (BFS, filtered to products)."""
    return _resolve(path).products_in(parent_guid)


@mcp.tool()
def diff(left_path: str, right_path: str, sample: int = 10) -> dict:
    """Compare two IFC files — products added/removed/changed,
    type cardinality deltas, storey changes. JSON-friendly."""
    left = _resolve(left_path)
    right = _resolve(right_path)
    return left.diff(right, sample=sample)


@mcp.tool()
def psets(
    path: str,
    guid: Optional[str] = None,
    pset_name: Optional[str] = None,
    prop_name: Optional[str] = None,
    limit: int = 200,
) -> list[dict]:
    """Property-set rows, filtered. The workhorse data question.

    Long-format rows: ``guid`` / ``pset_name`` / ``prop_name`` /
    ``value`` / ``value_type`` / ``source``. Filter by element
    ``guid``, ``pset_name`` (e.g. ``"Pset_WallCommon"``), and/or
    ``prop_name`` (e.g. ``"FireRating"``). Returns up to ``limit``
    rows — on a big model, always filter.
    """
    df = _resolve(path).psets
    if guid is not None:
        df = df[df["guid"] == guid]
    if pset_name is not None:
        df = df[df["pset_name"] == pset_name]
    if prop_name is not None:
        df = df[df["prop_name"] == prop_name]
    return _records(df, limit)


@mcp.tool()
def quantities(
    path: str,
    guid: Optional[str] = None,
    qto_name: Optional[str] = None,
    quantity_name: Optional[str] = None,
    limit: int = 200,
) -> list[dict]:
    """Base-quantity rows (lengths/areas/volumes/counts/weights), filtered.

    Long-format rows: ``guid`` / ``qto_name`` / ``quantity_name`` /
    ``value`` / ``quantity_type`` / ``unit_step_id`` / ``source``.
    These are the *authored* quantities from the file (IfcElementQuantity),
    not geometric measurements — for measured values see ``mesh_qto``
    in the Python API. Returns up to ``limit`` rows.
    """
    df = _resolve(path).quantities
    if guid is not None:
        df = df[df["guid"] == guid]
    if qto_name is not None:
        df = df[df["qto_name"] == qto_name]
    if quantity_name is not None:
        df = df[df["quantity_name"] == quantity_name]
    return _records(df, limit)


@mcp.tool()
def materials(
    path: str,
    guid: Optional[str] = None,
    material_name: Optional[str] = None,
    limit: int = 200,
) -> list[dict]:
    """Material-assignment rows, filtered.

    Long-format rows: ``guid`` / ``role`` / ``layer_index`` /
    ``material_name`` / ``layer_thickness_mm`` / ``category`` /
    ``fraction``. Returns up to ``limit`` rows.
    """
    df = _resolve(path).materials
    if guid is not None:
        df = df[df["guid"] == guid]
    if material_name is not None:
        df = df[df["material_name"] == material_name]
    return _records(df, limit)


@mcp.tool()
def product_card(path: str, guid: str) -> Optional[dict]:
    """Everything about one element in a single call.

    Returns the product row plus its psets, quantities, materials,
    classifications, and resolved storey/building guids — the answer
    to "tell me about this element" without five round-trips.
    ``None`` if the guid is unknown.
    """
    from dataclasses import asdict

    m = _resolve(path)
    p = m.product(guid)
    if p is None:
        return None
    card: dict = {"product": asdict(p)}
    for table in ("psets", "quantities", "materials", "classifications"):
        df = getattr(m, table)
        card[table] = _records(df[df["guid"] == guid], 500) if len(df) else []
    card["storey_guid"] = m.storey_of(guid)
    card["building_guid"] = m.building_of(guid)
    card["ancestors"] = m.ancestors(guid)
    return card


@mcp.tool()
def list_open() -> list[str]:
    """Currently-open IFC paths in this MCP session."""
    return sorted(_open_models.keys())


@mcp.tool()
def close(path: str) -> bool:
    """Drop a model from the in-process cache (parquet cache on disk
    is untouched). Returns ``True`` if a model was removed."""
    p = str(Path(path).expanduser().resolve())
    if p in _open_models:
        del _open_models[p]
        return True
    return False


# ----------------------------------------------------------------------
# Resources
# ----------------------------------------------------------------------


@mcp.resource("ifcfast://agents-guide")
def agents_guide() -> str:
    """The full AGENTS.md guide — agent onboarding, decision tree,
    performance budget, conventions."""
    candidates = [
        Path(__file__).parent.parent.parent / "AGENTS.md",
        Path(__file__).parent / "AGENTS.md",
    ]
    for p in candidates:
        if p.exists():
            return p.read_text(encoding="utf-8")
    return ifcfast.system_prompt()


# ----------------------------------------------------------------------
# Entry point
# ----------------------------------------------------------------------


def main() -> None:
    """Run the MCP server on stdio (the Claude Desktop / Cursor default)."""
    mcp.run()


if __name__ == "__main__":
    main()
