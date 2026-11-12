// Docs: guides/deploying (lib/agent.ts in a Next.js app).
// [show]
// lib/agent.ts
import { Storage, type Agent } from "criogenai";
import { buildAgent } from "@/lib/support_agent";

// One Storage and one Agent per process, made on first use and shared by every
// request. (Next.js reloads modules in development: keep them on globalThis.)
const cache = globalThis as typeof globalThis & { supportAgent?: Agent };

export function supportAgent(): Agent {
  cache.supportAgent ??= buildAgent(
    process.env.MODEL ?? "openai:gpt-5-mini", // the key comes from OPENAI_API_KEY
    new Storage(process.env.DATABASE_URL ?? "sqlite://support.db"),
  );
  return cache.supportAgent;
}
// [/show]
