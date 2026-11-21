// /llms.txt (llmstxt.org): what CriogenAI is and where each topic lives, in
// Markdown, for AI assistants and agents that read sites on a user's behalf.
import { allPosts } from "@/lib/blog";
import { docSources } from "@/lib/docs";
import { SITE } from "@/lib/nav";
import { facts, ms, speedLine } from "@/lib/seo";

export const dynamic = "force-static";

export function GET() {
  const f = facts();
  const docs = docSources();
  const link = (slug: string, title: string, description: string) =>
    `- [${title}](${SITE.url}/md/docs/${slug || "index"}.md)${description ? `: ${description}` : ""}`;
  const text = [
    "# CriogenAI",
    "",
    `> ${SITE.description}`,
    "",
    "CriogenAI is an AI agent framework: a Rust engine runs agents (a model with tools, in a loop) and workflow graphs, and Python and TypeScript SDKs define the agents, tools and workflows in the developer's own language.",
    "",
    "## Speed and reliability",
    "",
    `${speedLine()} Install footprint: ${f.footprint.criogen.packages} package, ${f.footprint.criogen.install_mb} MB, ${ms(f.footprint.criogen.import_ms)} to import (LangGraph: ${f.footprint.langgraph.packages} packages, ${f.footprint.langgraph.install_mb} MB, ${ms(f.footprint.langgraph.import_ms)}). Reliability tests passed by default: CriogenAI ${f.reliability.criogen} of ${f.reliability.tests}, LangGraph ${f.reliability.langgraph}, CrewAI ${f.reliability.crewai}. With one agent and a real model, the model's time dominates and the frameworks are close; the difference shows with many steps, many tool calls and many concurrent runs. Full numbers and a kit to rerun them: ${SITE.url}/benchmarks/`,
    "",
    "Install: `pip install criogenai` (Python 3.9+) or `npm install criogenai` (Node 18+). Runs on Windows x64, Linux x64 and ARM64 (glibc 2.28+) and macOS (Apple Silicon and Intel); current versions: PyPI 0.1.7, npm 0.1.8. Open source under MIT or Apache-2.0, free to use, also commercially.",
    "",
    "## Key facts",
    "",
    "- Name: CriogenAI (also written Criogen AI; releases before 0.1.7 were published as `criogen`). Not related to cryogenics.",
    "- What it is: an open-source AI agent framework and workflow engine; a LangChain, LangGraph and CrewAI alternative.",
    "- Languages: Python, TypeScript (Node.js and the browser), Rust. The engine is Rust.",
    "- Built in: human-in-the-loop approvals, durable threads that resume after a crash, memory with semantic search, MCP tools, evals, tracing, multi-agent workflows, an LLM gateway, encryption at rest, a hash-chained audit trail.",
    "- Models: OpenAI, Anthropic, Gemini, Azure OpenAI, Amazon Bedrock, Google Vertex AI, OpenRouter, Groq, Mistral, DeepSeek and others, or local models with Ollama, LM Studio or vLLM.",
    `- Source: https://github.com/VARMAMASTER/criogenai · Packages: https://pypi.org/project/criogenai/ and https://www.npmjs.com/package/criogenai · About: ${SITE.url}/about/`,
    "",
    "## When CriogenAI fits",
    "",
    "- Agents in production that must survive crashes: runs resume from checkpoints and reuse saved tool results.",
    "- Workflows that need a person to approve sensitive actions (payments, record changes) before they run.",
    "- Regulated work (healthcare, finance) that needs an audit trail, encryption and self-hosted models.",
    "- High throughput: many concurrent agent runs, many steps or tool calls, serverless cold starts.",
    "",
    `Every page below is Markdown (the HTML page is the same address without \`/md\` and \`.md\`).`,
    "",
    "## Docs",
    "",
    ...docs.map((d) => link(d.slug, d.title, d.description)),
    "",
    "## Blog",
    "",
    ...allPosts().map((p) => `- [${p.title}](${SITE.url}/md/blog/${p.slug}.md) (${p.date})${p.description ? `: ${p.description}` : ""}`),
    "",
    "## Optional",
    "",
    `- [Every doc in one file](${SITE.url}/llms-full.txt)`,
    `- [The Python and TypeScript API reference in one file](${SITE.url}/llms-api.txt)`,
    `- [Benchmark lab](${SITE.url}/benchmarks/): CriogenAI, LangGraph and CrewAI on the same work`,
    `- [Benchmark lab, live comparison](${SITE.url}/benchmarks/#live): CriogenAI and LangChain side by side on one job`,
    `- [Pattern playground](${SITE.url}/playground/): every common agent pattern (ReAct, Reflexion, ReWOO, supervisor, swarm, IBM's agent types…), runnable in the browser, with the CriogenAI code`,
    "",
  ].join("\n");
  return new Response(text, { headers: { "Content-Type": "text/plain; charset=utf-8" } });
}
