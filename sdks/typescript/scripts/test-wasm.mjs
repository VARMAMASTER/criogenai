// The whole test suite on the WebAssembly engine (design 0021) instead of the
// Node addon: CRIOGEN_ENGINE=wasm. What needs a server is skipped, with the
// reason. Build first: npm run build && npm run build:web.
import { spawnSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const tests = readdirSync(join(root, "test")).filter((f) => f.endsWith(".test.js")).map((f) => join("test", f));
const result = spawnSync(process.execPath, ["--test", ...tests], { cwd: root, stdio: "inherit", env: { ...process.env, CRIOGEN_ENGINE: "wasm" } });
process.exit(result.status ?? 1);
