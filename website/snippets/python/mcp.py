import os

from criogenai import Agent, mcp


async def main() -> None:
    # [show]
    # A server this app starts (stdio): any MCP server, in any language.
    files = await mcp.stdio("npx", ["-y", "@modelcontextprotocol/server-filesystem", "./notes"], prefix="files")

    # A server at a URL (Streamable HTTP), with a token in a header.
    docs = await mcp.http(
        "https://mcp.example.com/mcp",
        headers={"Authorization": f"Bearer {os.environ['DOCS_MCP_TOKEN']}"},
        prefix="docs",
    )

    agent = Agent(
        "openai:gpt-5-mini",
        tools=[files, docs.tool("docs_search")],  # a server = all its tools, or pick some
        approve=["files_write_file"],  # MCP tools work with approvals like any tool
    )
    result = await agent.run("Summarize what ./notes says about the Q3 launch.")
    print(result.text)

    await files.close()
    await docs.close()
    # [/show]

    # [show direct]
    async with await mcp.stdio("uvx", ["mcp-server-time"]) as time:
        print([t.name for t in time.tools], time.info["protocol_version"])
        print(await time.call("get_current_time", {"timezone": "Europe/Lisbon"}))
    # [/show direct]
