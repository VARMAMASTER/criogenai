// The answer as it is written: Server-Sent Events from a Next.js route handler.
// Each event is one line `data: {...}`: text pieces, tool calls as they start
// and finish, then `done` with the run facts (or `error` with a stable code).
// The browser side is in app/useAgentStream.ts. Node runtime only.
import { Agent, isCriogenError, kaggle, openai, tool } from "criogenai";

export const runtime = "nodejs";

const lookupOrder = tool({
  name: "lookup_order",
  description: "Look up an order by id.",
  input: { type: "object", properties: { order_id: { type: "string" } }, required: ["order_id"], additionalProperties: false },
  run: async ({ order_id }: { order_id: string }) => ({ order_id, status: "delivered" }), // your data source here
});

let agent: Agent | undefined;
function getAgent(): Agent {
  agent ??= new Agent({
    model: kaggle("google/gemini-3.5-flash-lite"),
    fallbacks: process.env.OPENAI_API_KEY ? [openai("gpt-5-mini")] : [],
    tools: [lookupOrder],
    limits: { modelCalls: 10, toolCalls: 20, tokens: 50_000 },
    redact: (text) => text.replace(/\b\d{10}\b/g, "[PHONE]"),
  });
  return agent;
}

export async function POST(request: Request) {
  const { question, conversationId } = (await request.json()) as { question: string; conversationId?: string };
  const encoder = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    async start(controller) {
      const send = (event: Record<string, unknown>) => controller.enqueue(encoder.encode(`data: ${JSON.stringify(event)}\n\n`));
      try {
        // A thread keeps the conversation (default storage: in memory; pass
        // Storage with a database URL to keep it across restarts).
        const stream = getAgent().thread(conversationId ?? crypto.randomUUID()).stream(question);
        for await (const event of stream) {
          // The browser went away: leaving the loop cancels the run.
          if (request.signal.aborted) break;
          if (event.type === "text") send({ type: "text", text: event.text });
          else if (event.type === "tool_call_started") send({ type: "tool", tool: event.tool, state: "started" });
          else if (event.type === "tool_call_finished") send({ type: "tool", tool: event.tool, state: event.status });
        }
        if (!request.signal.aborted) {
          const reply = await stream.reply();
          send({ type: "done", model: reply.answeredBy, durationMs: reply.durationMs, usage: reply.usage });
        }
      } catch (error) {
        // Codes only: error messages can quote what the model or a tool saw.
        send({ type: "error", code: isCriogenError(error) ? error.code : "internal" });
      } finally {
        controller.close();
      }
    },
  });
  return new Response(body, {
    headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache, no-transform", Connection: "keep-alive" },
  });
}
