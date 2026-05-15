"""MCP server smoke — verifies the stdio handshake, tool list, and
round-trip on a representative tool. Skipped if the optional ``mcp``
dependency isn't installed."""

from __future__ import annotations

import asyncio

import pytest

mcp_client = pytest.importorskip("mcp.client.stdio")
from mcp.client.session import ClientSession
from mcp.client.stdio import stdio_client, StdioServerParameters


EXPECTED_TOOLS = {
    "system_prompt", "example_path", "open_ifc",
    "summary", "schemas", "preview", "types", "by_type",
    "parent", "children", "ancestors", "descendants",
    "storey_of", "building_of", "products_in",
    "diff", "list_open", "close",
}


async def _drive_session():
    params = StdioServerParameters(command="ifcfast-mcp")
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            tools = await session.list_tools()
            tool_names = {t.name for t in tools.tools}
            result = await session.call_tool("example_path", {})
            return tool_names, result.content[0].text


def test_mcp_server_advertises_expected_tools():
    tool_names, example = asyncio.run(_drive_session())
    missing = EXPECTED_TOOLS - tool_names
    assert not missing, f"tools missing from server: {missing}"
    assert example.endswith("minimal.ifc"), example


def test_mcp_server_open_ifc_roundtrip():
    async def go():
        params = StdioServerParameters(command="ifcfast-mcp")
        async with stdio_client(params) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                result = await session.call_tool(
                    "open_ifc", {"path": "example"},
                )
                # MCP server's open_ifc returns the summary dict, MCP
                # wraps as text/JSON content. We just verify the call
                # didn't error and we got text back.
                assert len(result.content) > 0
                text = result.content[0].text
                assert "IFC4" in text or "schema" in text

    asyncio.run(go())
