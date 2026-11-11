// Docs: guides/deploying (Server-Sent Events from a Next.js route handler).
import { supportAgent } from "@/lib/agent";

declare function customerOf(request: Request): Promise<string>;

// [show]
// app/api/tickets/[id]/stream/route.ts
import { CriogenError } from "criogen";

export const runtime = "nodejs";

export async function POST(request: Request, { params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const { text } = (await request.json()) as { text: string };
  const customerId = await customerOf(request);
  const stream = supportAgent()
    .thread(id, { actor: `customer:${customerId}` })
    .stream(text, { state: { customer_id: customerId } });

  const encoder = new TextEncoder();
  const body = new ReadableStream({
    async start(controller) {
      const send = (event: string, data: unknown) =>
        controller.enqueue(encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`));
      try {
        for await (const event of stream) {
          if (event.type === "text") send("text", { text: event.text });
          else if (event.type === "tool_call_started") send("tool", { tool: event.tool });
        }
        const reply = await stream.reply();
        send("done", { needsApproval: reply.needsApproval });
      } catch (error) {
        send("error", { code: error instanceof CriogenError ? error.code : "internal" });
      } finally {
        controller.close();
      }
    },
    cancel() {
      stream.cancel(); // the browser went away: stop the run (it is saved; retry continues it)
    },
  });
  return new Response(body, {
    headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
  });
}
// [/show]
