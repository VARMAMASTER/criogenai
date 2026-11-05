// In the browser bundle, in place of ../web (web search and page reading):
// those tools run on servers. A search key in a page is visible to every
// visitor, search APIs refuse browser calls (CORS), and reading pages safely
// needs the server's view of addresses (no private networks).

import { errorFor } from "../errors";
import type { Tool } from "../tools";

export const TAVILY_URL = "https://api.tavily.com";
export const BRAVE_URL = "https://api.search.brave.com";

function serverOnly(): never {
  throw errorFor(
    "unsupported",
    "web tools (search, reading pages) run on servers, not in the browser: give the agent your own tools, or run it on a server",
  );
}

export function tavilySearch(_options: Record<string, unknown> = {}): Tool<{ query: string }> {
  return serverOnly();
}

export function braveSearch(_options: Record<string, unknown> = {}): Tool<{ query: string }> {
  return serverOnly();
}

export function search(_options: Record<string, unknown> = {}): Tool<{ query: string }> {
  return serverOnly();
}

export function defaultTools(): Tool<any>[] {
  return serverOnly();
}

export function fetchPage(_options: Record<string, unknown> = {}): Tool<{ url: string }> {
  return serverOnly();
}

export { fetchPage as fetch };
