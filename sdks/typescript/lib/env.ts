// API keys from the environment, and a .env file to keep several in.
//
// Each provider reads its key from its usual variable, so you can keep all of
// them side by side and switch models by changing a string:
//
//   OPENAI_API_KEY      "openai:gpt-5-mini"
//   ANTHROPIC_API_KEY   "anthropic:claude-sonnet-5-5"
//   GEMINI_API_KEY      "gemini:gemini-2.5-flash"
//   OPENROUTER_API_KEY  "openrouter:meta-llama/llama-3.3-70b-instruct"
//   DEEPSEEK_API_KEY    "deepseek:deepseek-chat"
//   GROQ_API_KEY        "groq:llama-3.3-70b-versatile"
//
// and the same for mistral, xai, cerebras, together, fireworks, qwen
// (DASHSCOPE_API_KEY), moonshot and zai (keyName(provider) gives the variable).
//
// loadEnv() reads a .env file into process.env (call it before you build a
// model). It never prints or returns a value, and never replaces a variable
// that is already set unless you ask.

import * as fs from "node:fs";
import { KEYS } from "./models";

const MAX_BYTES = 1_000_000;
const NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

/**
 * Parse .env text: NAME=value lines, `export NAME=value`, # comments, and
 * values in single or double quotes. No variable expansion. A malformed line
 * throws an Error naming the line number (never its content: it may be a secret).
 *
 * It does not touch `process.env`; `loadEnv` does.
 *
 * @param text The content of a .env file.
 * @returns The variables, by name.
 * @throws {Error} A line is not `NAME=value`, or a quoted value is not closed.
 * @example
 * import { parseEnv } from "criogenai";
 *
 * parseEnv("# keys\nOPENAI_API_KEY=sk-test\nexport GEMINI_API_KEY='g-test'\n");
 * // { OPENAI_API_KEY: "sk-test", GEMINI_API_KEY: "g-test" }
 */
export function parseEnv(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  text.split(/\r?\n/).forEach((raw, index) => {
    const number = index + 1;
    let line = raw.trim();
    if (!line || line.startsWith("#")) return;
    if (line.startsWith("export ") || line.startsWith("export\t")) line = line.slice(7).trimStart();
    const eq = line.indexOf("=");
    const name = eq === -1 ? "" : line.slice(0, eq).trim();
    if (eq === -1 || !NAME.test(name)) throw new Error(`.env line ${number}: expected NAME=value`);
    let value = line.slice(eq + 1).trim();
    const quote = value[0];
    if (quote === "'" || quote === '"') {
      const end = value.indexOf(quote, 1);
      if (end === -1) throw new Error(`.env line ${number}: a quoted value is not closed`);
      const rest = value.slice(end + 1).trim();
      if (rest && !rest.startsWith("#")) throw new Error(`.env line ${number}: unexpected text after a quoted value`);
      value = value.slice(1, end);
    } else {
      const hash = value.indexOf(" #");
      if (hash !== -1) value = value.slice(0, hash).trimEnd();
    }
    out[name] = value;
  });
  return out;
}

/** Options of `loadEnv`. */
export interface LoadEnvOptions {
  /** Replace variables that are already set (default: keep them). */
  override?: boolean;
  /** Throw if the file is missing (default: a missing file is fine). */
  required?: boolean;
}

/**
 * Read a .env file into process.env and return the NAMES it set (sorted;
 * never the values). Files over 1 MB are refused. Keep the file out of git.
 *
 * Call it before you build a model: model objects and "provider:model"
 * strings read their key from the environment when they are created.
 * Node only (a browser has no environment variables).
 *
 * @param path The file to read, relative to the current directory.
 * @param options `override` replaces variables already set; `required` makes a missing file an error.
 * @returns The names of the variables it set, sorted.
 * @throws {Error} The file is over 1 MB, a line is malformed, or (with `required`) the file is missing.
 * @example
 * import { Agent, loadEnv } from "criogenai";
 *
 * loadEnv(); // reads ./.env, e.g. OPENAI_API_KEY=...
 * const agent = new Agent({ model: "openai:gpt-5-mini" });
 */
export function loadEnv(path = ".env", options: LoadEnvOptions = {}): string[] {
  let size: number;
  try {
    size = fs.statSync(path).size;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT" && !options.required) return [];
    throw error;
  }
  if (size > MAX_BYTES) throw new Error(`${path} is larger than ${MAX_BYTES} bytes; not an environment file`);
  const text = fs.readFileSync(path, "utf8").replace(/^﻿/, "");
  const loaded: string[] = [];
  for (const [name, value] of Object.entries(parseEnv(text))) {
    if (options.override || process.env[name] === undefined) {
      process.env[name] = value;
      loaded.push(name);
    }
  }
  return loaded.sort();
}

/**
 * Which providers have a key set right now, by provider name.
 *
 * Names and yes/no only, never a key: `{ openai: true, gemini: false, ... }`.
 * Local providers (ollama, lmstudio) need no key and are not listed.
 */
export function envStatus(): Record<string, boolean> {
  return Object.fromEntries(Object.entries(KEYS).map(([provider, name]) => [provider, Boolean(process.env[name])]));
}

/**
 * The environment variable a provider's key comes from ("openai" -> "OPENAI_API_KEY").
 * @param provider A provider name, e.g. "openai" or "qwen" (local providers such as "ollama" need no key and are not known here).
 * @throws {Error} The provider is not one with a key.
 */
export function keyName(provider: string): string {
  const name = KEYS[provider];
  if (!name) throw new Error(`unknown provider "${provider}"; known: ${Object.keys(KEYS).sort().join(", ")}`);
  return name;
}
