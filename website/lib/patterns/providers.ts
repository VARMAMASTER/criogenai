// Calls a model provider straight from the visitor's browser, with the key
// they typed. Nothing goes through this site: the request leaves the browser
// for the provider's own API and nowhere else. The key lives only in React
// state (lib/patterns/key.tsx): no storage, no logs, no URLs.

export type ProviderId = "openai" | "anthropic" | "gemini" | "openrouter";

export interface ProviderInfo {
  id: ProviderId;
  label: string;
  model: string;
  keyHint: string;
  keysUrl: string;
}

export const PROVIDERS: ProviderInfo[] = [
  { id: "openai", label: "OpenAI", model: "gpt-5-mini", keyHint: "sk-…", keysUrl: "https://platform.openai.com/api-keys" },
  { id: "anthropic", label: "Anthropic", model: "claude-haiku-5-5", keyHint: "sk-ant-…", keysUrl: "https://console.anthropic.com/settings/keys" },
  { id: "gemini", label: "Google Gemini", model: "gemini-3.6-flash", keyHint: "AIza…", keysUrl: "https://aistudio.google.com/apikey" },
  { id: "openrouter", label: "OpenRouter", model: "openai/gpt-5-mini", keyHint: "sk-or-…", keysUrl: "https://openrouter.ai/keys" },
];

export interface Connection {
  provider: ProviderId;
  key: string;
  model: string;
}

/** Longest reply we ask for: the demos need short answers, and it keeps the visitor's bill small. */
const MAX_TOKENS = 700;

const OPENAI_STYLE: Record<Exclude<ProviderId, "anthropic">, string> = {
  openai: "https://api.openai.com/v1/chat/completions",
  gemini: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
  openrouter: "https://openrouter.ai/api/v1/chat/completions",
};

/** One model call: a system prompt and a user message in, text out. */
export async function complete(conn: Connection, system: string, prompt: string, signal: AbortSignal): Promise<string> {
  let res: Response;
  try {
    res =
      conn.provider === "anthropic"
        ? await fetch("https://api.anthropic.com/v1/messages", {
            method: "POST",
            signal,
            headers: {
              "content-type": "application/json",
              "x-api-key": conn.key,
              "anthropic-version": "2023-06-01",
              // Anthropic asks browser callers to say so: the key is the visitor's own.
              "anthropic-dangerous-direct-browser-access": "true",
            },
            body: JSON.stringify({ model: conn.model, max_tokens: MAX_TOKENS, system, messages: [{ role: "user", content: prompt }] }),
          })
        : await fetch(OPENAI_STYLE[conn.provider], {
            method: "POST",
            signal,
            headers: { "content-type": "application/json", authorization: `Bearer ${conn.key}` },
            body: JSON.stringify({
              model: conn.model,
              messages: [
                { role: "system", content: system },
                { role: "user", content: prompt },
              ],
            }),
          });
  } catch (e) {
    if ((e as Error).name === "AbortError") throw e;
    throw new Error("The browser could not reach the provider (offline, blocked by an extension, or the provider refused a browser call).");
  }
  const body = (await res.json().catch(() => null)) as Record<string, unknown> | null;
  if (!res.ok) throw new Error(`${label(conn.provider)} answered ${res.status}: ${redact(errorText(body), conn.key)}`);
  const text = conn.provider === "anthropic" ? anthropicText(body) : openaiText(body);
  if (!text) throw new Error(`${label(conn.provider)} returned no text.`);
  return text.trim();
}

function label(id: ProviderId): string {
  return PROVIDERS.find((p) => p.id === id)?.label ?? id;
}

function errorText(body: Record<string, unknown> | null): string {
  const err = (Array.isArray(body) ? body[0] : body)?.error as { message?: unknown } | string | undefined;
  const msg = typeof err === "string" ? err : typeof err?.message === "string" ? err.message : "no details";
  return msg.slice(0, 300);
}

/** A provider's error text never shows the key, even if it echoes it. */
function redact(text: string, key: string): string {
  return key.length >= 6 ? text.split(key).join("[your key]") : text;
}

function openaiText(body: Record<string, unknown> | null): string {
  const choice = (body?.choices as { message?: { content?: unknown } }[] | undefined)?.[0];
  const content = choice?.message?.content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map((c: { text?: string }) => c.text ?? "").join("");
  return "";
}

function anthropicText(body: Record<string, unknown> | null): string {
  const blocks = (body?.content as { type?: string; text?: string }[] | undefined) ?? [];
  return blocks
    .filter((b) => b.type === "text")
    .map((b) => b.text ?? "")
    .join("");
}
