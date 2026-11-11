// Docs: guides/deploying (a Next.js route handler).
import { supportAgent } from "@/lib/agent";

/** Your authentication: who is calling. */
declare function customerOf(request: Request): Promise<string>;

// [show]
// app/api/tickets/[id]/messages/route.ts
import { CriogenError } from "criogen";

export const runtime = "nodejs"; // the engine is native code: not the Edge runtime

export async function POST(request: Request, { params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const { text } = (await request.json()) as { text: string };
  const customerId = await customerOf(request); // from your session, never from the body
  try {
    const reply = await supportAgent()
      .thread(id, { actor: `customer:${customerId}` })
      .send(text, { state: { customer_id: customerId }, signal: request.signal });
    return Response.json({ text: reply.text, needsApproval: reply.needsApproval });
  } catch (error) {
    if (error instanceof CriogenError) {
      const status = error.code === "thread_busy" || error.code === "thread_suspended" ? 409 : 502;
      return Response.json({ error: error.code }, { status }); // the code, never the message
    }
    throw error;
  }
}
// [/show]
