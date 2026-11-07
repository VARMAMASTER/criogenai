// The evals guide's samples (content/docs/guides/evals.mdx), design 0024.
import { Agent, evals } from "criogen";

export async function basics(agent: Agent) {
  // [show]
  const data = await evals.Dataset.load("cases.jsonl"); // {"input": "...", "expected": "..."} per line
  const result = await evals.evaluate(
    agent,
    data,
    [
      evals.contains(), // the answer mentions what was expected
      evals.toolCalled("lookup_order"), // and the agent looked it up
      evals.maxLatency(5_000),
      evals.judge("openai:gpt-5-mini", "Polite, accurate, and no promises we cannot keep."),
    ],
    { concurrency: 8 },
  );
  console.log(result.report());
  await result.save("eval.json");

  // In CI: fail when quality drops against the last good run.
  result.check({ minPassRate: 0.9, baseline: await evals.EvalResult.load("baseline.json") });
  // [/show]
}
