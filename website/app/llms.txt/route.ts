// /llms.txt (llmstxt.org): what Criogen is and where each topic lives, in
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
    `- [${title}](${SITE.url}/docs/${slug ? `${slug}/` : ""})${description ? `: ${description}` : ""}`;
  const text = [
    "# Criogen",
    "",
    `> ${SITE.description}`,
    "",
    "Criogen is an AI agent framework: a Rust engine runs agents (a model with tools, in a loop) and workflow graphs, and Python and TypeScript SDKs define the agents, tools and workflows in the developer's own language.",
    "",
    "## Speed and reliability",
    "",
    `${speedLine()} Install footprint: ${f.footprint.criogen.packages} package, ${f.footprint.criogen.install_mb} MB, ${ms(f.footprint.criogen.import_ms)} to import (LangGraph: ${f.footprint.langgraph.packages} packages, ${f.footprint.langgraph.install_mb} MB, ${ms(f.footprint.langgraph.import_ms)}). Reliability tests passed by default: Criogen ${f.reliability.criogen} of ${f.reliability.tests}, LangGraph ${f.reliability.langgraph}, CrewAI ${f.reliability.crewai}. With one agent and a real model, the model's time dominates and the frameworks are close; the difference shows with many steps, many tool calls and many concurrent runs. Full numbers and a kit to rerun them: ${SITE.url}/benchmarks/`,
    "",
    "Install: `npm install criogen` (Node 18+, Linux x64/ARM64, macOS, Windows x64). Python: `pip install criogen` (Python 3.9+, the same platforms). Free to use, also commercially.",
    "",
    "## Docs",
    "",
    ...docs.map((d) => link(d.slug, d.title, d.description)),
    "",
    "## Blog",
    "",
    ...allPosts().map((p) => `- [${p.title}](${SITE.url}/blog/${p.slug}/) (${p.date})${p.description ? `: ${p.description}` : ""}`),
    "",
    "## Optional",
    "",
    `- [Every doc in one file](${SITE.url}/llms-full.txt)`,
    `- [The Python and TypeScript API reference in one file](${SITE.url}/llms-api.txt)`,
    `- [Benchmark lab](${SITE.url}/benchmarks/): Criogen, LangGraph and CrewAI on the same work`,
    `- [Benchmark lab, live comparison](${SITE.url}/benchmarks/#live): Criogen and LangChain side by side on one job`,
    `- [Pattern playground](${SITE.url}/playground/): every common agent pattern (ReAct, Reflexion, ReWOO, supervisor, swarm, IBM's agent types…), runnable in the browser, with the Criogen code`,
    "",
  ].join("\n");
  return new Response(text, { headers: { "Content-Type": "text/plain; charset=utf-8" } });
}
