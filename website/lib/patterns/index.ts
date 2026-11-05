// Every pattern on the playground, in sidebar order.
import { CASTS } from "./casts";
import type { PatternDef } from "./engine";
import { IBM } from "./ibm";
import { MULTI } from "./multi";
import { REASONING } from "./reasoning";

/** Every pattern runs its code sample on the engine in the browser (casts.ts). */
export const PATTERNS: PatternDef[] = [...REASONING, ...MULTI, ...IBM].map((p) => ({ ...p, engine: CASTS[p.id] ?? p.engine }));

export const GROUPS: { name: PatternDef["group"]; about: string }[] = [
  { name: "Reasoning", about: "One agent, different ways to think." },
  { name: "Multi-agent", about: "Several agents, different ways to share the work." },
  { name: "IBM agent types", about: "The five classic types, from IBM's guide to AI agents." },
];

export function byId(id: string): PatternDef | undefined {
  return PATTERNS.find((p) => p.id === id);
}
