// Internet tools for agents: web search (Tavily or Brave) and reading a page
// (design 0019).
//
//     import { Agent, web } from "criogenai";
//     const agent = new Agent({ model: "openai:gpt-5-mini", tools: [web.search(), web.fetch()] });
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
import { request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";
import { isIP } from "node:net";
import { Tool, ToolError, tool } from "./tools";

/** Tavily's API, the default `baseUrl` of `tavilySearch`. */
export const TAVILY_URL = "https://api.tavily.com";
/** Brave Search's API, the default `baseUrl` of `braveSearch`. */
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

/**
 * A `web_search` tool backed by Tavily (TAVILY_API_KEY). `depth`: "basic" or "advanced".
 *
 * The model gives a `query` (cut to 400 characters) and gets
 * `{ answer, results: [{ title, url, snippet }] }`, with snippets cut to 600
 * characters. The key is read when the tool runs; without one the call fails
 * with a `ToolError` that says what to set.
 *
 * @param options `apiKey` (default: the `TAVILY_API_KEY` environment variable), `maxResults` (1 to 10, default 5), `depth` (default "basic"), `baseUrl` (default `TAVILY_URL`; https only, plain http only to this computer).
 * @throws {TypeError} `depth` is not "basic" or "advanced", or `baseUrl` is not https.
 *
 * @example
 * import { Agent, web } from "criogenai";
 *
 * // Needs TAVILY_API_KEY and OPENAI_API_KEY.
 * const agent = new Agent({ model: "openai:gpt-5-mini", tools: [web.tavilySearch({ maxResults: 3, depth: "advanced" })] });
 * console.log((await agent.run("What changed in the latest Node.js release?")).text);
 */
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

/**
 * A `web_search` tool backed by Brave Search (BRAVE_API_KEY).
 *
 * The model gives a `query` (cut to 400 characters) and gets
 * `{ answer: null, results: [{ title, url, snippet }] }`.
 *
 * @param options `apiKey` (default: the `BRAVE_API_KEY` environment variable), `maxResults` (1 to 10, default 5), `baseUrl` (default `BRAVE_URL`; https only, plain http only to this computer).
 * @throws {TypeError} `baseUrl` is not https.
 */
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
 *
 * @param options `provider`, `apiKey` (with no provider, a key given here is used for Tavily) and `maxResults` (1 to 10, default 5).
 * @throws {Error} No provider is chosen, no key is given, and neither environment variable is set.
 * @throws {TypeError} `provider` is neither "tavily" nor "brave".
 *
 * @example
 * import { Agent, web } from "criogenai";
 *
 * // Needs TAVILY_API_KEY or BRAVE_API_KEY, and OPENAI_API_KEY.
 * const agent = new Agent({ model: "openai:gpt-5-mini", tools: [web.search(), web.fetch()] });
 * console.log((await agent.run("Find this week's weather forecast for Lisbon.")).text);
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

/** Refuses a host that resolves to a private address; returns the address to connect to. */
async function checkPublic(host: string, allowPrivate: boolean): Promise<string> {
  const bare = host.replace(/^\[|\]$/g, "");
  let addresses: string[];
  try {
    addresses = isIP(bare) ? [bare] : (await lookup(bare, { all: true })).map((a) => a.address);
  } catch {
    throw new ToolError(`could not find the host "${host}"`, { retryable: false });
  }
  if (!addresses.length) throw new ToolError(`could not find the host "${host}"`, { retryable: false });
  if (!allowPrivate && addresses.some(isPrivateIp)) {
    throw new ToolError("that address is on a private network; only public web pages can be read", { retryable: false });
  }
  return addresses[0];
}

/**
 * A GET that connects only to `address` (the one that was checked), so a
 * second DNS answer cannot send it to a private one (DNS rebinding). TLS still
 * checks the certificate against the host name. No redirects, a time limit,
 * and at most `limit` bytes.
 */
function pinnedGet(url: URL, address: string, headers: Record<string, string>, limit: number): Promise<Buffer> {
  const family = isIP(address);
  const send = url.protocol === "https:" ? httpsRequest : httpRequest;
  return new Promise<Buffer>((resolve, reject) => {
    let settled = false;
    const fail = (error: ToolError) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      req.destroy();
      reject(error);
    };
    const req = send(
      url,
      {
        method: "GET",
        headers,
        lookup: ((_host: string, options: { all?: boolean }, callback: (...args: unknown[]) => void) =>
          options?.all ? callback(null, [{ address, family }]) : callback(null, address, family)) as never,
      },
      (response) => {
        const status = response.statusCode ?? 0;
        if (status >= 300) {
          response.resume();
          const retryable = status === 408 || status === 429 || status >= 500;
          const moved = status < 400 ? " (redirects are not followed)" : "";
          fail(new ToolError(`reading the page failed: HTTP ${status}${moved}`, { retryable }));
          return;
        }
        const chunks: Buffer[] = [];
        let total = 0;
        response.on("data", (chunk: Buffer) => {
          total += chunk.length;
          if (total > limit) fail(new ToolError(`reading the page failed: the response is larger than ${limit / 1000} KB`, { retryable: false }));
          else chunks.push(chunk);
        });
        response.on("end", () => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          resolve(Buffer.concat(chunks));
        });
        response.on("error", (error) => fail(new ToolError(`reading the page failed: ${error.name}`, { retryable: true })));
      },
    );
    const timer = setTimeout(() => fail(new ToolError("reading the page failed: TimeoutError", { retryable: true })), TIMEOUT_MS);
    req.on("error", (error) => fail(new ToolError(`reading the page failed: ${error.name}`, { retryable: true })));
    req.end();
  });
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
 *
 * Also exported as `web.fetch`. The model gives a `url` and gets
 * `{ url, text, truncated }`: the page's text without markup. Redirects are
 * not followed, at most 500 KB is read, and a request times out after 20 s.
 *
 * @param options `allowPrivate` (default false) and `maxChars`, the most characters returned (200 to 50,000, default 8,000).
 *
 * @example
 * import { Agent, web } from "criogenai";
 *
 * // Needs OPENAI_API_KEY; reading the page needs no key.
 * const agent = new Agent({ model: "openai:gpt-5-mini", tools: [web.fetchPage({ maxChars: 4000 })] });
 * console.log((await agent.run("Summarize https://example.com")).text);
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
      const address = await checkPublic(parts.hostname, options.allowPrivate ?? false);
      const body = await pinnedGet(parts, address, { "user-agent": "criogenai-web/0.1", accept: "text/html,text/plain" }, MAX_PAGE_BYTES);
      const text = htmlToText(body.toString("utf8"));
      return { url, text: text.slice(0, cap), truncated: text.length > cap };
    },
  });
}

export { fetchPage as fetch };
