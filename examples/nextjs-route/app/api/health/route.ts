// Health check: the engine loads, and (with ?model=1) the model answers.
import { kaggle, ready } from "criogen";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(request: Request) {
  const checkModel = new URL(request.url).searchParams.get("model") === "1";
  const report = await ready(checkModel ? { model: kaggle("google/gemini-3.5-flash-lite"), timeoutMs: 10_000 } : {});
  return Response.json(report, { status: report.ok ? 200 : 503 });
}
