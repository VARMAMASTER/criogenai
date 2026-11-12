// [show]
import { Agent, Telemetry } from "criogenai";

// LANGFUSE_PUBLIC_KEY, LANGFUSE_SECRET_KEY, LANGFUSE_BASE_URL
const langfuse = Telemetry.langfuse();
// Or any OpenTelemetry collector:
const otel = Telemetry.otlp("http://collector:4318/v1/traces");
// Prompts and answers stay out unless you ask: { captureContent: true }.

const agent = new Agent({ model: "openai:gpt-5-mini", telemetry: langfuse }); // every run is a trace
await agent.run("Where is order A-1001?", { state: { user_id: "u-42" } });
await langfuse.flush(); // before the script ends
// [/show]

export async function local() {
  // [show local]
  // In a terminal: npx criogenai trace   (then open http://127.0.0.1:4318)
  const traces = Telemetry.local(); // { captureContent: true } to see prompts and outputs too
  const agent = new Agent({ model: "openai:gpt-5-mini", telemetry: traces });
  await agent.run("Where is order A-1001?");
  await traces.flush();
  // [/show local]
}
