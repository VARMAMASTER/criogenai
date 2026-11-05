// Builds the TypeScript SDK's engine for the browser (design 0021):
//
//   1. the Rust engine as WebAssembly (web/, wasm32-unknown-unknown);
//   2. its JavaScript glue (wasm-bindgen): web-dist/engine (an ES module, for
//      browsers, workers and bundlers) and web-dist/node (CommonJS: the Node
//      test suite on the WebAssembly engine, CRIOGEN_ENGINE=wasm);
//   3. smaller code (wasm-opt, from binaryen);
//   4. the browser bundle: lib/browser/index.ts with the glue, as one ES
//      module, dist-web/criogen.js, next to dist-web/criogen_bg.wasm; and its
//      types, dist-web/types.
//
// Usage: node scripts/build-web.mjs [--skip-cargo]   (needs Rust with the
// wasm32-unknown-unknown target and wasm-bindgen-cli at the version in
// web/Cargo.lock: cargo install wasm-bindgen-cli --version <it> --locked)

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
import * as esbuild from "esbuild";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const crate = join(root, "web");
const exe = process.platform === "win32" ? ".exe" : "";
const cargoBin = join(process.env.CARGO_HOME ?? join(homedir(), ".cargo"), "bin");

function tool(name) {
  const local = join(cargoBin, name + exe);
  return existsSync(local) ? local : name;
}

function run(command, args, options = {}) {
  console.log(`> ${[command, ...args].join(" ")}`);
  execFileSync(command, args, { stdio: "inherit", ...options });
}

// 1. The engine.
const wasmIn = join(crate, "target", "wasm32-unknown-unknown", "release", "criogen_web.wasm");
if (!process.argv.includes("--skip-cargo")) {
  run(tool("cargo"), ["build", "--release", "--locked", "--target", "wasm32-unknown-unknown", "--manifest-path", join(crate, "Cargo.toml")]);
}

// 2. The glue. wasm-bindgen must match the crate's version exactly.
const lock = readFileSync(join(crate, "Cargo.lock"), "utf8");
const wanted = /name = "wasm-bindgen"\nversion = "([^"]+)"/.exec(lock)?.[1];
const bindgen = tool("wasm-bindgen");
const have = execFileSync(bindgen, ["--version"], { encoding: "utf8" }).trim().split(/\s+/).pop();
if (wanted && have !== wanted) {
  throw new Error(`wasm-bindgen ${have} is installed, the engine needs ${wanted}: cargo install wasm-bindgen-cli --version ${wanted} --locked`);
}
const out = join(root, "web-dist");
rmSync(out, { recursive: true, force: true });
for (const [target, dir] of [["web", "engine"], ["nodejs", "node"]]) {
  run(bindgen, ["--target", target, "--out-dir", join(out, dir), "--out-name", "criogen", "--remove-producers-section", wasmIn]);
}

// 3. Smaller code: the features rustc emits for wasm32 by default.
const features = ["bulk-memory", "mutable-globals", "nontrapping-float-to-int", "sign-ext", "reference-types", "multivalue"];
const wasmOpt = join(root, "node_modules", ".bin", "wasm-opt" + (process.platform === "win32" ? ".cmd" : ""));
for (const dir of ["engine", "node"]) {
  const file = join(out, dir, "criogen_bg.wasm");
  run(wasmOpt, ["-Os", ...features.map((f) => `--enable-${f}`), "-o", file, file], { shell: process.platform === "win32" });
}

// 4. The browser bundle. Three Node-only modules are swapped for browser
// ones: the addon loader, web tools, and nothing else reaches Node.
const swaps = { "./native-node": "native-node.ts", "./web": "web.ts" };
const browserOnly = {
  name: "criogen-browser",
  setup(build) {
    build.onResolve({ filter: /^\.\/(native-node|web)$/ }, (args) => {
      if (resolve(args.resolveDir) !== join(root, "lib")) return undefined;
      return { path: join(root, "lib", "browser", swaps[args.path]) };
    });
    build.onResolve({ filter: /^(node:|path$|fs$)/ }, (args) => ({
      errors: [{ text: `${args.path} (from ${args.importer}) cannot run in a browser` }],
    }));
  },
};
const dist = join(root, "dist-web");
rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });
await esbuild.build({
  entryPoints: [join(root, "lib", "browser", "index.ts")],
  bundle: true,
  format: "esm",
  platform: "neutral",
  mainFields: ["module", "main"],
  target: "es2022",
  outfile: join(dist, "criogen.js"),
  sourcemap: true,
  legalComments: "none",
  plugins: [browserOnly],
  logLevel: "warning",
});
copyFileSync(join(out, "engine", "criogen_bg.wasm"), join(dist, "criogen_bg.wasm"));
run(process.execPath, [join(root, "node_modules", "typescript", "bin", "tsc"), "-p", join(root, "tsconfig.web.json")]);

const size = (file) => {
  const bytes = readFileSync(file);
  return `${(statSync(file).size / 1024).toFixed(0)} KiB (${(gzipSync(bytes, { level: 9 }).length / 1024).toFixed(0)} KiB gzipped)`;
};
console.log(`engine:  dist-web/criogen_bg.wasm  ${size(join(dist, "criogen_bg.wasm"))}`);
console.log(`library: dist-web/criogen.js       ${size(join(dist, "criogen.js"))}`);
