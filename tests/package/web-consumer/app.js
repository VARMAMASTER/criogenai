// A browser app using the installed package as a user would (AGENTS.md rule 9,
// design 0021): `import … from "criogen/web"`, bundled by esbuild, the engine
// file served next to the bundle. Public API only.
import { Agent, FakeReply, FunctionModel, Workflow, END, isCriogenError, ready, tool } from "criogen/web";

async function main() {
  const report = await ready();
  if (!report.ok) throw new Error(`not ready: ${JSON.stringify(report)}`);

  const add = tool({
    name: "add",
    description: "Adds two numbers.",
    input: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] },
    run: ({ a, b }) => a + b,
  });
  const model = new FunctionModel((call) => {
    const result = call.messages.find((m) => m.role === "tool");
    return result ? `the answer is ${result.content[0].value}` : FakeReply.toolCall("add", { a: 19, b: 23 });
  });
  const answer = await new Agent({ model, tools: [add] }).run("19 + 23?");

  const flow = new Workflow()
    .node("double", (s) => ({ n: s.n * 2 }))
    .start("double")
    .edge("double", END)
    .compile();
  const doubled = await flow.run({ n: 21 });

  let refused = false;
  try {
    new Agent({ model, storage: "sqlite://app.db" });
  } catch (error) {
    refused = isCriogenError(error, "unsupported");
  }
  return { ok: true, engine: report.engine, text: answer.text, toolCalls: answer.toolCalls, doubled: doubled.state.n, refused };
}

main().then(
  (result) => (window.__result = result),
  (error) => (window.__result = { ok: false, error: String(error?.stack ?? error) }),
);
