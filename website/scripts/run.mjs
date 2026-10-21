// Build or develop the site: `node scripts/run.mjs [--fixture] build|dev`.
//
// build: copies ../schemas into public/schemas, runs `next build` (a static
//        export into out/), then indexes out/ with Pagefind for the search.
// dev:   the same copy, then `next dev`.
// --fixture: the benchmark lab shows fixtures/bench-sample.json (fake
//        numbers, marked as such on the page) instead of
//        public/bench/results.json. For development only: never deploy a
//        fixture build.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const fixture = args.includes("--fixture");
const mode = args.find((a) => !a.startsWith("--")) ?? "build";

function run(file, argv, env = {}) {
  const r = spawnSync(process.execPath, [file, ...argv], { cwd: root, stdio: "inherit", env: { ...process.env, ...env } });
  if (r.status !== 0) process.exit(r.status ?? 1);
}

// The JSON Schemas the docs link to (/schemas/…), from the repository.
const schemas = path.join(root, "..", "schemas");
const target = path.join(root, "public", "schemas");
fs.rmSync(target, { recursive: true, force: true });
fs.mkdirSync(target, { recursive: true });
for (const f of fs.readdirSync(schemas).filter((f) => f.endsWith(".json"))) {
  fs.copyFileSync(path.join(schemas, f), path.join(target, f));
}

const env = { BENCH_FIXTURE: fixture ? "1" : "" };
if (fixture) console.warn("\n[agentbuild] FIXTURE MODE: the benchmark lab shows fake numbers. Do not deploy this build.\n");

const next = path.join(root, "node_modules", "next", "dist", "bin", "next");
if (mode === "dev") {
  run(next, ["dev", "--port", "4400"], env);
} else {
  run(next, ["build"], env);
  run(path.join(root, "node_modules", "pagefind", "lib", "runner", "bin.cjs"), ["--site", "out"]);
  if (fixture) console.warn("\n[agentbuild] out/ was built with the FIXTURE: do not deploy it.\n");
}
