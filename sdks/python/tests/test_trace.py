"""The local trace viewer (design 0023): an agent's runs read back from it."""

import asyncio
import json
import urllib.request

from criogen import Agent, FakeModel, FakeReply, Telemetry, tool, trace


@tool
def weather(city: str) -> str:
    """Today's forecast."""
    return f"{city}: sunny"


def get(url):
    with urllib.request.urlopen(url, timeout=10) as r:
        return json.loads(r.read())


def test_runs_sent_with_telemetry_local_show_up_in_the_viewer():
    async def main():
        async with trace.serve(0, db=":memory:", prices={"fake": {"input": 1, "output": 2}}) as viewer:
            assert viewer.url.startswith("http://127.0.0.1:")
            telemetry = Telemetry.local(url=viewer.url)
            agent = Agent(
                FakeModel([FakeReply.tool_call("weather", {"city": "Lisbon"}), "Sunny."]),
                tools=[weather],
                telemetry=telemetry,
            )
            await agent.run("Weather in Lisbon?")
            await telemetry.flush()
            runs = (await asyncio.to_thread(get, viewer.url + "/api/runs"))["runs"]
            assert len(runs) == 1
            assert runs[0]["model_calls"] == 2 and runs[0]["tool_calls"] == 1
            assert runs[0]["cost"] is not None
            detail = await asyncio.to_thread(get, viewer.url + "/api/runs/" + runs[0]["id"])
            assert {"run", "model", "tool"} <= {s["kind"] for s in detail["spans"]}
            await telemetry.shutdown()

    asyncio.run(main())


def test_another_address_needs_a_token():
    try:
        trace.serve(0, host="0.0.0.0", db=":memory:")
    except Exception as error:  # noqa: BLE001
        assert "token" in str(error)
    else:
        raise AssertionError("served on every address without a token")
