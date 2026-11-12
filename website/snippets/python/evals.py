from criogenai import Agent


async def main(agent: Agent) -> None:
    # [show]
    from criogenai import evals

    data = evals.Dataset.load("cases.jsonl")  # {"input": "...", "expected": "..."} per line
    result = await evals.evaluate(
        agent,
        data,
        [
            evals.contains(),  # the answer mentions what was expected
            evals.tool_called("lookup_order"),  # and the agent looked it up
            evals.max_latency(5_000),
            evals.judge("openai:gpt-5-mini", "Polite, accurate, and no promises we cannot keep."),
        ],
        concurrency=8,
    )
    print(result.report())
    result.save("eval.json")

    # In CI: fail when quality drops against the last good run.
    result.check(min_pass_rate=0.9, baseline=evals.EvalResult.load("baseline.json"))
    # [/show]
