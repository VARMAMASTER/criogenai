type Error = Box<dyn std::error::Error>;
use criogenai_agent::Agent;

pub async fn evals(agent: Agent) -> Result<(), Error> {
    // [show]
    use std::time::Duration;

    use criogenai_agent::evals::{self, Check, Dataset, EvalOptions, EvalResult};

    let data = Dataset::load("cases.jsonl")?; // {"input": "...", "expected": "..."} per line
    let result = evals::evaluate(
        agent,
        &data,
        vec![
            evals::contains(),                     // the answer mentions what was expected
            evals::tool_called("lookup_order", 1), // and the agent looked it up
            evals::max_latency(Duration::from_secs(5)),
        ],
        EvalOptions::default().concurrency(8),
    )
    .await?;
    println!("{}", result.report());
    result.save("eval.json", true)?;

    // In CI: fail when quality drops against the last good run.
    result.check(
        &Check::new()
            .min_pass_rate(0.9)
            .baseline(EvalResult::load("baseline.json")?),
    )?;
    // [/show]
    Ok(())
}
