// What search engines and AI assistants read about CriogenAI: the claims come
// from the recorded benchmark file (public/bench/results.json), so a page can
// never say more than was measured. Build time only.
import fs from "node:fs";
import path from "node:path";
import type { Metadata } from "next";
import { SITE } from "@/lib/nav";

type Result = { scenario: string; framework: string; status: string; median_ms: number };
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
  const crowd = {
    criogen: median(data, "concurrent_1000", "criogen"),
    langgraph: median(data, "concurrent_1000", "langgraph"),
    crewai: median(data, "concurrent_1000", "crewai"),
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
    crowdVsLangGraph: times(crowd.langgraph, crowd.criogen),
    reliability: { tests, criogen: passed("criogen"), langgraph: passed("langgraph"), crewai: passed("crewai") },
    footprint: fp,
  };
}

/** 0.29 ms, 333 ms, 2.85 s. */
export const ms = (v: number) => (v < 10 ? `${v.toFixed(2)} ms` : v < 1000 ? `${Math.round(v)} ms` : `${(v / 1000).toFixed(2)} s`);

/** One sentence of measured speed, for descriptions. */
export function speedLine(): string {
  const f = facts();
  return `Measured on one machine: ${ms(f.turn.criogen)} of framework overhead per agent turn against ${ms(f.turn.langgraph)} for LangGraph and ${ms(f.turn.crewai)} for CrewAI, and 1,000 concurrent agent runs in ${ms(f.crowd.criogen)} against ${ms(f.crowd.langgraph)} for LangGraph.`;
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

export function pageMeta(options: { title: string; description: string; path: string; absolute?: boolean; type?: "website" | "article" }): Metadata {
  const shown = options.absolute ? options.title : `${options.title} | CriogenAI`;
  return {
    title: options.absolute ? { absolute: options.title } : options.title,
    description: options.description,
    alternates: { canonical: options.path },
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
      applicationCategory: "DeveloperApplication",
      applicationSubCategory: "AI agent framework",
      operatingSystem: "Linux, macOS, Windows",
      softwareVersion: packageVersion(),
      programmingLanguage: ["Python", "TypeScript", "Rust"],
      url: `${SITE.url}/`,
      image: `${SITE.url}/icon-512.png`,
      downloadUrl: "https://www.npmjs.com/package/criogen",
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
      url: `${SITE.url}/`,
      logo: { "@type": "ImageObject", url: `${SITE.url}/icon-512.png`, width: 512, height: 512 },
      sameAs: ["https://www.npmjs.com/package/criogen", "https://pypi.org/project/criogen/"],
    },
  ];
  // `<` escaped so the JSON cannot close the script tag.
  return JSON.stringify({ "@context": "https://schema.org", "@graph": graph }).replace(/</g, "\\u003c");
}

/** FAQPage structured data for questions that are visible on the page. */
export function faqJsonLd(items: { q: string; a: string }[]): string {
  const data = {
    "@context": "https://schema.org",
    "@type": "FAQPage",
    mainEntity: items.map((item) => ({ "@type": "Question", name: item.q, acceptedAnswer: { "@type": "Answer", text: item.a } })),
  };
  return JSON.stringify(data).replace(/</g, "\u003c");
}
