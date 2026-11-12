// An agent behind a Next.js route handler (also what runs on Vercel).
// The engine is native: Node runtime only, and kept out of the bundle
// (next.config.mjs). Keys come from server environment variables.
import { Agent, NativeUnavailableError, isCriogenError, kaggle, openai, tool } from "criogenai";

export const runtime = "nodejs";

const lookupOrder = tool({
  name: "lookup_order",
  description: "Look up an order by id.",
  input: { type: "object", properties: { order_id: { type: "string" } }, required: ["order_id"], additionalProperties: false },
  run: async ({ order_id }: { order_id: string }) => ({ order_id, status: "delivered" }), // your data source here
});

// Built on first request, not at import: a system that cannot run the engine
// fails this route with a clear error instead of the whole app.
let agent: Agent | undefined;
function getAgent(): Agent {
  agent ??= new Agent({
    // Kaggle's allowance for prototypes; a paid provider as the fallback.
    model: kaggle("google/gemini-3.5-flash-lite"),
    fallbacks: process.env.OPENAI_API_KEY ? [openai("gpt-5-mini")] : [],
    tools: [lookupOrder],
    limits: { modelCalls: 10, toolCalls: 20, tokens: 50_000 },
    redact: (text) => text.replace(/\b\d{10}\b/g, "[PHONE]"),
  });
  return agent;
}

export async function POST(request: Request) {
  const { question } = (await request.json()) as { question: string };
  try {
    const result = await getAgent().run(question, { signal: AbortSignal.timeout(30_000) });
    return Response.json({
      answer: result.text,
      model: result.answeredBy,
      durationMs: result.durationMs,
      usage: result.usage,
    });
  } catch (error) {
    if (error instanceof NativeUnavailableError) {
      return Response.json({ error: "AI is unavailable on this server", details: error.details }, { status: 503 });
    }
    if (isCriogenError(error)) return Response.json({ error: error.code }, { status: 502 });
    throw error;
  }
}
