// Build or develop the site: `node scripts/run.mjs [--fixture] build|dev`.
//
// build: copies ../schemas into public/schemas, builds the playground's
//        engine into public/engine (scripts/build-engine.mjs: the engine as
//        WebAssembly and the code samples on it), runs `next build` (a static
//        export into out/), then indexes out/ with Pagefind for the search.
// dev:   the same copies, then `next dev`.
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

// The playground runs its code samples on the engine in the browser (design 0021).
run(path.join(root, "scripts", "build-engine.mjs"), []);

const env = { BENCH_FIXTURE: fixture ? "1" : "" };
if (fixture) console.warn("\n[criogen] FIXTURE MODE: the benchmark lab shows fake numbers. Do not deploy this build.\n");

const next = path.join(root, "node_modules", "next", "dist", "bin", "next");
if (mode === "dev") {
  run(next, ["dev", "--port", "4400"], env);
} else {
  run(next, ["build"], env);
  run(path.join(root, "node_modules", "pagefind", "lib", "runner", "bin.cjs"), ["--site", "out"]);
  dottedSegments(path.join(root, "out"));
  if (fixture) console.warn("\n[criogen] out/ was built with the FIXTURE: do not deploy it.\n");
}

// Next writes a page's data for client navigation as
// `__next.docs/$c$slug/__PAGE__.txt` but asks for `__next.docs.$c$slug.__PAGE__.txt`.
// A static host serves only the files that exist, so write the second name too
// (the first stays): then navigation and prefetching work on any host.
function dottedSegments(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (!entry.isDirectory()) continue;
    if (entry.name.startsWith("__next.")) {
      const walk = (d, prefix) => {
        for (const e of fs.readdirSync(d, { withFileTypes: true })) {
          if (e.isDirectory()) walk(path.join(d, e.name), `${prefix}.${e.name}`);
          else if (e.name.endsWith(".txt")) fs.copyFileSync(path.join(d, e.name), path.join(dir, `${prefix}.${e.name}`));
        }
      };
      walk(full, entry.name);
    } else {
      dottedSegments(full);
    }
  }
}
