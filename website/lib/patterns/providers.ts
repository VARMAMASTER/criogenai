// The providers a visitor can use with their own key. The engine in the
// browser (live.ts) calls the provider's API straight from the visitor's tab:
// nothing goes through this site. The key lives only in React state
// (components/playground/KeyProvider.tsx): no storage, no logs, no URLs.

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
