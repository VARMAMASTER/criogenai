// The lab's recorded results, read at build time.
//
// Production: public/bench/results.json when it exists (copied there from
// bench/results/ by whoever records a run), else nothing: the lab then says
// "No results recorded yet".
// Development: BENCH_FIXTURE=1 (scripts/run.mjs --fixture) shows
// fixtures/bench-sample.json instead: fake numbers, marked as such on the
// page. The fixture is never copied into public/.
import fs from "node:fs";
import path from "node:path";
import { validate, type BenchFile } from "./bench";

export interface Recorded {
  data: BenchFile | null;
  /** Problems with a results file that exists but does not validate. */
  errors: string[];
  fixture: boolean;
  /** Where "Download raw JSON" points (null: offer the data from memory). */
  href: string | null;
}

export function loadRecorded(): Recorded {
  const fixture = process.env.BENCH_FIXTURE === "1";
  const file = fixture
    ? path.join(process.cwd(), "fixtures", "bench-sample.json")
    : path.join(process.cwd(), "public", "bench", "results.json");
  if (!fs.existsSync(file)) return { data: null, errors: [], fixture: false, href: null };
  let raw: unknown;
  try {
    raw = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (e) {
    return { data: null, errors: [`${path.basename(file)} is not valid JSON: ${(e as Error).message}`], fixture, href: null };
  }
  const v = validate(raw);
  if (!v.ok) {
    if (!fixture) {
      // A broken committed file should fail the build, not ship an empty lab.
      throw new Error(`public/bench/results.json does not match criogen-compare/1:\n- ${v.errors.join("\n- ")}`);
    }
    return { data: null, errors: v.errors, fixture, href: null };
  }
  return { data: v.data, errors: [], fixture, href: fixture ? null : "/bench/results.json" };
}
