// The feature tour's results (demos/feature-tour/tour.py writes them into
// public/demo/): every feature scenario with the code that ran, what it
// printed and which checks held. Read at build time; the page invents nothing.
import fs from "node:fs";
import path from "node:path";

export interface TourFeature {
  id: string;
  group: string;
  title: string;
  claim: string;
  code: string;
  steps: { label: string; value: unknown }[];
  checks: { claim: string; ok: boolean }[];
  ok: boolean;
  error: string | null;
  ms: number;
  attempts?: number;
}
export interface TourFile {
  ran_at: string;
  sdk: { name: string; version: string; language: string };
  machine: { os: string; python: string };
  model: { provider: string; name: string } | null;
  features: TourFeature[];
}

export function loadTour(file: "tour.json" | "live.json"): TourFile | null {
  try {
    return JSON.parse(fs.readFileSync(path.join(process.cwd(), "public", "demo", file), "utf-8")) as TourFile;
  } catch {
    return null;
  }
}

/** Group features in the order the tour defines them. */
export function byGroup(features: TourFeature[]): [string, TourFeature[]][] {
  const groups = new Map<string, TourFeature[]>();
  for (const f of features) groups.set(f.group, [...(groups.get(f.group) ?? []), f]);
  return [...groups.entries()];
}

/** A step's value as text for the page. */
export function valueText(value: unknown): string {
  if (typeof value === "string") return value;
  return JSON.stringify(value, null, 2);
}

/**
 * The status page's rows (website/content/docs/status.mdx) and the scenarios
 * that exercise each one. A row with no scenario is listed as not shown.
 */
export const COVERAGE: { feature: string; sim: string[]; live: string[] }[] = [
  { feature: "Agents with tools, limits, structured output", sim: ["agents", "validation", "tool-errors"], live: ["live-tools", "live-structured"] },
  { feature: "Human approval (approve, reject, edit)", sim: ["approvals"], live: ["live-approval"] },
  { feature: "Threads: saved conversations, retry, history, rewind", sim: ["threads", "crash-recovery"], live: [] },
  { feature: "Streaming and cancellation", sim: ["streaming"], live: ["live-streaming"] },
  { feature: "Storage: SQLite, in memory (Postgres: not in this tour)", sim: ["storage"], live: [] },
  { feature: "Encryption at rest, audit trail, retention", sim: ["encryption", "audit", "retention"], live: ["live-audit"] },
  { feature: "Long-term memory, by keyword and by meaning", sim: ["memory", "semantic-memory"], live: ["live-memory"] },
  { feature: "Workflows, durable pauses for a person", sim: ["workflows", "durable-workflows"], live: ["live-workflow"] },
  { feature: "Hooks around model and tool calls", sim: ["hooks"], live: ["live-hooks"] },
  { feature: "Agents as tools and as workflow steps", sim: ["agents-compose", "agent-nodes"], live: ["live-workflow"] },
  { feature: "Fallback models, retry policies", sim: ["resilience"], live: ["live-fallback"] },
  { feature: "Tracing to OpenTelemetry (Langfuse uses the same export)", sim: ["telemetry"], live: [] },
  { feature: "Calling a model directly (Gateway)", sim: ["gateway-sdk"], live: ["live-gateway"] },
  { feature: "Patterns: pipelines, review loops, coordinator, swarm, human review (demo runs the Python form)", sim: ["patterns-pipelines", "patterns-review-loop", "patterns-routing", "patterns-human-review"], live: ["live-patterns", "live-coordinator"] },
  { feature: "Specialist agents you can extend (demo runs the Python form)", sim: ["specialists"], live: ["live-patterns"] },
  { feature: "Web search and page reading (demo runs the Python form; Tavily needs a key, so a local stand-in server is used)", sim: ["web-search"], live: [] },
];
