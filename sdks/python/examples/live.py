"""Live check of the Python SDK against real models (design 0011 §5).
Manual only: needs API keys (OPENAI_API_KEY / GEMINI_API_KEY, or KEY=value
lines in ~/.criogen-keys.env) and costs a few cents. Never run in CI.

    python sdks/python/examples/live.py [openai] [gemini]
"""

import asyncio
import os
import sys
import time

from criogenai import Agent, Gemini, OpenAI, Storage, tool


def load_keys():
    path = os.environ.get("CRIOGEN_KEYS_FILE", os.path.expanduser("~/.criogen-keys.env"))
    if os.path.exists(path):
        for line in open(path, encoding="utf-8"):
            key, sep, value = line.strip().partition("=")
            if sep and key and value and key not in os.environ:
                os.environ[key] = value


weather_calls = []


@tool
async def weather(city: str) -> dict:
    """Current temperature in a city, in Celsius."""
    weather_calls.append(city)
    return {"city": city, "celsius": 17}


refunds = []


@tool(destructive=True)
def refund(order_id: str) -> str:
    """Refunds an order."""
    refunds.append(order_id)
    return f"refunded {order_id}"


async def check(name, model):
    failures = 0
    weather_calls.clear()
    refunds.clear()

    async def case(label, coroutine):
        nonlocal failures
        started = time.time()
        try:
            detail = await coroutine
            print(f"PASS {label:<38} {time.time() - started:5.1f}s  {detail}")
        except Exception as error:  # report and go on
            failures += 1
            print(f"FAIL {label:<38} {time.time() - started:5.1f}s  {type(error).__name__}: {error}")

    async def tools():
        agent = Agent(model, tools=[weather], instructions="Use tools when they help. Be brief.")
        result = await agent.run("What is the temperature in Paris right now?")
        assert weather_calls, "the tool was not called"
        assert "17" in (result.text or ""), result.text
        return result.text[:80]

    async def conversation():
        agent = Agent(model, storage=Storage())
        chat = agent.thread("live")
        await chat.send("Hi! My name is Ada and my favourite colour is teal.")
        reply = await chat.send("What is my name and favourite colour?")
        text = (reply.text or "").lower()
        assert "ada" in text and "teal" in text, text
        return text[:80]

    async def approval():
        agent = Agent(model, tools=[refund], approve=["refund"], storage=Storage(audit=True))
        chat = agent.thread("refunds", actor="user:live")
        reply = await chat.send("Please refund order A-1001 now.")
        assert reply.needs_approval, reply.text
        assert not refunds
        reply = await chat.approve()
        assert refunds == ["A-1001"], refunds
        return (reply.text or "")[:80]

    async def streaming():
        agent = Agent(model)
        stream = agent.thread("stream").stream("Count from 1 to 5, separated by spaces.")
        pieces = [e.text async for e in stream if e.type == "text"]
        reply = await stream.reply()
        assert "".join(pieces).strip() == (reply.text or "").strip()
        return f"{len(pieces)} pieces"

    print(f"\n=== python: {name} ===")
    await case("tools: async Python tool", tools())
    await case("threads: memory across turns", conversation())
    await case("approval: approve runs the tool", approval())
    await case("stream: text in pieces", streaming())
    print(f"--- {name}: {failures} failed")
    return failures


async def main():
    load_keys()
    wanted = sys.argv[1:]
    models = []
    if os.environ.get("OPENAI_API_KEY"):
        models.append(("openai", OpenAI(os.environ.get("OPENAI_MODEL", "gpt-5-mini"))))
    if os.environ.get("GEMINI_API_KEY"):
        models.append(("gemini", Gemini(os.environ.get("GEMINI_MODEL", "gemini-2.5-flash"))))
    models = [m for m in models if not wanted or m[0] in wanted]
    if not models:
        sys.exit("no provider: set OPENAI_API_KEY and/or GEMINI_API_KEY")
    failed = 0
    for name, model in models:
        failed += await check(name, model)
    sys.exit(1 if failed else 0)


asyncio.run(main())
