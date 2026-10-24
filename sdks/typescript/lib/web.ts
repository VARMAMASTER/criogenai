// Internet tools for agents: web search (Tavily or Brave) and reading a page
// (design 0019).
//
//   import { Agent, web } from "@agentbuild/sdk";
//   const agent = new Agent({ model: "openai:gpt-5-mini", tools: [web.search(), web.fetch()] });
//
// `web.search()` needs no arguments: it uses TAVILY_API_KEY if set, else
// BRAVE_API_KEY, and says what to set if neither is. The key is read from the
// environment (or passed in), sent only in a request header over https, and
// never appears in an error, a log or a tool result.
//
// Results are data for the model, so they are bounded: a few results, short
// snippets, pages cut to a fixed size. `fetch` refuses addresses on private
// networks (a model must not reach your internal services through it), does
// not follow redirects, and reads at most 500 KB.

import { lookup } from "node:dns/promises";
import { isIP } from "node:net";
import { Tool, ToolError, tool } from "./tools";

export const TAVILY_URL = "https://api.tavily.com";
export const BRAVE_URL = "https://api.search.brave.com";
const MAX_RESULTS = 10;
const SNIPPET_CHARS = 600;
const MAX_BODY = 1_000_000; // bytes read from a search API
const MAX_PAGE_BYTES = 500_000;
const MAX_PAGE_CHARS = 8_000;
const TIMEOUT_MS = 20_000;

function key(explicit: string | undefined, env: string, provider: string): string {
  const value = explicit ?? process.env[env];
  if (!value) throw new ToolError(`${provider} is not set up: set the ${env} environment variable (or pass apiKey)`, { retryable: false });
  return value;
}

/** Credentials only travel over https (plain http only to this computer, for tests). */
function checkUrl(url: string): string {
  const parts = new URL(url);
  if (parts.protocol === "https:") return url;
  if (parts.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(parts.hostname)) return url;
  throw new TypeError(`"${url}": search APIs are called over https only`);
}

function trim(text: unknown, n = SNIPPET_CHARS): string {
  const clean = String(text ?? "").split(/\s+/).filter(Boolean).join(" ");
  return clean.length <= n ? clean : `${clean.slice(0, n - 1)}…`;
}

/** One request: no redirects, a timeout, and at most `limit` bytes read. */
async function request(url: string, init: { method?: string; body?: string; headers: Record<string, string> }, limit: number): Promise<Buffer> {
  let response: Response;
  try {
    response = await fetch(url, { ...init, redirect: "manual", signal: AbortSignal.timeout(TIMEOUT_MS) });
  } catch (error) {
    throw new ToolError(`search failed: ${(error as Error).name}`, { retryable: true });
  }
  // The status is useful to the model; the response body might echo the request, so it is not shown.
  if (response.status >= 300) {
    const retryable = response.status === 408 || response.status === 429 || response.status >= 500;
    throw new ToolError(`search failed: HTTP ${response.status}`, { retryable });
  }
  const chunks: Buffer[] = [];
  let total = 0;
  const reader = response.body?.getReader();
  if (reader) {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.length;
      if (total > limit) {
        await reader.cancel();
        throw new ToolError("search failed: the response was too large", { retryable: false });
      }
      chunks.push(Buffer.from(value));
    }
  }
  return Buffer.concat(chunks);
}

function parseJson(body: Buffer): any {
  try {
    return JSON.parse(body.toString("utf8"));
  } catch {
    throw new ToolError("search failed: the response was not JSON", { retryable: false });
  }
}

function results(items: unknown, title: string, url: string, snippet: string, limit: number): { title: string; url: string; snippet: string }[] {
  const out: { title: string; url: string; snippet: string }[] = [];
  for (const item of Array.isArray(items) ? items : []) {
    if (item && typeof item === "object" && typeof item[url] === "string") {
      out.push({ title: trim(item[title], 200), url: item[url], snippet: trim(item[snippet]) });
    }
    if (out.length >= limit) break;
  }
  return out;
}

const QUERY_SCHEMA = { type: "object", properties: { query: { type: "string" } }, required: ["query"], additionalProperties: false };
const clampResults = (n: number | undefined): number => Math.max(1, Math.min(Math.trunc(n ?? 5), MAX_RESULTS));

/** A `web_search` tool backed by Tavily (TAVILY_API_KEY). `depth`: "basic" or "advanced". */
export function tavilySearch(options: { apiKey?: string; maxResults?: number; depth?: "basic" | "advanced"; baseUrl?: string } = {}): Tool<{ query: string }> {
  const limit = clampResults(options.maxResults);
  const depth = options.depth ?? "basic";
  if (depth !== "basic" && depth !== "advanced") throw new TypeError("depth is 'basic' or 'advanced'");
  const endpoint = checkUrl(`${(options.baseUrl ?? TAVILY_URL).replace(/\/$/, "")}/search`);
  return tool<{ query: string }>({
    name: "web_search",
    description: "Search the web for current information. Returns an answer (when available) and a few results with title, url and snippet.",
    input: QUERY_SCHEMA,
    readOnly: true,
    async run({ query }) {
      const apiKey = key(options.apiKey, "TAVILY_API_KEY", "Tavily");
      const body = await request(
        endpoint,
        {
          method: "POST",
          body: JSON.stringify({ query: query.slice(0, 400), max_results: limit, search_depth: depth, include_answer: true }),
          headers: { "content-type": "application/json", authorization: `Bearer ${apiKey}` },
        },
        MAX_BODY,
      );
      const data = parseJson(body);
      return {
        answer: typeof data?.answer === "string" ? trim(data.answer) : null,
        results: results(data?.results, "title", "url", "content", limit),
      };
    },
  });
}

/** A `web_search` tool backed by Brave Search (BRAVE_API_KEY). */
export function braveSearch(options: { apiKey?: string; maxResults?: number; baseUrl?: string } = {}): Tool<{ query: string }> {
  const limit = clampResults(options.maxResults);
  const base = checkUrl(`${(options.baseUrl ?? BRAVE_URL).replace(/\/$/, "")}/res/v1/web/search`);
  return tool<{ query: string }>({
    name: "web_search",
    description: "Search the web for current information. Returns a few results with title, url and snippet.",
    input: QUERY_SCHEMA,
    readOnly: true,
    async run({ query }) {
      const apiKey = key(options.apiKey, "BRAVE_API_KEY", "Brave Search");
      const url = `${base}?${new URLSearchParams({ q: query.slice(0, 400), count: String(limit) })}`;
      const body = await request(url, { headers: { accept: "application/json", "x-subscription-token": apiKey } }, MAX_BODY);
      return { answer: null, results: results(parseJson(body)?.web?.results, "title", "url", "description", limit) };
    },
  });
}

/**
 * A web search tool with no setup: Tavily if TAVILY_API_KEY is set, else Brave
 * if BRAVE_API_KEY is. `provider: "tavily" | "brave"` chooses explicitly.
 */
export function search(options: { apiKey?: string; provider?: "tavily" | "brave"; maxResults?: number } = {}): Tool<{ query: string }> {
  const chosen = options.provider ?? (process.env.TAVILY_API_KEY ? "tavily" : process.env.BRAVE_API_KEY ? "brave" : undefined);
  if (chosen === undefined && options.apiKey === undefined) {
    throw new Error("web search needs a key: set TAVILY_API_KEY (tavily.com) or BRAVE_API_KEY, or pass provider and apiKey");
  }
  if (chosen === undefined || chosen === "tavily") return tavilySearch(options);
  if (chosen === "brave") return braveSearch(options);
  throw new TypeError("provider is 'tavily' or 'brave'");
}

/** What a researcher gets when you give it no tools: web search if a key is set, else nothing. */
export function defaultTools(): Tool<any>[] {
  try {
    return [search(), fetchPage()];
  } catch {
    return [];
  }
}

// ---------------------------------------------------------------- reading pages

function isPrivateIp(address: string): boolean {
  const v6 = address.toLowerCase();
  // IPv4 inside IPv6, in either spelling (::ffff:127.0.0.1 or, as URL parsers write it, ::ffff:7f00:1).
  const dotted = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/.exec(v6);
  if (dotted) return isPrivateIp(dotted[1]);
  const hex = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(v6);
  if (hex) {
    const hi = parseInt(hex[1], 16);
    const lo = parseInt(hex[2], 16);
    return isPrivateIp(`${hi >> 8}.${hi & 255}.${lo >> 8}.${lo & 255}`);
  }
  // Translation prefixes (NAT64, 6to4) can reach private IPv4 hosts: refused too.
  if (v6.startsWith("64:ff9b:") || v6.startsWith("2002:")) return true;
  if (isIP(address) === 4) {
    const [a, b] = address.split(".").map(Number);
    return (
      a === 0 || a === 10 || a === 127 || a >= 224 || (a === 100 && b >= 64 && b <= 127) || (a === 169 && b === 254) ||
      (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 192 && b === 0) || (a === 198 && (b === 18 || b === 19))
    );
  }
  return v6 === "::" || v6 === "::1" || v6.startsWith("fc") || v6.startsWith("fd") || /^fe[89ab]/.test(v6) || v6.startsWith("ff");
}

async function checkPublic(host: string, allowPrivate: boolean): Promise<void> {
  const bare = host.replace(/^\[|\]$/g, "");
  let addresses: string[];
  try {
    addresses = isIP(bare) ? [bare] : (await lookup(bare, { all: true })).map((a) => a.address);
  } catch {
    throw new ToolError(`could not find the host "${host}"`, { retryable: false });
  }
  if (!allowPrivate && addresses.some(isPrivateIp)) {
    throw new ToolError("that address is on a private network; only public web pages can be read", { retryable: false });
  }
}

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };

function htmlToText(html: string): string {
  const stripped = html
    .replace(/<(script|style|noscript|svg|head|template)\b[\s\S]*?<\/\1\s*>/gi, " ")
    .replace(/<!--[\s\S]*?-->/g, " ")
    .replace(/<\/?(p|br|div|li|h[1-6]|tr)\b[^>]*>/gi, "\n")
    .replace(/<[^>]*>/g, "");
  return stripped
    .replace(/&(#x?[0-9a-f]+|[a-z]+);/gi, (whole, name: string) => {
      if (name[0] === "#") {
        const code = name[1].toLowerCase() === "x" ? parseInt(name.slice(2), 16) : parseInt(name.slice(1), 10);
        return Number.isFinite(code) && code > 0 && code < 0x110000 ? String.fromCodePoint(code) : whole;
      }
      return ENTITIES[name.toLowerCase()] ?? whole;
    })
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .join("\n");
}

/**
 * A `read_page` tool: fetches a public web page and returns its text (cut to
 * `maxChars`). Private-network addresses are refused unless `allowPrivate`
 * (tests, or on purpose).
 */
export function fetchPage(options: { allowPrivate?: boolean; maxChars?: number } = {}): Tool<{ url: string }> {
  const cap = Math.max(200, Math.min(Math.trunc(options.maxChars ?? MAX_PAGE_CHARS), 50_000));
  return tool<{ url: string }>({
    name: "read_page",
    description: "Read a public web page (an http or https URL) and return its text.",
    input: { type: "object", properties: { url: { type: "string" } }, required: ["url"], additionalProperties: false },
    readOnly: true,
    async run({ url }) {
      let parts: URL;
      try {
        parts = new URL(url);
      } catch {
        throw new ToolError("give a full http or https URL", { retryable: false });
      }
      if (parts.protocol !== "http:" && parts.protocol !== "https:") throw new ToolError("give a full http or https URL", { retryable: false });
      await checkPublic(parts.hostname, options.allowPrivate ?? false);
      const body = await request(url, { headers: { "user-agent": "agentbuild-web/0.1", accept: "text/html,text/plain" } }, MAX_PAGE_BYTES);
      const text = htmlToText(body.toString("utf8"));
      return { url, text: text.slice(0, cap), truncated: text.length > cap };
    },
  });
}

export { fetchPage as fetch };
