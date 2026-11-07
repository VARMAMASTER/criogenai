// The MCP guide's samples (content/docs/guides/mcp.mdx), design 0022.
import { Agent, mcp, openai } from "criogen";

export async function basics() {
  // [show]
  // A server this app starts (stdio): any MCP server, in any language.
  const files = await mcp.stdio("npx", ["-y", "@modelcontextprotocol/server-filesystem", "./notes"], { prefix: "files" });

  // A server at a URL (Streamable HTTP), with a token in a header.
  const docs = await mcp.http("https://mcp.example.com/mcp", {
    headers: { Authorization: `Bearer ${process.env.DOCS_MCP_TOKEN}` },
    prefix: "docs",
  });

  const agent = new Agent({
    model: openai("gpt-5-mini"),
    tools: [files, docs.tool("docs_search")], // a server = all its tools, or pick some
    approve: ["files_write_file"], // MCP tools work with approvals like any tool
  });
  const result = await agent.run("Summarize what ./notes says about the Q3 launch.");
  console.log(result.text);

  await files.close();
  await docs.close();
  // [/show]
}

export async function direct() {
  // [show direct]
  const time = await mcp.stdio("uvx", ["mcp-server-time"]);
  console.log(time.tools.map((t) => t.name), time.info.protocolVersion);
  console.log(await time.call("get_current_time", { timezone: "Europe/Lisbon" }));
  await time.close();
  // [/show direct]
}
