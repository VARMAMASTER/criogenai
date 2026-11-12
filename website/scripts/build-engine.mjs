// The engine for the playground (design 0021), in public/engine:
//
//   criogenai.js, criogenai_bg.wasm   the TypeScript SDK's browser build
//                                 (sdks/typescript/dist-web), copied;
//   playground.js                 the code samples (snippets/typescript)
//                                 bundled with esbuild, where "criogenai" is
//                                 engine/runtime.js: the same engine, plus a
//                                 report of what the samples do.
//
// Builds the SDK's browser build first when it is missing (needs Rust with
// the wasm32-unknown-unknown target and wasm-bindgen-cli: see
// sdks/typescript/scripts/build-web.mjs). Run by scripts/run.mjs.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const site = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sdk = path.join(site, "..", "sdks", "typescript");
const dist = path.join(sdk, "dist-web");
const out = path.join(site, "public", "engine");

if (!fs.existsSync(path.join(dist, "criogenai.js")) || process.argv.includes("--rebuild")) {
  console.log("[engine] building the SDK's browser build (sdks/typescript/scripts/build-web.mjs)");
  const r = spawnSync(process.execPath, [path.join(sdk, "scripts", "build-web.mjs")], { cwd: sdk, stdio: "inherit" });
  if (r.status !== 0) process.exit(r.status ?? 1);
}

fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
for (const file of ["criogenai.js", "criogenai_bg.wasm"]) fs.copyFileSync(path.join(dist, file), path.join(out, file));

const esbuild = createRequire(path.join(sdk, "package.json"))("esbuild");
await esbuild.build({
  entryPoints: [path.join(site, "engine", "entry.js")],
  bundle: true,
  format: "esm",
  platform: "browser",
  target: "es2022",
  outfile: path.join(out, "playground.js"),
  // The engine is loaded beside this bundle at run time, once for both.
  external: ["./criogenai.js"],
  minify: true,
  legalComments: "none",
  logLevel: "warning",
  plugins: [
    {
      name: "criogenai-is-the-runtime",
      setup(build) {
        build.onResolve({ filter: /^criogenai$/ }, () => ({ path: path.join(site, "engine", "runtime.js") }));
        // zod (the samples' tool schemas) is the SDK's pinned one, never a
        // copy higher up the folder tree (a stray node_modules in a home folder).
        build.onResolve({ filter: /^zod(\/.*)?$/ }, (args) =>
          args.pluginData === "sdk" ? undefined : build.resolve(args.path, { kind: args.kind, resolveDir: sdk, pluginData: "sdk" }),
        );
      },
    },
  ],
});
const kib = (f) => `${(fs.statSync(path.join(out, f)).size / 1024).toFixed(0)} KiB`;
console.log(`[engine] public/engine: criogenai_bg.wasm ${kib("criogenai_bg.wasm")}, criogenai.js ${kib("criogenai.js")}, playground.js ${kib("playground.js")}`);
