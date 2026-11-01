// The playground's data: the events a playground server (playground/server.py)
// streams, or a recording of them (public/playground/recorded.json), turned
// into what the page shows. Every number is computed from those events; the
// page invents none.

export type Fw = "criogen" | "langchain";
export const FRAMEWORKS: { id: Fw; label: string }[] = [
  { id: "criogen", label: "Criogen" },
  { id: "langchain", label: "LangChain" },
];

export interface Job {
  message: string;
  tools: number;
  model_delay_ms: number;
  tool_delay_ms: number;
  runs: number;
  concurrency: number;
  order: "parallel" | "sequential";
  balanced?: boolean;
  limited?: Record<string, [number, number]>;
}

export type PlayEvent =
  | { e: "job"; job: Job; cores?: number }
  | { e: "end" }
  | { e: "timeout" }
  | { e: "start"; fw: Fw; pass?: number; version: string; ready_ms: number }
  | { e: "tool"; fw: Fw; pass?: number; name: string; start_ms: number; end_ms: number }
  | { e: "first_run"; fw: Fw; pass?: number; ms: number; answer: string }
  | { e: "run"; fw: Fw; pass?: number; ms: number }
  | { e: "done"; fw: Fw; pass?: number; runs: number[]; wall_ms: number }
  | { e: "sample"; fw: Fw; pass?: number; t: number; cpu: number; rss_mb: number }
  | { e: "error"; fw: Fw; pass?: number; error: string }
  | { e: "exit"; fw: Fw; pass?: number; code: number; peak_rss_mb: number; cpu_time_s: number };

export interface Sample {
  t: number;
  cpu: number;
  rss: number;
}
export interface Pass {
  tools: { name: string; start_ms: number; end_ms: number }[];
  samples: Sample[];
  runs: number[];
  answer?: string;
  ready_ms?: number;
  first_ms?: number;
  wall_ms?: number;
  peak_rss_mb?: number;
  cpu_time_s?: number;
  exited?: boolean;
}
export interface Metrics {
  ready_ms?: number;
  first_ms?: number;
  median?: number;
  p95?: number;
  throughput?: number;
  peak_rss_mb?: number;
  cpu_time_s?: number;
}
export interface FwState {
  status: "waiting" | "running" | "done" | "error";
  version?: string;
  passes: Pass[];
  // derived
  tools: Pass["tools"];
  samples: Sample[];
  answer: string;
  metrics: Metrics;
  error?: string;
}
export interface PlayState {
  job?: Job;
  cores?: number;
  note?: string;
  fw: Record<Fw, FwState>;
}

export function fresh(): PlayState {
  const one = (): FwState => ({ status: "waiting", passes: [], tools: [], samples: [], answer: "", metrics: {} });
  return { fw: { criogen: one(), langchain: one() } };
}

const mean = (list: (number | undefined)[]): number | undefined => {
  const v = list.filter((x): x is number => x != null);
  return v.length ? v.reduce((a, b) => a + b, 0) / v.length : undefined;
};

export function stats(runs: number[]): { median?: number; p95?: number } {
  if (!runs.length) return {};
  const sorted = [...runs].sort((a, b) => a - b);
  const at = (q: number) => sorted[Math.min(sorted.length - 1, Math.round((sorted.length - 1) * q))];
  return { median: at(0.5), p95: at(0.95) };
}

/** One framework's numbers from all its passes (side by side: one; A B B A: two). */
function derive(state: PlayState, id: Fw): void {
  const s = state.fw[id];
  const passes = s.passes.filter(Boolean);
  if (!passes.length) return;
  const first = passes[0];
  s.tools = first.tools;
  s.samples = first.samples;
  s.answer = first.answer ?? "";
  const expected = state.job?.order === "sequential" && state.job.balanced ? 2 : 1;
  if (s.status !== "error") s.status = passes.length < expected ? "running" : passes.every((p) => p.exited) ? "done" : "running";
  const m: Metrics = (s.metrics = {});
  m.ready_ms = mean(passes.map((p) => p.ready_ms));
  m.first_ms = mean(passes.map((p) => p.first_ms));
  m.peak_rss_mb = mean(passes.map((p) => p.peak_rss_mb));
  m.cpu_time_s = mean(passes.map((p) => p.cpu_time_s));
  const runs = passes.flatMap((p) => p.runs);
  Object.assign(m, stats(runs));
  const wall = passes.reduce((a, p) => a + (p.wall_ms ?? 0), 0);
  if (wall && runs.length) m.throughput = (runs.length / wall) * 1000;
}

/** Apply one event (mutates `state`; the caller re-renders). */
export function apply(state: PlayState, ev: PlayEvent): void {
  if (ev.e === "job") {
    state.job = ev.job;
    state.cores = ev.cores;
    return;
  }
  if (ev.e === "timeout") {
    state.note = "Stopped: it took too long.";
    return;
  }
  if (ev.e === "end") return;
  const s = state.fw[ev.fw];
  if (!s) return;
  const p = (s.passes[ev.pass ?? 0] ??= { tools: [], samples: [], runs: [] });
  switch (ev.e) {
    case "start":
      s.version = ev.version;
      p.ready_ms = ev.ready_ms;
      break;
    case "tool":
      p.tools.push({ name: ev.name, start_ms: ev.start_ms, end_ms: ev.end_ms });
      break;
    case "first_run":
      p.answer = ev.answer;
      p.first_ms = ev.ms;
      break;
    case "run":
      p.runs.push(ev.ms);
      break;
    case "done":
      p.runs = ev.runs;
      p.wall_ms = ev.wall_ms;
      break;
    case "sample":
      p.samples.push({ t: ev.t, cpu: ev.cpu, rss: ev.rss_mb });
      break;
    case "error":
      s.status = "error";
      s.error = ev.error;
      break;
    case "exit":
      p.peak_rss_mb = ev.peak_rss_mb;
      p.cpu_time_s = ev.cpu_time_s;
      p.exited = true;
      if (ev.code !== 0) s.status = "error";
      break;
  }
  derive(state, ev.fw);
}

export interface Row {
  label: string;
  key: keyof Metrics;
  unit: string;
  digits: number;
  higher?: boolean;
  /** Part of the headline chips. */
  chip?: boolean;
}
export const ROWS: Row[] = [
  { label: "Import and build time", key: "ready_ms", unit: " ms", digits: 0, chip: true },
  { label: "First run (cold)", key: "first_ms", unit: " ms", digits: 0 },
  { label: "Median run", key: "median", unit: " ms", digits: 1, chip: true },
  { label: "95th percentile run", key: "p95", unit: " ms", digits: 1 },
  { label: "Throughput (runs/s)", key: "throughput", unit: "", digits: 1, higher: true },
  { label: "Peak memory", key: "peak_rss_mb", unit: " MB", digits: 1, chip: true },
  { label: "CPU time", key: "cpu_time_s", unit: " s", digits: 2, chip: true },
];

export interface Compared {
  row: Row;
  criogen?: number;
  langchain?: number;
  win: Fw | null;
  note: string;
}

export const fmt = (n: number | undefined, digits = 0): string =>
  n == null ? "–" : n.toLocaleString("en-US", { maximumFractionDigits: digits, minimumFractionDigits: digits });

/** The table: which framework is ahead on each row, and by how much (under 1.1x counts as the same). */
export function compare(state: PlayState): Compared[] {
  const out: Compared[] = [];
  for (const row of ROWS) {
    const x = state.fw.criogen.metrics[row.key];
    const y = state.fw.langchain.metrics[row.key];
    if (x == null && y == null) continue;
    let win: Fw | null = null;
    let note = "";
    if (x != null && y != null && x > 0 && y > 0) {
      const abAhead = row.higher ? x > y : x < y;
      const ratio = row.higher ? (abAhead ? x / y : y / x) : abAhead ? y / x : x / y;
      if (ratio >= 1.1) {
        win = abAhead ? "criogen" : "langchain";
        note = `${abAhead ? "Criogen" : "LangChain"} ${fmt(ratio, ratio < 10 ? 1 : 0)}× ${row.higher ? "higher" : "lower"}`;
      } else note = "within 10%";
    }
    out.push({ row, criogen: x, langchain: y, win, note });
  }
  return out;
}

/** Events with the time (ms since the job started) they happened at, for replays. */
export function timeOf(ev: PlayEvent): number {
  if ("t" in ev && typeof (ev as { t?: unknown }).t === "number") return (ev as { t: number }).t;
  return 0;
}

export interface Recording {
  id: string;
  label: string;
  about: string;
  events: PlayEvent[];
}
export interface Recorded {
  recorded_at: string;
  machine: { cpu?: string; cores?: number; ram_gb?: number; os?: string; python?: string };
  recordings: Recording[];
}

/**
 * Replay order: events as recorded sorted by their own time. Events without a
 * time (job, tool, run...) stay right after the last timed event before them,
 * so a replay shows them as they were streamed.
 */
export function replayPlan(events: PlayEvent[]): { at: number; ev: PlayEvent }[] {
  let now = 0;
  return events.map((ev) => {
    const t = timeOf(ev);
    if (t > 0) now = Math.max(now, t);
    return { at: now, ev };
  });
}
