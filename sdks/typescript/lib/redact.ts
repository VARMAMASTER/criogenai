// `redact`: a function applied to every text in the transcript before each
// model call: the user's input, earlier replies and tool results (text, and
// the strings inside JSON values). It runs first among the hooks, so no
// other hook and no provider sees the text before it. The redacted transcript
// is also what a thread saves. Tool names, call ids and arguments are kept.

import type { Hook, HookContext } from "./hooks";

/**
 * A function that removes what must not reach a model provider, for the
 * agent's `redact` option. It gets each text of the transcript before every
 * model call (the user's input, earlier replies, tool results and the
 * strings inside JSON results) and returns the text to keep; it may be async.
 *
 * It runs before every hook, and the redacted transcript is what a thread
 * saves. Tool names, call ids and arguments are not passed through it.
 *
 * @example
 * import { Agent, type Redactor } from "criogen";
 *
 * // Replace ids shaped like 123-45-6789 before anything leaves the process.
 * const redact: Redactor = (text) => text.replace(/\b\d{3}-\d{2}-\d{4}\b/g, "[id]");
 * const agent = new Agent({ model: "openai:gpt-5-mini", redact });
 */
export type Redactor = (text: string) => string | Promise<string>;

async function redactValue(value: unknown, redact: Redactor): Promise<unknown> {
  if (typeof value === "string") return redact(value);
  if (Array.isArray(value)) return Promise.all(value.map((v) => redactValue(v, redact)));
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value)) out[k] = await redactValue(v, redact);
    return out;
  }
  return value;
}

async function redactBlock(block: Record<string, unknown>, redact: Redactor): Promise<Record<string, unknown>> {
  const out = { ...block };
  if (typeof out.text === "string") out.text = await redact(out.text);
  if ("value" in out) out.value = await redactValue(out.value, redact);
  if (Array.isArray(out.content)) {
    out.content = await Promise.all(
      out.content.map((c) => (c && typeof c === "object" ? redactBlock(c as Record<string, unknown>, redact) : c)),
    );
  }
  return out;
}

/** The hook behind the agent's `redact` option. */
export function redactionHook(redact: Redactor): Hook {
  return {
    async beforeModel(ctx: HookContext) {
      let changed = false;
      const messages = await Promise.all(
        ctx.messages.map(async (message) => {
          if (!Array.isArray(message.content)) return message;
          const content = await Promise.all(
            (message.content as Record<string, unknown>[]).map((b) => redactBlock(b, redact)),
          );
          if (JSON.stringify(content) !== JSON.stringify(message.content)) changed = true;
          return { ...message, content };
        }),
      );
      return changed ? { messages } : undefined;
    },
  };
}
