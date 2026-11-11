// The docs sidebar: the same groups, labels and order as the Astro site
// (website/astro.config.mjs). Slugs are paths under /docs/ ("" is /docs/).
import { API_NAV } from "./api-nav";

export interface NavItem {
  label: string;
  slug: string;
}
export interface NavGroup {
  label: string;
  items: NavItem[];
}

export const SITE = {
  title: "Criogen",
  url: "https://criogen.vercel.app",
  description:
    "A fast AI agent framework and LangChain alternative: agents and workflows on a Rust engine, used from Python, TypeScript or Rust, with tools, approvals, durable threads, memory, an audit trail and an LLM gateway.",
};

export const SIDEBAR: NavGroup[] = [
  {
    label: "Get started",
    items: [
      { label: "What Criogen is", slug: "" },
      { label: "Install", slug: "install" },
      { label: "Quickstart", slug: "quickstart" },
      { label: "Status: built and planned", slug: "status" },
      { label: "Coming from LangGraph", slug: "from-langgraph" },
    ],
  },
  {
    label: "Tutorials",
    items: [{ label: "Build a support agent", slug: "tutorial/support-agent" }],
  },
  {
    label: "Concepts",
    items: [
      { label: "Agents and the loop", slug: "concepts/agent-loop" },
      { label: "How a run works", slug: "concepts/runs" },
      { label: "Messages and tools", slug: "concepts/messages-and-tools" },
      { label: "State, storage and threads", slug: "concepts/state-and-threads" },
      { label: "Errors and retries", slug: "concepts/errors-and-retries" },
    ],
  },
  {
    label: "Build agents",
    items: [
      { label: "Agents", slug: "guides/agents" },
      { label: "Tools", slug: "guides/tools" },
      { label: "Human approval", slug: "guides/approvals" },
      { label: "Hooks", slug: "guides/hooks" },
      { label: "Streaming", slug: "guides/streaming" },
      { label: "MCP servers' tools", slug: "guides/mcp" },
      { label: "Testing without a model", slug: "guides/testing" },
    ],
  },
  {
    label: "Data and memory",
    items: [
      { label: "Conversations (threads)", slug: "guides/threads" },
      { label: "Storage: SQLite and Postgres", slug: "guides/storage" },
      { label: "Long-term memory", slug: "guides/memory" },
      { label: "Collections and retrieval", slug: "guides/retrieval" },
    ],
  },
  {
    label: "Orchestration",
    items: [
      { label: "Workflows", slug: "guides/workflows" },
      { label: "Patterns and specialists", slug: "guides/patterns" },
    ],
  },
  {
    label: "Models and gateway",
    items: [
      { label: "Models and providers", slug: "guides/models" },
      { label: "The LLM gateway", slug: "guides/gateway" },
      { label: "In the browser (WebAssembly)", slug: "guides/browser" },
    ],
  },
  {
    label: "Production and compliance",
    items: [
      { label: "Using Criogen in your app", slug: "guides/production" },
      { label: "Deploying", slug: "guides/deploying" },
      { label: "Encryption, audit, retention", slug: "guides/compliance" },
      { label: "Tracing (Langfuse, OpenTelemetry)", slug: "guides/telemetry" },
      { label: "Evals and quality gates", slug: "guides/evals" },
    ],
  },
  {
    label: "Help",
    items: [{ label: "Troubleshooting and FAQ", slug: "troubleshooting" }],
  },
  {
    label: "Reference",
    items: [
      { label: "API reference", slug: "reference/api" },
      { label: "Error codes", slug: "reference/errors" },
      { label: "JSON formats", slug: "reference/schemas" },
      { label: "Benchmarks", slug: "reference/benchmarks" },
    ],
  },
  // The Python, TypeScript and Rust API pages: generated (scripts/gen-api.mjs).
  ...API_NAV,
];

export const ALL_PAGES: NavItem[] = SIDEBAR.flatMap((g) => g.items);

export function hrefOf(slug: string): string {
  return slug ? `/docs/${slug}/` : "/docs/";
}

/** The docs header's sections, Docusaurus-style. */
export function sectionOf(slug: string): "docs" | "guides" | "api" {
  if (slug.startsWith("guides/")) return "guides";
  if (slug.startsWith("reference/")) return "api";
  return "docs";
}

export const SECTIONS = [
  { id: "docs", label: "Docs", href: "/docs/" },
  { id: "guides", label: "Guides", href: "/docs/guides/agents/" },
  { id: "api", label: "API", href: "/docs/reference/api/" },
] as const;

export const FOOTER_COLUMNS = [
  {
    title: "Learn",
    links: [
      { label: "Quickstart", href: "/docs/quickstart/" },
      { label: "Tutorial: a support agent", href: "/docs/tutorial/support-agent/" },
      { label: "Human approval", href: "/docs/guides/approvals/" },
      { label: "Troubleshooting", href: "/docs/troubleshooting/" },
    ],
  },
  {
    title: "Reference",
    links: [
      { label: "API reference", href: "/docs/reference/api/" },
      { label: "Error codes", href: "/docs/reference/errors/" },
      { label: "Benchmarks", href: "/docs/reference/benchmarks/" },
    ],
  },
  {
    title: "Project",
    links: [
      { label: "Built and planned", href: "/docs/status/" },
      { label: "llms.txt", href: "/llms.txt" },
    ],
  },
];
