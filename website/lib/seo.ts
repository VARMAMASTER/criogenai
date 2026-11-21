// What search engines and AI assistants read about CriogenAI: the claims come
// from the recorded benchmark file (public/bench/results.json), so a page can
// never say more than was measured. Build time only.
import fs from "node:fs";
import path from "node:path";
import type { Metadata } from "next";
import { SITE } from "@/lib/nav";

type Result = { scenario: string; framework: string; status: string; median_ms: number; wall_ms?: number | null };
type Footprint = { framework: string; import_ms: number; packages: number; install_mb: number };
type Recorded = { recorded: string; results: Result[]; footprint: Footprint[]; reliability: { framework: string; outcome: string }[] };

function recorded(): Recorded {
  const file = path.join(process.cwd(), "public", "bench", "results.json");
  return JSON.parse(fs.readFileSync(file, "utf8")) as Recorded;
}

function median(data: Recorded, scenario: string, framework: string): number {
  const row = data.results.find((r) => r.scenario === scenario && r.framework === framework && r.status === "ok");
  if (!row) throw new Error(`benchmark ${scenario}/${framework} missing from results.json`);
  return row.median_ms;
}

/** A load scenario's wall time: the whole batch, start to last run finished (what the charts show). */
function wall(data: Recorded, scenario: string, framework: string): number {
  const row = data.results.find((r) => r.scenario === scenario && r.framework === framework && r.status === "ok");
  if (!row || typeof row.wall_ms !== "number") throw new Error(`benchmark ${scenario}/${framework} has no wall_ms in results.json`);
  return row.wall_ms;
}

/** "11.7×": one decimal, rounded down, never flattering. */
function timesExact(slow: number, fast: number): string {
  return `${Math.floor((slow / fast) * 10) / 10}×`;
}

/** The npm package's version (sdks/typescript/package.json), the released one. */
function packageVersion(): string {
  const file = path.join(process.cwd(), "..", "sdks", "typescript", "package.json");
  return (JSON.parse(fs.readFileSync(file, "utf8")) as { version: string }).version;
}

/** "4×", "100×": rounded down, never flattering. */
function times(slow: number, fast: number): string {
  const ratio = slow / fast;
  return `${ratio >= 10 ? Math.floor(ratio / 10) * 10 : Math.floor(ratio)}×`;
}

/** The measured facts every page quotes, with the run they come from. */
export function facts() {
  const data = recorded();
  const turn = { criogen: median(data, "agent_call", "criogen"), langgraph: median(data, "agent_call", "langgraph"), crewai: median(data, "agent_call", "crewai") };
  // 1,000 runs at once: the whole batch's wall time, not one run's median.
  const crowd = {
    criogen: wall(data, "concurrent_1000", "criogen"),
    langgraph: wall(data, "concurrent_1000", "langgraph"),
    crewai: wall(data, "concurrent_1000", "crewai"),
  };
  const passed = (fw: string) => data.reliability.filter((r) => r.framework === fw && r.outcome === "pass").length;
  const tests = data.reliability.filter((r) => r.framework === "criogen").length;
  const fp = Object.fromEntries(data.footprint.map((f) => [f.framework, f])) as Record<string, Footprint>;
  return {
    date: data.recorded.slice(0, 10),
    turn,
    crowd,
    vsLangGraph: times(turn.langgraph, turn.criogen),
    vsCrewAI: times(turn.crewai, turn.criogen),
    crowdVsLangGraph: timesExact(crowd.langgraph, crowd.criogen),
    reliability: { tests, criogen: passed("criogen"), langgraph: passed("langgraph"), crewai: passed("crewai") },
    footprint: fp,
  };
}

/** 0.29 ms, 33.9 ms, 333 ms, 2.85 s. */
export const ms = (v: number) =>
  v < 10 ? `${v.toFixed(2)} ms` : v < 100 ? `${v.toFixed(1)} ms` : v < 1000 ? `${Math.round(v)} ms` : `${(v / 1000).toFixed(2)} s`;

/** A batch's wall time in seconds, as the blog quotes it: 0.42 s, 4.95 s, 78.2 s. */
export const secs = (v: number) => `${(v / 1000).toFixed(v < 10000 ? 2 : 1)} s`;

/** One sentence of measured speed, for descriptions. */
export function speedLine(): string {
  const f = facts();
  return `Measured on one machine: ${ms(f.turn.criogen)} of framework overhead per agent turn against ${ms(f.turn.langgraph)} for LangGraph and ${ms(f.turn.crewai)} for CrewAI, and 1,000 concurrent agent runs in ${secs(f.crowd.criogen)} for the whole batch against ${secs(f.crowd.langgraph)} for LangGraph (${f.crowdVsLangGraph}).`;
}

export const KEYWORDS = [
  "AI agent framework",
  "fast AI agent framework",
  "LangChain alternative",
  "LangGraph alternative",
  "CrewAI alternative",
  "Rust AI agents",
  "Python AI agents",
  "TypeScript AI agents",
  "agent orchestration",
  "multi-agent framework",
  "LLM workflows",
  "tool calling",
  "human in the loop",
  "durable agents",
  "LLM gateway",
];

/**
 * A page's metadata: title, description, its own canonical URL, and the same
 * text on link cards (Open Graph, X). `title` absolute when `absolute` is set.
 */
/** Bumped when the mark changes: browsers and link previewers (LinkedIn, X,
 * Slack) cache icons and cards by address, so a new address shows the new logo. */
export const ICON_VERSION = 2;

/** The card image (app/og.png/route.tsx), on every page. */
export const OG_IMAGE = {
  url: `/og.png?v=${ICON_VERSION}`,
  width: 1200,
  height: 630,
  alt: "CriogenAI: the fast AI agent framework, a LangChain alternative",
  type: "image/png",
};

/** Docs pages and blog posts also exist as Markdown for AI agents (app/md). */
function markdownAlternate(path: string): { types?: Record<string, string> } {
  const trimmed = path.replace(/^\/|\/$/g, "");
  if (trimmed === "docs") return { types: { "text/markdown": "/md/docs/index.md" } };
  if (trimmed.startsWith("docs/") || (trimmed.startsWith("blog/") && trimmed.length > 5)) return { types: { "text/markdown": `/md/${trimmed}.md` } };
  return {};
}

export function pageMeta(options: { title: string; description: string; path: string; absolute?: boolean; type?: "website" | "article" }): Metadata {
  // Search results show about 60 characters: the " | CriogenAI" suffix goes when it would not fit.
  const absolute = options.absolute || `${options.title} | CriogenAI`.length > 60;
  options = { ...options, absolute };
  const shown = options.absolute ? options.title : `${options.title} | CriogenAI`;
  return {
    title: options.absolute ? { absolute: options.title } : options.title,
    description: options.description,
    alternates: { canonical: options.path, ...markdownAlternate(options.path) },
    openGraph: { type: options.type ?? "website", siteName: "CriogenAI", locale: "en_US", url: options.path, title: shown, description: options.description, images: [OG_IMAGE] },
    twitter: { card: "summary_large_image", title: shown, description: options.description, images: [OG_IMAGE.url] },
  };
}

/** Structured data for the home page: the software and the site. */
export function jsonLd(): string {
  const f = facts();
  const graph = [
    {
      "@type": "SoftwareApplication",
      "@id": `${SITE.url}/#software`,
      name: "CriogenAI",
      alternateName: ["Criogen AI", "Criogen"],
      applicationCategory: "DeveloperApplication",
      applicationSubCategory: "AI agent framework",
      operatingSystem: "Linux, macOS, Windows",
      softwareVersion: packageVersion(),
      programmingLanguage: ["Python", "TypeScript", "Rust"],
      url: `${SITE.url}/`,
      image: `${SITE.url}/icon-512.png`,
      downloadUrl: "https://pypi.org/project/criogenai/",
      codeRepository: "https://github.com/VARMAMASTER/criogenai",
      license: "https://github.com/VARMAMASTER/criogenai/blob/main/LICENSE-MIT",
      installUrl: `${SITE.url}/docs/install/`,
      softwareHelp: `${SITE.url}/docs/`,
      description: `${SITE.description} ${speedLine()}`,
      offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
      featureList: [
        `${f.vsLangGraph} less framework overhead per agent turn than LangGraph, ${f.vsCrewAI} less than CrewAI (benchmark of ${f.date})`,
        "Agent loop with concurrent tool calls, limits, retries and fallback models",
        "Human approval before chosen tool calls",
        "Durable threads on SQLite or Postgres, resumable after a crash",
        "Long-term memory with keyword and semantic search",
        "Audit trail with a hash chain, encryption at rest",
        "LLM gateway with OpenAI and Anthropic wire formats",
      ],
    },
    { "@type": "WebSite", "@id": `${SITE.url}/#website`, name: "CriogenAI", url: `${SITE.url}/`, about: { "@id": `${SITE.url}/#software` }, publisher: { "@id": `${SITE.url}/#org` } },
    // The logo search engines show with results (at least 112 px, raster).
    {
      "@type": "Organization",
      "@id": `${SITE.url}/#org`,
      name: "CriogenAI",
      alternateName: ["Criogen AI", "Criogen"],
      url: `${SITE.url}/`,
      logo: { "@type": "ImageObject", url: `${SITE.url}/icon-512.png`, width: 512, height: 512 },
      sameAs: ["https://github.com/VARMAMASTER/criogenai", "https://www.npmjs.com/package/criogenai", "https://pypi.org/project/criogenai/", "https://www.linkedin.com/company/criogenai/"],
    },
  ];
  return ldString({ "@context": "https://schema.org", "@graph": graph });
}

/** JSON-LD as script text; `<` escaped so the JSON cannot close the script tag. */
export function ldString(data: unknown): string {
  return JSON.stringify(data).replace(/</g, "\\u003c");
}

/** FAQPage structured data for questions that are visible on the page. */
export function faqJsonLd(items: { q: string; a: string }[]): string {
  const data = {
    "@context": "https://schema.org",
    "@type": "FAQPage",
    mainEntity: items.map((item) => ({ "@type": "Question", name: item.q, acceptedAnswer: { "@type": "Answer", text: item.a } })),
  };
  return ldString(data);
}

/** The benchmark as a Dataset: original, citable data with its raw file and rerun kit. */
export function benchmarkJsonLd(): string {
  const f = facts();
  return ldString({
    "@context": "https://schema.org",
    "@type": "Dataset",
    "@id": `${SITE.url}/benchmarks/#dataset`,
    name: "AI agent framework benchmark: CriogenAI, LangGraph and CrewAI",
    description: `Framework overhead, load, memory, reliability and install footprint of CriogenAI, LangGraph and CrewAI in Python, on the same work with the same simulated model, so only the framework is measured. ${speedLine()}`,
    url: `${SITE.url}/benchmarks/`,
    creator: { "@id": `${SITE.url}/#org`, "@type": "Organization", name: "CriogenAI", url: `${SITE.url}/` },
    datePublished: f.date,
    dateModified: f.date,
    isAccessibleForFree: true,
    license: "https://github.com/VARMAMASTER/criogenai/blob/main/LICENSE-MIT",
    keywords: ["AI agent framework benchmark", "framework overhead", "LangGraph", "CrewAI", "agent latency", "concurrent agent runs"],
    measurementTechnique:
      "Each framework runs the same scenarios in its own virtual environment against one simulated model with fixed reply times, so the time measured is the framework's; medians of repeated runs on one machine.",
    variableMeasured: [
      "framework overhead per agent turn (ms)",
      "wall time of 1,000 concurrent agent runs (s)",
      "peak memory (MB)",
      "reliability tests passed",
      "install size (MB) and package count",
    ],
    distribution: [
      { "@type": "DataDownload", encodingFormat: "application/json", contentUrl: `${SITE.url}/bench/results.json` },
      { "@type": "DataDownload", encodingFormat: "application/zip", contentUrl: `${SITE.url}/bench/criogen-compare-kit.zip` },
    ],
  });
}

/** A page's breadcrumb trail: home, then each named level. */
export function breadcrumbLd(trail: { name: string; path: string }[]) {
  const crumbs = [{ name: "CriogenAI", path: "/" }, ...trail];
  return {
    "@type": "BreadcrumbList",
    itemListElement: crumbs.map((c, i) => ({ "@type": "ListItem", position: i + 1, name: c.name, item: `${SITE.url}${c.path}` })),
  };
}

/** A docs or playground page: a technical article on the site, with its breadcrumb. */
export function articleJsonLd(page: { title: string; description: string; path: string; trail: { name: string; path: string }[] }): string {
  const url = `${SITE.url}${page.path}`;
  return ldString({
    "@context": "https://schema.org",
    "@graph": [
      {
        "@type": "TechArticle",
        headline: page.title,
        description: page.description,
        url,
        mainEntityOfPage: url,
        inLanguage: "en",
        isPartOf: { "@id": `${SITE.url}/#website` },
        about: { "@id": `${SITE.url}/#software` },
        publisher: { "@id": `${SITE.url}/#org`, "@type": "Organization", name: "CriogenAI" },
      },
      breadcrumbLd(page.trail),
    ],
  });
}

/** The first candidate that fits `max` characters (titles about 60, descriptions about 160), else the shortest. */
export function fit(candidates: string[], max: number): string {
  return candidates.find((c) => c.length <= max) ?? [...candidates].sort((a, b) => a.length - b.length)[0];
}
