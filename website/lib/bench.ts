// The framework comparison's results file, schema `agentbuild-compare/1`
// (docs/design/0016-framework-comparison.md §4), and a validator that runs
// in the browser (for a file a visitor drops into the lab) and at build time.
// Every number the lab shows comes from such a file. Any field may be
// missing or null; the lab says so instead of guessing.

export const SCHEMA = "agentbuild-compare/1";

export interface BenchFramework {
  label: string;
  version?: string | null;
  also?: Record<string, string> | null;
  model_us?: number | null;
}
export interface BenchScenario {
  id: string;
  kind: string; // speed | load | footprint
  title: string;
  detail?: string | null;
  unit?: string | null;
}
export type Status = "ok" | "unsupported" | "error";
export interface BenchResult {
  scenario: string;
  framework: string;
  status: Status;
  runs?: number | null;
  median_ms?: number | null;
  p95_ms?: number | null;
  min_ms?: number | null;
  max_ms?: number | null;
  wall_ms?: number | null;
  cpu_ms?: number | null;
  baseline_rss_mb?: number | null;
  peak_rss_mb?: number | null;
  rss_per_run_kb?: number | null;
  /** Load scenarios: the wall time of every repeated batch (the median one is reported). */
  batch_walls_ms?: number[] | null;
  model_us?: number | null;
  samples_ms?: number[] | null;
  note?: string | null;
  error?: string | null;
}
export type Outcome = "pass" | "partial" | "fail";
export interface BenchReliability {
  test: string;
  framework: string;
  outcome: Outcome;
  reason?: string | null;
  log?: string | null;
  note?: string | null;
}
export interface BenchFootprint {
  framework: string;
  import_ms?: number | null;
  packages?: number | null;
  install_mb?: number | null;
}
export interface BenchMachine {
  cpu?: string | null;
  cores?: number | null;
  threads?: number | null;
  ram_gb?: number | null;
  os?: string | null;
}
export interface BenchFile {
  schema: typeof SCHEMA;
  recorded: string;
  command: string;
  machine: BenchMachine;
  python?: string | null;
  frameworks: Record<string, BenchFramework>;
  scenarios: BenchScenario[];
  results: BenchResult[];
  reliability: BenchReliability[];
  footprint: BenchFootprint[];
  /** Corrections made to a recorded file after its run, and why. */
  amended?: { at: string; what: string; why: string }[] | null;
}

export type Validation = { ok: true; data: BenchFile; warnings: string[] } | { ok: false; errors: string[] };

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const isStr = (v: unknown): v is string => typeof v === "string";
const optNum = (v: unknown) => v === undefined || v === null || (typeof v === "number" && Number.isFinite(v));
const optStr = (v: unknown) => v === undefined || v === null || typeof v === "string";

const RESULT_NUMBERS = [
  "runs",
  "median_ms",
  "p95_ms",
  "min_ms",
  "max_ms",
  "wall_ms",
  "cpu_ms",
  "baseline_rss_mb",
  "peak_rss_mb",
  "rss_per_run_kb",
  "model_us",
] as const;

/** Check a parsed JSON value against `agentbuild-compare/1`. */
export function validate(raw: unknown): Validation {
  const errors: string[] = [];
  const warnings: string[] = [];
  const err = (m: string) => errors.length < 25 && errors.push(m);
  if (!isObj(raw)) return { ok: false, errors: ["The file is not a JSON object."] };
  if (raw.schema !== SCHEMA) {
    err(`"schema" must be "${SCHEMA}" (found ${JSON.stringify(raw.schema ?? null)}).`);
    return { ok: false, errors };
  }
  if (!isStr(raw.recorded)) err(`"recorded" must be a date string.`);
  if (!isStr(raw.command)) err(`"command" must be a string.`);
  if (!isObj(raw.machine)) err(`"machine" must be an object.`);
  else {
    for (const k of ["cpu", "os"]) if (!optStr(raw.machine[k])) err(`machine.${k} must be a string.`);
    for (const k of ["cores", "threads", "ram_gb"]) if (!optNum(raw.machine[k])) err(`machine.${k} must be a number.`);
  }
  if (!optStr(raw.python)) err(`"python" must be a string.`);
  if (!isObj(raw.frameworks)) err(`"frameworks" must be an object of { label, version }.`);
  else
    for (const [id, f] of Object.entries(raw.frameworks)) {
      if (!isObj(f) || !isStr(f.label)) err(`frameworks.${id} needs a "label".`);
      else {
        if (!optStr(f.version)) err(`frameworks.${id}.version must be a string.`);
        if (!optNum(f.model_us)) err(`frameworks.${id}.model_us must be a number.`);
        if (f.also !== undefined && f.also !== null && !isObj(f.also)) err(`frameworks.${id}.also must be an object.`);
      }
    }
  const arr = (k: string) => {
    const v = raw[k];
    if (!Array.isArray(v)) {
      err(`"${k}" must be an array.`);
      return [] as unknown[];
    }
    return v as unknown[];
  };
  arr("scenarios").forEach((s, i) => {
    if (!isObj(s) || !isStr(s.id) || !isStr(s.kind) || !isStr(s.title)) err(`scenarios[${i}] needs "id", "kind" and "title" strings.`);
    else if (!optStr(s.detail) || !optStr(s.unit)) err(`scenarios[${i}]: "detail" and "unit" must be strings.`);
  });
  arr("results").forEach((r, i) => {
    if (!isObj(r) || !isStr(r.scenario) || !isStr(r.framework)) return err(`results[${i}] needs "scenario" and "framework".`);
    if (r.status !== "ok" && r.status !== "unsupported" && r.status !== "error")
      err(`results[${i}].status must be ok, unsupported or error.`);
    for (const k of RESULT_NUMBERS) if (!optNum(r[k])) err(`results[${i}].${k} must be a number or null.`);
    if (r.samples_ms !== undefined && r.samples_ms !== null) {
      if (!Array.isArray(r.samples_ms) || !r.samples_ms.every((x) => typeof x === "number" && Number.isFinite(x)))
        err(`results[${i}].samples_ms must be an array of numbers.`);
    }
    if (!optStr(r.note) || !optStr(r.error)) err(`results[${i}]: "note" and "error" must be strings or null.`);
  });
  arr("reliability").forEach((r, i) => {
    if (!isObj(r) || !isStr(r.test) || !isStr(r.framework)) return err(`reliability[${i}] needs "test" and "framework".`);
    if (r.outcome !== "pass" && r.outcome !== "partial" && r.outcome !== "fail")
      err(`reliability[${i}].outcome must be pass, partial or fail.`);
    for (const k of ["reason", "log", "note"]) if (!optStr(r[k])) err(`reliability[${i}].${k} must be a string or null.`);
  });
  arr("footprint").forEach((f, i) => {
    if (!isObj(f) || !isStr(f.framework)) return err(`footprint[${i}] needs "framework".`);
    for (const k of ["import_ms", "packages", "install_mb"]) if (!optNum(f[k])) err(`footprint[${i}].${k} must be a number or null.`);
  });
  if (errors.length) return { ok: false, errors };

  const data = raw as unknown as BenchFile;
  const scenarioIds = new Set(data.scenarios.map((s) => s.id));
  for (const r of data.results) {
    if (!scenarioIds.has(r.scenario)) warnings.push(`A result names scenario "${r.scenario}", which is not in "scenarios".`);
    if (!data.frameworks[r.framework]) warnings.push(`A result names framework "${r.framework}", which is not in "frameworks".`);
  }
  return { ok: true, data, warnings: [...new Set(warnings)].slice(0, 10) };
}

// ---- Presentation helpers (pure; used by the lab) ---------------------------

/** Known frameworks first, in this order; then any other in the file. */
export const FRAMEWORK_ORDER = ["agentbuild", "langgraph", "crewai"];

export function frameworkIds(files: BenchFile[]): string[] {
  const seen = new Set<string>();
  for (const f of files) {
    for (const id of Object.keys(f.frameworks)) seen.add(id);
    for (const r of f.results) seen.add(r.framework);
  }
  return [...FRAMEWORK_ORDER.filter((id) => seen.has(id)), ...[...seen].filter((id) => !FRAMEWORK_ORDER.includes(id)).sort()];
}

/** A colour slot per framework: fixed by identity, never by rank. */
export function colorOf(id: string): string {
  const i = FRAMEWORK_ORDER.indexOf(id);
  return `var(--fw-${i >= 0 ? i + 1 : 4})`;
}

export function labelOf(files: BenchFile[], id: string): string {
  for (const f of files) if (f.frameworks[id]?.label) return f.frameworks[id].label;
  return id;
}

/** `toPrecision` without trailing zeros after the decimal point (100 stays 100). */
function trim(n: number, digits: number): string {
  const s = n.toPrecision(digits);
  if (s.includes("e")) return String(Number(s));
  return s.includes(".") ? s.replace(/0+$/, "").replace(/\.$/, "") : s;
}

export function fmtMs(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return "–";
  if (ms === 0) return "0";
  if (ms < 1) return `${trim(ms * 1000, 3)} µs`;
  if (ms < 1000) return `${trim(ms, 3)} ms`;
  return `${trim(ms / 1000, 3)} s`;
}

export function fmtNum(n: number | null | undefined, unit = "", digits = 3): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return "–";
  const s = n >= 1000 ? Math.round(n).toLocaleString("en-US") : trim(n, digits);
  return unit ? `${s} ${unit}` : s;
}

export function quantile(sorted: number[], q: number): number {
  if (!sorted.length) return NaN;
  const pos = (sorted.length - 1) * q;
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo);
}
