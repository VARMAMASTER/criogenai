"""The feature tour: every feature on the website's status page, run for real.

    <python with the agentbuild SDK> demos/feature-tour/tour.py [out.json]

Each scenario below is ordinary SDK code. The tour runs it (simulated models
only: no network, no keys), records what it printed with `show(...)` and
whether every `check(...)` held, and saves the result with the scenario's own
source. The website's /demo/ page shows that file, so what a visitor reads is
the code that ran and what it produced.

A scenario that fails is recorded as failed (and the tour exits 1): the page
never shows a green tick the code did not earn.
"""

import asyncio
import datetime
import http.server
import inspect
import json
import os
import platform
import sys
import tempfile
import textwrap
import threading
import time
import uuid
from pathlib import Path
from typing import Callable, Optional

import agentbuild
from agentbuild import (
    END,
    Agent,
    Decision,
    Embedder,
    FakeModel,
    FakeReply,
    Gateway,
    Gemini,
    HookAction,
    LimitExceededError,
    Memory,
    OpenAI,
    OpenRouter,
    Retry,
    Storage,
    StorageError,
    Telemetry,
    ThreadSuspendedError,
    ToolContext,
    ToolError,
    ToolResult,
    ValidationError,
    Workflow,
    suspend,
    tool,
)

FEATURES: list = []
_current: dict = {}


def feature(id: str, group: str, title: str, claim: str, live: bool = False):
    """Register a scenario. `claim` is the one sentence it sets out to prove.
    `live` scenarios run only with --live, against a real model."""

    def register(fn: Callable):
        FEATURES.append({"id": id, "group": group, "title": title, "claim": claim, "fn": fn, "live": live})
        return fn

    return register


LIVE: dict = {}  # set by --live: the provider's model, broken model and embedder


def real():
    """The real model of this run."""
    return LIVE["model"]()


def load_keys() -> None:
    """Read provider keys from ~/.agentbuild-keys.env into this process only (never printed)."""
    path = Path.home() / ".agentbuild-keys.env"
    if not path.exists():
        return
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip().removeprefix("export ").strip()
        if line and not line.startswith("#") and "=" in line:
            key, _, value = line.partition("=")
            os.environ.setdefault(key.strip(), value.strip().strip('"').strip("'"))


def _plain(value):
    try:
        json.dumps(value)
        return value
    except TypeError:
        return repr(value)


def show(label: str, value=None) -> None:
    """Record a line of output for the page."""
    _current["steps"].append({"label": label, "value": _plain(value)})


def check(claim: str, condition: bool) -> None:
    """Record a claim and whether it held; a false one fails the scenario."""
    _current["checks"].append({"claim": claim, "ok": bool(condition)})
    if not condition:
        raise AssertionError(claim)


def tool_results(result):
    return [m for m in result.messages if m["role"] == "tool"]


def temp_db(prefix: str) -> str:
    return os.path.join(tempfile.gettempdir(), f"agentbuild-tour-{prefix}-{uuid.uuid4().hex}.db")


def remove_db(path: str) -> None:
    for suffix in ("", "-wal", "-shm"):
        for _ in range(50):
            try:
                if os.path.exists(path + suffix):
                    os.remove(path + suffix)
                break
            except PermissionError:
                time.sleep(0.02)


# ------------------------------------------------------------------ agents


@feature("agents", "Agents", "Agents with tools, limits and typed output",
         "Python functions become tools, the model's calls run concurrently, and the answer can be a checked structure.")
async def agents():
    @tool
    def add(a: int, b: int) -> int:
        """Adds two numbers."""
        return a + b

    @tool
    async def double(n: int, ctx: ToolContext) -> dict:
        """Doubles a number."""
        await asyncio.sleep(0.01)
        return {"value": n * 2, "call_id": ctx.call_id}

    show("schema built from the type hints", add.schema)
    agent = Agent(
        FakeModel([FakeReply.tool_calls([("add", {"a": 2, "b": 3}), ("double", {"n": 4})]), "5 and 8"]),
        tools=[add, double],
    )
    result = await agent.run("go")
    show("answer", result.text)
    show("tool results, in call order", [m["content"][0]["value"] for m in tool_results(result)])
    check("both tools ran and the model got both results", result.tool_calls == 2 and result.text == "5 and 8")

    schema = {
        "type": "object",
        "properties": {"category": {"type": "string"}, "refund": {"type": "boolean"}},
        "required": ["category", "refund"],
        "additionalProperties": False,
    }
    typed = Agent(FakeModel([FakeReply.tool_call("final_output", {"category": "billing", "refund": True})]), output=schema)
    result = await typed.run("I was charged twice")
    show("typed output", result.output)
    check("the output is the checked structure", result.output == {"category": "billing", "refund": True})

    looping = Agent(FakeModel([FakeReply.tool_call("add", {"a": 1, "b": 1})] * 5), tools=[add], limits={"model_calls": 2})
    try:
        await looping.run("loop")
        check("a looping agent is stopped", False)
    except LimitExceededError as error:
        show("a run that loops", f"{error.code}: {error.message}")
        check("the limit stopped it with a stable error code", error.code == "limit_exceeded")


@feature("validation", "Agents", "Arguments are checked before your code runs",
         "A model's malformed call never reaches the tool; the model is told what was wrong and can recover.")
async def validation():
    calls = []

    @tool
    def charge(amount: int) -> str:
        """Charges a card."""
        calls.append(amount)
        return "ok"

    agent = Agent(FakeModel([FakeReply.tool_call("charge", {"amount": "lots"}), "I could not charge that."]), tools=[charge])
    result = await agent.run("charge")
    message = str(tool_results(result)[0]["content"])
    show("what the model was told", message)
    check("the tool function never ran", calls == [])
    check("the model received an 'Invalid arguments' error", "Invalid arguments" in message)


@feature("tool-errors", "Agents", "Tool failures go back to the model, on your terms",
         "A ToolError or an unexpected exception reaches the model by default; you can hide the details or fail the run.")
async def tool_errors():
    @tool
    def lookup(order_id: str) -> str:
        """Looks up an order."""
        if order_id == "missing":
            raise ToolError("No such order", retryable=False)
        raise KeyError(order_id)

    result = await Agent(FakeModel([FakeReply.tool_call("lookup", {"order_id": "missing"}), "Sorry, not found."]), tools=[lookup]).run("where")
    show("ToolError: the model sees", str(tool_results(result)[0]["content"]))
    check("the model got the message and answered", result.text == "Sorry, not found.")

    script = [FakeReply.tool_call("lookup", {"order_id": "x"}), "The order system failed."]
    result = await Agent(FakeModel(script), tools=[lookup]).run("go")
    crash = tool_results(result)[0]
    show("unexpected exception (default)", str(crash["content"]))
    check("the crash is reported with a stable code", crash["error"]["code"] == "tool_failed")

    quiet = await Agent(FakeModel(script), tools=[lookup], on_tool_exception="return_generic").run("go")
    hidden = tool_results(quiet)[0]
    show("return_generic: the model sees", str(hidden["content"]))
    check("the exception text stays out of the model's view", "KeyError" not in str(hidden["content"]))

    try:
        await Agent(FakeModel(script), tools=[lookup], on_tool_exception="fail").run("go")
        check("fail ends the run", False)
    except agentbuild.ToolExecutionError as error:
        show("fail: the run raises", f"{type(error).__name__} (cause: {type(error.__cause__).__name__})")
        check("the original exception is the cause", isinstance(error.__cause__, KeyError))


@feature("agents-compose", "Agents", "Agents as tools of other agents",
         "An agent becomes a tool of another, so you can build teams without a separate orchestration layer.")
async def agents_compose():
    researcher = Agent(FakeModel(["The order shipped on Monday."]), instructions="Find facts.")
    writer = Agent(
        FakeModel([FakeReply.tool_call("research", {"input": "When did A-1 ship?"}), "It shipped on Monday."]),
        tools=[researcher.as_tool("research", "Looks things up")],
    )
    result = await writer.run("When did my order ship?")
    show("what the researcher told the writer", str(tool_results(result)[0]["content"]))
    show("final answer", result.text)
    check("the writer used the researcher's answer", "Monday" in result.text)


# --------------------------------------------------------------- approvals


@feature("approvals", "Human in the loop", "Approve, reject or edit a call before it runs",
         "A risky tool pauses the run in the database; a different process decides later, and the decision is applied exactly.")
async def approvals():
    storage = Storage()
    refunds = []

    @tool(destructive=True)
    def refund(order_id: str, amount: float) -> str:
        """Refunds an order."""
        refunds.append((order_id, amount))
        return "refunded"

    def make(replies):
        return Agent(FakeModel(replies), tools=[refund], approve=["refund"], storage=storage)

    chat = make([FakeReply.tool_call("refund", {"order_id": "A-1", "amount": 24})]).thread("t-1", actor="user:ada")
    reply = await chat.send("Refund A-1")
    show("the run stops and asks", [{"tool": r.tool, "args": r.args} for r in reply.approvals])
    check("the refund has not run", reply.needs_approval and refunds == [])
    try:
        await chat.send("hello?")
        check("a waiting thread refuses new messages", False)
    except ThreadSuspendedError as error:
        show("sending while it waits", error.code)

    other = make(["Refunded."]).thread("t-1", actor="user:boss")  # another process, in real life
    reply = await other.approve()
    show("approved by user:boss", reply.text)
    check("it ran once, after the approval", refunds == [("A-1", 24)])

    # Reject
    agent = make([FakeReply.tool_call("refund", {"order_id": "A-2", "amount": 9}), "I did not refund it."])
    chat = agent.thread("t-2")
    await chat.send("Refund A-2")
    reply = await chat.reject("Policy says no")
    show("rejected: the model is told why", reply.text)
    check("a rejected call never runs", ("A-2", 9) not in refunds)

    # Edit
    agent = make([FakeReply.tool_call("refund", {"order_id": "A-3", "amount": 500}), "Refunded the smaller amount."])
    chat = agent.thread("t-3")
    reply = await chat.send("Refund A-3")
    call_id = reply.approvals[0].call_id
    await chat.decide({call_id: Decision.edit({"order_id": "A-3", "amount": 12})})
    show("edited from 500 to 12; what ran", refunds[-1])
    check("the edited arguments were used", refunds[-1] == ("A-3", 12))


# ------------------------------------------------------------------ threads


@feature("threads", "Conversations", "Saved conversations: history, rewind, delete",
         "Every step is a checkpoint you can list, go back to, and delete; a new agent object continues the same conversation.")
async def threads():
    path = temp_db("threads")
    try:
        first = Agent(FakeModel(["Hello Ada"]), storage=Storage(f"sqlite:///{path}"))
        await first.thread("t").send("I am Ada")
        await first.storage.close()

        storage = Storage(f"sqlite:///{path}")
        second = Agent(FakeModel(["You are Ada"]), storage=storage)  # a new agent and connection
        chat = second.thread("t")
        reply = await chat.send("Who am I?")
        show("messages the model saw", [m["role"] for m in reply.messages])
        check("the second agent continued the conversation from disk", len(reply.messages) == 4)

        history = await chat.history(20)
        show("checkpoints, newest first", [{"status": h["status"]} for h in history])
        rewound = await chat.rewind(history[-1]["id"])
        show("rewind forks from the oldest", rewound["source"])
        check("rewinding creates a fork", rewound["source"] == "fork")
        check("delete reports it deleted something", await chat.delete() is True)
        await storage.close()
    finally:
        remove_db(path)


@feature("crash-recovery", "Conversations", "Crash recovery without repeating finished tools",
         "When a run fails mid-way, retry() continues from the last saved step; tools that finished are not run again.")
async def crash_recovery():
    counts = {"fetch": 0, "send": 0}
    broken = {"on": True}

    @tool
    def fetch(order_id: str) -> str:
        """Fetches an order."""
        counts["fetch"] += 1
        return "order A-1: shipped"

    @tool
    def send(text: str) -> str:
        """Sends an email."""
        counts["send"] += 1
        if broken["on"]:
            raise RuntimeError("the mail server is down")
        return "sent"

    agent = Agent(
        FakeModel([FakeReply.tool_calls([("fetch", {"order_id": "A-1"})]), FakeReply.tool_call("send", {"text": "hi"}), "Done."]),
        tools=[fetch, send],
        storage=Storage(),
        on_tool_exception="fail",
    )
    chat = agent.thread("job")
    try:
        await chat.send("Look it up and email the customer")
        check("the first attempt fails", False)
    except agentbuild.AgentBuildError as error:
        show("first attempt", f"{error.code}: {error.message}")
    show("tool calls so far", dict(counts))
    broken["on"] = False
    reply = await chat.retry()
    show("after retry()", reply.text)
    show("tool calls in total", dict(counts))
    check("the run finished after the retry", reply.text == "Done.")
    check("fetch was not run a second time", counts["fetch"] == 1)


@feature("streaming", "Conversations", "Streaming and cancellation",
         "Text and tool events arrive as they happen; cancelling stops the model call and the thread is still saved.")
async def streaming():
    @tool
    def add(a: int, b: int) -> int:
        """Adds."""
        return a + b

    agent = Agent(FakeModel(["Hello there", FakeReply.tool_call("add", {"a": 1, "b": 2}), "Three."]), tools=[add], approve=["add"])
    chat = agent.thread("s")
    stream = chat.stream("hi")
    events = [e async for e in stream]
    show("events", [e.type for e in events])
    text = "".join(e.text for e in events if e.type == "text")
    check("the streamed text equals the saved reply", text == "Hello there" == (await stream.reply()).text)

    stream = chat.stream("add")
    types = [e.type async for e in stream]
    check("an approval request is a stream event", "approval_needed" in types)
    await stream.reply()

    slow = Agent(FakeModel(["never"], delay=5.0))
    stream = slow.thread("x").stream("hi")
    await asyncio.sleep(0.2)
    started = time.perf_counter()
    stream.cancel()
    try:
        await stream.reply()
    except agentbuild.CancelledError:
        took = time.perf_counter() - started
        show("cancel() on a 5 s model call returned after", f"{took * 1000:.0f} ms")
        check("cancellation is prompt", took < 1.0)


# ------------------------------------------------------------------ storage


@feature("storage", "Storage and compliance", "SQLite, Postgres and in-memory storage",
         "The same agent code saves to a file, to Postgres, or to memory; here SQLite and memory.")
async def storage_backends():
    path = temp_db("storage")
    try:
        storage = Storage(f"sqlite:///{path}")
        agent = Agent(FakeModel(["saved"]), storage=storage)
        await agent.thread("t").send("hello")
        await storage.close()
        size = os.path.getsize(path)
        show("SQLite file written", f"{size // 1024} KB")
        check("the conversation is on disk", size > 0)
    finally:
        remove_db(path)
    memory = Storage()
    reply = await Agent(FakeModel(["kept in memory"]), storage=memory).thread("t").send("hi")
    show("in-memory storage", reply.text)
    show("Postgres", "the same Storage('postgres://…') call; its tests run in CI against a real server (not part of this tour)")


@feature("encryption", "Storage and compliance", "Encryption at rest",
         "Content in the database is encrypted; without the key the database refuses to open.")
async def encryption():
    path = temp_db("enc")
    keys = "k1:" + "ab" * 32
    try:
        agent = Agent(FakeModel(["secret noted"]), storage=Storage(f"sqlite:///{path}", encryption_keys=keys))
        await agent.thread("t").send("my secret is pelican")
        await agent.storage.close()
        raw = Path(path).read_bytes()
        show("bytes in the database file", len(raw))
        check("the secret word is not in the file", b"pelican" not in raw)
        try:
            Storage(f"sqlite:///{path}")
            check("opening without the key fails", False)
        except StorageError as error:
            show("opening without the key", f"{error.code}")
            check("it refuses with a stable code", error.code == "encryption_key_missing")
    finally:
        remove_db(path)


@feature("audit", "Storage and compliance", "A tamper-evident audit trail with no content",
         "Runs, model calls, tool calls and decisions are recorded with who did them, in a hash chain, without message text.")
async def audit():
    storage = Storage(audit=True)

    @tool(destructive=True)
    def refund(order_id: str) -> str:
        """Refunds."""
        return "done"

    agent = Agent(FakeModel([FakeReply.tool_call("refund", {"order_id": "A-1"}), "Refunded."]), tools=[refund], approve=["refund"], storage=storage)
    await agent.thread("visit-1", actor="user:ada").send("My name is Ada Lovelace, refund A-1")
    await agent.thread("visit-1", actor="user:dr-rao").approve()
    records = await storage.audit_records(thread_id="visit-1")
    show("records (action, actor)", [(r["action"], r.get("actor")) for r in records])
    report = await storage.verify_audit()
    show("hash chain verification", report)
    check("the chain verifies", report["ok"] is True)
    check("the actors are recorded", {"user:ada", "user:dr-rao"} <= {r.get("actor") for r in records})
    check("no message content is in any record", "Lovelace" not in json.dumps(records))


@feature("retention", "Storage and compliance", "Retention: old data is deleted on a schedule",
         "keep_checkpoints and thread_ttl bound how much is kept; prune() applies them.")
async def retention():
    path = temp_db("ret")
    try:
        storage = Storage(f"sqlite:///{path}", retention={"keep_checkpoints": 1})
        agent = Agent(FakeModel(["one", "two", "three"]), storage=storage)
        chat = agent.thread("t")
        for text in ("a", "b", "c"):
            await chat.send(text)
        before = len(await chat.history(100))
        report = await storage.prune()
        after = len(await chat.history(100))
        show("checkpoints before and after prune()", [before, after])
        show("report", report)
        check("old checkpoints were deleted", report["checkpoints_deleted"] > 0 and after < before)
        await storage.close()
    finally:
        remove_db(path)


# ------------------------------------------------------------------- memory


@feature("memory", "Memory", "Long-term memory per user",
         "The model can remember, search and forget facts that outlive a conversation; your code can read them too.")
async def memory():
    storage = Storage()
    agent = Agent(
        FakeModel([FakeReply.tool_call("remember", {"key": "diet", "value": "vegan"}), "Noted.", "Eat lentils."]),
        storage=storage,
        memory=Memory.by_state("user_id", "users"),
    )
    await agent.run("I am vegan", user_id="ada")
    found = await storage.search("users/ada")
    show("stored for users/ada", [(f["key"], f["value"]) for f in found])
    check("the model's remember call saved the fact", found[0]["value"] == "vegan")
    await agent.run("What should I eat?", user_id="ada")
    check("a second user has separate memory", await storage.search("users/bob") == [])
    deleted = await storage.forget_all("users/ada")
    show("forget_all (right to erasure)", f"{deleted} fact deleted")
    check("everything about one user can be deleted", deleted == 1)


@feature("semantic-memory", "Memory", "Memory by meaning",
         "Facts are found by meaning with embeddings, and by keywords when the embedder is down.")
async def semantic_memory():
    storage = Storage()
    mem = Memory.shared("facts").semantic(Embedder.fake(synonyms={"automobile": "car"}))
    agent = Agent(
        FakeModel([FakeReply.tool_call("remember", {"key": "vehicle", "value": "The customer drives a red car"}), "Noted.",
                   FakeReply.tool_call("search_memory", {"query": "automobile"}), "Found it."]),
        storage=storage,
        memory=mem,
    )
    await agent.run("Remember my car")
    found = await agent.run("Which automobile do I have?")
    result = str(tool_results(found)[0]["content"])
    show("searching 'automobile' (the fact says 'car')", result)
    check("found with no shared word, by meaning", "red car" in result)
    broken = Agent(
        FakeModel([FakeReply.tool_call("search_memory", {"query": "red"}), "ok"]),
        storage=storage,
        memory=Memory.shared("facts").semantic(Embedder.fake(failing=True)),
    )
    result = str(tool_results(await broken.run("search"))[0]["content"])
    show("embedder down: falls back to keywords", result)
    check("the search still worked", "red car" in result)


# ---------------------------------------------------------------- workflows


@feature("workflows", "Workflows", "Graphs of steps: routes, parallel branches, joins",
         "A graph with a router, two parallel nodes and a join runs on the Rust engine; every graph error is reported at compile time.")
async def workflows():
    wf = Workflow(reducers={"log": "append"})
    wf.add_node("classify", lambda s: {"log": ["classify"]})
    wf.add_node("fan", lambda s: None)
    wf.add_node("search_web", lambda s: {"log": ["web"], "web": 1})
    wf.add_node("search_docs", lambda s: {"log": ["docs"], "docs": 2})
    wf.add_node("answer", lambda s: {"answer": s["web"] + s["docs"]})
    wf.add_node("refuse", lambda s: {"answer": None})
    wf.start("classify")
    wf.route("classify", lambda s: "ok" if s["question"] else "empty", {"ok": "fan", "empty": "refuse"})
    wf.edge("fan", "search_web").edge("fan", "search_docs")
    wf.join(["search_web", "search_docs"], "answer")
    wf.edge("answer", END).edge("refuse", END)
    app = wf.compile()
    result = await app.run({"question": "why?"})
    show("state at the end", result.state)
    show("steps", result.steps)
    check("both parallel branches ran and joined", result.state["answer"] == 3 and sorted(result.state["log"]) == ["classify", "docs", "web"])
    empty = await app.run({"question": ""})
    check("the router sent an empty question elsewhere", empty.state["answer"] is None)

    broken = Workflow()
    broken.add_node("a", lambda s: None)
    broken.start("a").edge("a", "nowhere")
    try:
        broken.compile()
        check("a broken graph is refused", False)
    except agentbuild.AgentBuildError as error:
        show("compile() of a broken graph", error.message)


@feature("durable-workflows", "Workflows", "Workflows that pause for a person, durably",
         "A node suspends the run; the pause lives in the database and any process can resume it.")
async def durable_workflows():
    storage = Storage(audit=True)
    paid = []

    def build():
        wf = Workflow()

        @wf.node
        def approve(state, ctx):
            if ctx.resume is None:
                return suspend({"refund": state["amount"]}, {"status": "waiting"})
            return {"approved": ctx.resume == "yes", "status": "decided"}

        @wf.node
        def pay(state):
            if state["approved"]:
                paid.append(state["amount"])
            return {"paid": state["approved"]}

        wf.start("approve").edge("approve", "pay").edge("pay", END)
        return wf

    thread = build().compile(storage=storage).thread("refund-1", actor="user:ada")
    turn = await thread.run({"amount": 25})
    show("first run", {"suspended": turn.suspended, "state": turn.state})
    show("what it waits for", [i.payload for i in await thread.interrupts()])
    check("it is waiting and nothing was paid", turn.suspended and paid == [])
    other = build().compile(storage=storage).thread("refund-1", actor="user:boss")  # another process, in real life
    turn = await other.resume({"approve": "yes"})
    show("resumed by user:boss", turn.state)
    check("it paid once after the resume", turn.state["paid"] is True and paid == [25])
    records = await storage.audit_records(thread_id="refund-1")
    check("the audit trail names both people", {"user:ada", "user:boss"} <= {r.get("actor") for r in records})


@feature("agent-nodes", "Workflows", "Agents as workflow steps",
         "A node can be a whole agent, so a graph mixes plain functions and agents.")
async def agent_nodes():
    wf = Workflow(reducers={"messages": "upsert_by_id"})
    # An agent node reads and extends the conversation in state["messages"].
    question = {"id": "m1", "role": "user", "content": [{"type": "text", "text": "Refund?"}]}
    wf.add_node("ask", lambda s: {"messages": [question]})
    wf.add_node("agent", Agent(FakeModel(["Refund approved."])))
    wf.start("ask").edge("ask", "agent").edge("agent", END)
    result = await wf.compile().run({})
    show("roles in the conversation", [m["role"] for m in result.state["messages"]])
    show("the agent's reply", result.state["messages"][-1]["content"][0]["text"])
    check("the agent ran as a node and added its reply", [m["role"] for m in result.state["messages"]] == ["user", "assistant"])


# ------------------------------------------------------------------- models


@feature("resilience", "Models", "Retries and fallback models",
         "A transient model error is retried; when a model keeps failing, the next one answers.")
async def resilience():
    result = await Agent(FakeModel(["fine now"], fail_first=1)).run("hello")
    show("one transient failure, retried", result.text)
    check("the retry succeeded", result.text == "fine now")
    agent = Agent(
        FakeModel(["never"], fail_first=100),
        fallbacks=[FakeModel(["from the fallback"])],
        model_retry=Retry(max_attempts=2, initial_backoff=0.01, jitter=False),
    )
    result = await agent.run("hi")
    show("primary keeps failing", result.text)
    check("the fallback model answered", result.text == "from the fallback")


@feature("hooks", "Models", "Hooks around model and tool calls",
         "Your code can change arguments, skip a tool, redact a result, replace a reply or end a run, in a fixed order.")
async def hooks():
    seen = []

    @tool
    def lookup(order_id: str) -> str:
        """Looks up an order."""
        seen.append(order_id)
        return f"order {order_id}: shipped, card 4111-1111"

    @tool
    def weather(city: str) -> str:
        """The weather."""
        raise AssertionError("skipped by the hook")

    class Guard:
        def before_tool(self, call):
            if call.name == "weather":
                return ToolResult("sunny (cached)")  # skip the tool
            call.args["order_id"] = call.args["order_id"].upper()
            return call

        def after_tool(self, call, result):
            return result.replace("4111-1111", "[card]") if isinstance(result, str) else None

        def after_model(self, ctx):
            if "secret" in (ctx.messages[-1]["content"][0].get("text", "") if ctx.messages[-1]["content"] else ""):
                return HookAction(message=dict(ctx.messages[-1], content=[{"type": "text", "text": "[redacted]"}]))
            return None

    agent = Agent(
        FakeModel([FakeReply.tool_calls([("lookup", {"order_id": "a-1"}), ("weather", {"city": "Paris"})]), "The secret answer"]),
        tools=[lookup, weather],
        hooks=[Guard()],
    )
    result = await agent.run("where is it?")
    show("argument the tool received", seen)
    show("what the model saw from the tools", [str(m["content"]) for m in tool_results(result)])
    show("final answer", result.text)
    check("before_tool changed the argument", seen == ["A-1"])
    check("after_tool redacted the card number", "[card]" in str(tool_results(result)[0]["content"]))
    check("a hook skipped a tool with a cached value", "sunny (cached)" in str(tool_results(result)[1]["content"]))
    check("after_model replaced the reply", result.text == "[redacted]")

    class Budget:
        def before_model(self, ctx):
            return HookAction(end=True, output="Stopped by the budget hook.") if ctx.model_calls >= 1 else None

    @tool
    def noop() -> str:
        """Does nothing."""
        return "ok"

    result = await Agent(FakeModel([FakeReply.tool_call("noop", {}), "never"], by_turn=True), tools=[noop], hooks=[Budget()]).run("go")
    show("a hook ended the run", result.output)
    check("the budget hook stopped it", result.output == "Stopped by the budget hook.")


@feature("gateway-sdk", "Models", "Calling a model directly: chat, stream, JSON output, embed",
         "The Gateway class gives retries, fallbacks and schema-checked output without an agent.")
async def gateway_sdk():
    gw = Gateway(fallbacks=[FakeModel(["from the backup"], name="backup")], retry=Retry(initial_backoff=0.01, jitter=False))
    reply = await gw.chat(FakeModel([], fail_first=10, name="primary"), "hi")
    show("primary fails, the backup answers", {"text": reply.text, "model": reply.model})
    check("the reply says which model answered", reply.model == "fake:backup")

    chunks = [c async for c in Gateway().stream(FakeModel(["Hello there."]), "hi")]
    show("stream chunks", [c.text for c in chunks])
    check("chunks add up to the reply", "".join(c.text for c in chunks) == "Hello there.")

    weather = {"type": "object", "properties": {"city": {"type": "string"}, "temp": {"type": "integer"}}, "required": ["city", "temp"]}
    reply = await Gateway().chat(FakeModel(['{"city": "Oslo", "temp": 3}']), "Weather?", output=weather)
    show("JSON output, checked against the schema", reply.output)
    try:
        await Gateway().chat(FakeModel(['{"city": "Oslo"}']), "Weather?", output=weather)
        check("a wrong structure is refused", False)
    except agentbuild.AgentBuildError as error:
        show("a structure that does not match", error.code)
        check("it fails with a stable code", error.code == "output_invalid")


@feature("telemetry", "Observability", "Tracing to OpenTelemetry and Langfuse, without content",
         "Every run is exported as a trace to any OTLP collector; prompts and answers stay out unless you ask.")
async def telemetry():
    received = []

    class Collector(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            received.append(self.rfile.read(int(self.headers["Content-Length"])).decode())
            self.send_response(200)
            self.end_headers()

        def log_message(self, *args):
            pass

    server = http.server.HTTPServer(("127.0.0.1", 0), Collector)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        tel = Telemetry.otlp(f"http://127.0.0.1:{server.server_port}/v1/traces", service_name="feature-tour")
        agent = Agent(FakeModel(["fine"]), telemetry=tel)
        await agent.run("a private question")
        await tel.flush()
        body = "".join(received)
        spans = json.loads(received[0])["resourceSpans"][0]["scopeSpans"][0]["spans"] if received else []
        show("what the collector received", f"{len(body)} bytes, {len(spans)} spans: {[s['name'] for s in spans]}")
        check("a trace reached the collector", "resourceSpans" in body)
        check("the prompt text was left out", "a private question" not in body)
        await tel.shutdown()
    finally:
        server.shutdown()


# ------------------------------------------------ patterns and specialists


@feature("patterns-pipelines", "Patterns and specialists", "Sequential and parallel pipelines in one line",
         "Registered specialists chain with '>' or run side by side with '|'; steps are traced and parallel branches overlap.")
async def patterns_pipelines():
    from agentbuild import patterns

    model = FakeModel(["a step's answer"])
    flow = patterns.quick("researcher > writer", model=model)
    result = await flow.run("solar panels")
    show("researcher > writer", result.explain())
    check("both specialists ran, in order", [t["step"] for t in result.trace] == ["researcher", "writer"])

    slow = {f"analyst{i}": Agent(FakeModel(["done"], delay=0.4)) for i in range(4)}
    started = time.perf_counter()
    result = await patterns.parallel(slow, synthesizer=("summary", Agent(FakeModel(["All four agree."])))).run("review this")
    took = time.perf_counter() - started
    show("four 0.4 s analysts at the same time", f"{took:.2f} s, then: {result.output}")
    check("they overlapped (about 0.4 s, not 1.6 s)", took < 1.2)


@feature("patterns-review-loop", "Patterns and specialists", "Review and critique, with a loop that always ends",
         "A critic sends a draft back until it approves; a cap stops any loop that never converges.")
async def patterns_review_loop():
    from agentbuild import patterns

    writer = Agent(FakeModel(["draft one", "draft two", "draft three"]))
    critic = Agent(FakeModel(["REVISE: add a number", "REVISE: shorter", "APPROVED"]))
    result = await patterns.review_and_critique(writer, critic, max_rounds=5).run("Write a blurb")
    show("three rounds", result.explain())
    check("it ended on approval with the third draft", result.stop_reason == "approved" and result.output == "draft three")

    stubborn = await patterns.review_and_critique(
        Agent(FakeModel(["d1", "d2"])), Agent(FakeModel(["REVISE: a", "REVISE: b"])), max_rounds=2
    ).run("Write a blurb")
    show("a critic that never approves, max_rounds=2", f"{stubborn.stop_reason}, kept the last draft: {stubborn.output}")
    check("the cap ended it and the last draft was kept", stubborn.stop_reason == "max_rounds" and stubborn.output == "d2")
    asked = await patterns.loop([lambda t: t + "!"], until=lambda t, n: False, max_iterations=10_000).run("x")
    show("max_iterations=10000 is clamped to", len(asked.trace))
    check("no loop runs more than the ceiling", len(asked.trace) == patterns.CEILING)


@feature("patterns-routing", "Patterns and specialists", "Coordinator, hierarchy and swarm",
         "A coordinator picks a specialist, specialists can be nested, and a swarm hands work over without a coordinator.")
async def patterns_routing():
    from agentbuild import patterns

    orders = Agent(FakeModel(["Order A-1 shipped."]))
    triage = patterns.coordinator(
        FakeModel([FakeReply.tool_call("orders", {"input": "where is A-1"}), "It shipped."]),
        {"orders": (orders, "order status questions")},
    )
    result = await triage.run("where is A-1")
    show("coordinator called the 'orders' specialist", [m["content"] for m in tool_results(result)])
    check("the coordinator routed and answered", result.text == "It shipped.")

    team = {
        "market": Agent(FakeModel(["Demand is high.\nHANDOFF: engineering"])),
        "engineering": Agent(FakeModel(["Feasible in 3 months.\nHANDOFF: finance"])),
        "finance": Agent(FakeModel(["Budget fits.\nFINAL"])),
    }
    result = await patterns.swarm(team, start="market").run("Should we build it?")
    show("swarm hand-offs", result.explain())
    check("agents handed over until FINAL", result.stop_reason == "final" and len(result.trace) == 3)
    ping = {"a": Agent(FakeModel(["x\nHANDOFF: b"] * 9)), "b": Agent(FakeModel(["y\nHANDOFF: a"] * 9))}
    result = await patterns.swarm(ping, max_handoffs=3).run("go")
    show("two agents that keep passing it back", result.stop_reason)
    check("the hand-off cap ended it", result.stop_reason == "max_handoffs")


@feature("patterns-human-review", "Patterns and specialists", "Human in the loop as a pattern, durable",
         "A step's draft waits for a person; another process approves it later, and the audit trail names both.")
async def patterns_human_review():
    from agentbuild import patterns

    storage = Storage(audit=True)
    first = patterns.human_review(Agent(FakeModel(["Release 120 rows."]))).compile(storage).thread("release-1", actor="user:ada")
    turn = await first.run({"input": "Release the dataset?"})
    show("it waits with the draft", (await first.interrupts())[0].payload)
    check("nothing is released yet", turn.suspended)
    other = patterns.human_review(Agent(FakeModel(["unused"]))).compile(storage).thread("release-1", actor="user:boss")
    done = await other.resume({"review": {"decision": "edit", "text": "Release 100 rows."}})
    show("the reviewer edited it", done.state["output"])
    check("the edit is the output", done.state["output"] == "Release 100 rows.")
    records = await storage.audit_records(thread_id="release-1")
    check("the audit trail names both people", {"user:ada", "user:boss"} <= {r.get("actor") for r in records})


@feature("specialists", "Patterns and specialists", "Specialist agents you can extend",
         "Default recipes for common jobs; extend one, register it, and use it by name anywhere.")
async def specialists_demo():
    from agentbuild import patterns, specialists

    show("built in", specialists.available())
    careful = specialists.researcher.extend("careful-researcher", add="Always give two sources.")
    show("extended instructions end with", careful.instructions.splitlines()[-1])
    check("the original recipe is unchanged", "two sources" not in specialists.researcher.instructions)
    specialists.register(careful, replace=True)
    try:
        flow = patterns.sequential(["careful-researcher", "summarizer"], model=FakeModel(["ok"]))
        result = await flow.run("topic")
        show("used by name", [t["step"] for t in result.trace])
        check("a registered recipe works as a step", [t["step"] for t in result.trace] == ["careful-researcher", "summarizer"])
    finally:
        specialists.unregister("careful-researcher")


@feature("web-search", "Patterns and specialists", "Web search tools (Tavily, Brave) and page reading",
         "web.search() works from an environment key; results are bounded, the key never reaches the model, and private addresses are refused.")
async def web_search_demo():
    from agentbuild import web

    seen = []

    class Fake(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            seen.append((self.headers.get("authorization"), payload))
            body = json.dumps({"answer": "Solar is growing.", "results": [
                {"title": f"Result {i}", "url": f"https://example.com/{i}", "content": "x" * 2000} for i in range(20)]}).encode()
            self.send_response(200)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    server = http.server.HTTPServer(("127.0.0.1", 0), Fake)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        search = web.tavily_search("tvly-demo-key", max_results=3, base_url=f"http://127.0.0.1:{server.server_port}")
        agent = Agent(FakeModel([FakeReply.tool_call("web_search", {"query": "solar panels"}), "Summary."]), tools=[search])
        result = await agent.run("What is new in solar?")
        value = tool_results(result)[0]["content"][0]["value"]
        show("what the model saw", {"answer": value["answer"], "results": len(value["results"]), "first": value["results"][0]["title"]})
        check("20 results were cut to 3, with short snippets", len(value["results"]) == 3 and all(len(r["snippet"]) <= 600 for r in value["results"]))
        check("the key went in a header and nowhere the model can see", seen[0][0] == "Bearer tvly-demo-key" and "tvly-demo-key" not in json.dumps(result.messages))
    finally:
        server.shutdown()
    refused = Agent(FakeModel([FakeReply.tool_call("read_page", {"url": "http://127.0.0.1:1/admin"}), "I could not read it."]), tools=[web.fetch()])
    result = await refused.run("Read http://127.0.0.1:1/admin")
    show("reading a private address", str(tool_results(result)[0]["content"]))
    check("private network addresses are refused", "private network" in str(tool_results(result)[0]["content"]))
    show("with TAVILY_API_KEY or BRAVE_API_KEY set", "web.search() needs no arguments; the researcher specialist picks it up automatically")


# ----------------------------------------------------- with a real model
# The same SDK calls, with a real model deciding what to do. The model's words
# and choices vary, so these scenarios check behaviour (which tools ran, what
# paused, what was saved, what was kept out), never exact wording.

SUPPORT = "You are a support agent for an online shop. Be brief. Use the tools for facts; never guess."


@feature("live-tools", "With a real model", "A real model chooses and calls a tool",
         "Given a question, the model decides to call get_order with the right argument and answers from the result.", live=True)
async def live_tools():
    calls = []

    @tool(read_only=True)
    def get_order(order_id: str) -> dict:
        """Look up an order by its id."""
        calls.append(order_id)
        return {"order_id": order_id, "status": "shipped", "eta": "tomorrow"}

    result = await Agent(real(), instructions=SUPPORT, tools=[get_order]).run("Where is order A-1001?")
    show("the model called get_order with", calls)
    show("answer", result.text)
    show("usage", result.usage)
    check("it looked the order up with the right id", calls == ["A-1001"])
    check("the answer reflects the tool's result", "ship" in (result.text or "").lower())


@feature("live-approval", "With a real model", "A real model asks to refund; a person decides",
         "The model proposes a destructive call; the run pauses before it, and it runs exactly once after approval.", live=True)
async def live_approval():
    refunds = []

    @tool(destructive=True)
    def issue_refund(order_id: str, amount: float) -> str:
        """Refund an order. Amount in dollars."""
        refunds.append((order_id, amount))
        return "refunded"

    storage = Storage()

    def make():
        return Agent(real(), instructions=SUPPORT, tools=[issue_refund], approve=["issue_refund"], storage=storage)

    chat = make().thread("t-1", actor="user:ada")
    reply = await chat.send("Order A-1001 arrived broken. Refund it, 24 dollars.")
    show("the model asked for", [{"tool": r.tool, "args": r.args} for r in reply.approvals])
    check("the run paused before the refund", reply.needs_approval and refunds == [])
    reply = await make().thread("t-1", actor="user:boss").approve()  # another process, in real life
    show("after approval the model said", reply.text)
    check("the refund ran exactly once", len(refunds) == 1 and refunds[0][0] == "A-1001")

    chat = make().thread("t-2")
    await chat.send("Order A-2002 arrived broken. Refund it, 10 dollars.")
    reply = await chat.reject("Our policy does not allow refunds on this order.")
    show("after rejection the model said", reply.text)
    check("a rejected call never ran", len(refunds) == 1)


@feature("live-structured", "With a real model", "A real model's answer as a checked structure",
         "The answer comes back as data that matches your schema, not as text to parse.", live=True)
async def live_structured():
    schema = {
        "type": "object",
        "properties": {"category": {"enum": ["billing", "shipping", "other"]}, "urgent": {"type": "boolean"}},
        "required": ["category", "urgent"],
        "additionalProperties": False,
    }
    result = await Agent(real(), instructions="Classify the customer's message.", output=schema).run(
        "I was charged twice for order A-1001 and need it fixed today!"
    )
    show("output", result.output)
    check("it is a valid structure", set(result.output) == {"category", "urgent"})
    check("it chose an allowed category", result.output["category"] in {"billing", "shipping", "other"})


@feature("live-streaming", "With a real model", "Streaming from a real model",
         "Text arrives in several events as the model writes, and the saved reply equals what was streamed.", live=True)
async def live_streaming():
    stream = Agent(real(), instructions="Answer in about 40 words.").thread("s").stream("Explain what a database index is.")
    texts = []
    async for event in stream:
        if event.type == "text":
            texts.append(event.text)
    reply = await stream.reply()
    show("number of text events", len(texts))
    show("first events", texts[:3])
    check("the text came in more than one piece", len(texts) > 1)
    check("the pieces add up to the saved reply", "".join(texts) == reply.text)


@feature("live-memory", "With a real model", "A real model remembers across conversations",
         "A fact stated in one conversation is stored, and recalled in a different one for the same user.", live=True)
async def live_memory():
    storage = Storage()
    agent = Agent(
        real(),
        instructions="You are a helpful assistant. When the user tells you a lasting fact about themselves, save it with the remember tool.",
        storage=storage,
        memory=Memory.by_state("user_id", "users"),
    )
    await agent.thread("monday").send("Please remember that I am vegetarian.", user_id="ada")
    facts = await storage.search("users/ada")
    show("stored for users/ada", [(f["key"], f["value"]) for f in facts])
    check("the model saved a fact", len(facts) > 0)
    reply = await agent.thread("friday").send("What do you know about my diet? One short sentence.", user_id="ada")
    show("in a new conversation the model said", reply.text)
    check("it took the remembered fact into account", any(w in (reply.text or "").lower() for w in ("vegetarian", "veggie", "meat-free", "vegan", "plant")))
    check("another user's memory is empty", await storage.search("users/bob") == [])


@feature("live-hooks", "With a real model", "Hooks keep secrets away from a real model",
         "An after_tool hook redacts a card number before the model ever sees it.", live=True)
async def live_hooks():
    @tool(read_only=True)
    def get_payment(order_id: str) -> str:
        """Payment details for an order."""
        return f"order {order_id}: paid with card 4111-1111-1111-1111"

    class Redact:
        def after_tool(self, call, result):
            return result.replace("4111-1111-1111-1111", "[card]") if isinstance(result, str) else None

    result = await Agent(real(), instructions=SUPPORT, tools=[get_payment], hooks=[Redact()]).run("How was order A-1001 paid?")
    seen = json.dumps(result.messages)
    show("what the model saw from the tool", [str(m["content"]) for m in tool_results(result)])
    show("answer", result.text)
    check("the card number never reached the model", "4111" not in seen)
    check("the model still answered", bool(result.text))


@feature("live-audit", "With a real model", "An audit trail of a real run, without its content",
         "Model calls, tool calls and the approver are recorded and verifiable; the conversation text is not.", live=True)
async def live_audit():
    storage = Storage(audit=True)

    @tool(destructive=True)
    def issue_refund(order_id: str, amount: float) -> str:
        """Refund an order."""
        return "refunded"

    agent = Agent(real(), instructions=SUPPORT, tools=[issue_refund], approve=["issue_refund"], storage=storage)
    reply = await agent.thread("visit-9", actor="user:ada").send("My name is Ada Lovelace. Refund order A-1001, 5 dollars.")
    if reply.needs_approval:
        await agent.thread("visit-9", actor="user:dr-rao").approve()
    records = await storage.audit_records(thread_id="visit-9")
    show("records (action, actor)", [(r["action"], r.get("actor")) for r in records])
    report = await storage.verify_audit()
    show("hash chain verification", report)
    check("the chain verifies", report["ok"] is True)
    check("the model call was recorded", "model.called" in [r["action"] for r in records])
    check("no message content is in the records", "Lovelace" not in json.dumps(records))


@feature("live-fallback", "With a real model", "A broken model falls back to a working one",
         "When the primary model fails, the agent answers from the fallback.", live=True)
async def live_fallback():
    agent = Agent(
        LIVE["broken"](),
        fallbacks=[real()],
        model_retry=Retry(max_attempts=1, initial_backoff=0.01, jitter=False),
        fallback_on=["not_found"],  # by default only rate limits and outages fall back; a wrong model name is opt-in
    )
    result = await agent.run("Reply with the single word: ready")
    show("answer", result.text)
    check("the fallback model answered", bool(result.text))


@feature("live-gateway", "With a real model", "Direct model calls: chat, JSON, stream, embeddings",
         "The Gateway class calls a real model without an agent, with schema-checked output and embeddings.", live=True)
async def live_gateway():
    gw = Gateway()
    reply = await gw.chat(real(), "What is the capital of France?", instructions="Answer in one word.")
    show("chat", {"text": reply.text, "model": reply.model})
    check("it answered", "paris" in (reply.text or "").lower())
    triage = {"type": "object", "properties": {"urgent": {"type": "boolean"}}, "required": ["urgent"]}
    reply = await gw.chat(real(), "Patient reports crushing chest pain.", output=triage)
    show("JSON output", reply.output)
    check("the output matches the schema", isinstance(reply.output.get("urgent"), bool))
    chunks = [c.text async for c in gw.stream(real(), "Write about 120 words on why rain matters to farmers.")]
    show("stream pieces", len(chunks))
    check("it streamed in pieces", len(chunks) > 1)
    if LIVE["embedder"] is None:
        show("embeddings", "skipped: this provider has no embedding model")
        return
    vectors = await gw.embed(LIVE["embedder"](), ["first note", "second note"])
    show("embeddings", f"{len(vectors)} vectors of {len(vectors[0])} numbers")
    check("one vector per text", len(vectors) == 2 and len(vectors[0]) > 10)


@feature("live-workflow", "With a real model", "A real model as a workflow step",
         "A graph mixes a plain function and a real agent; the agent's reply lands in the shared state.", live=True)
async def live_workflow():
    wf = Workflow(reducers={"messages": "upsert_by_id"})
    question = {"id": "m1", "role": "user", "content": [{"type": "text", "text": "In one sentence: what is a refund?"}]}
    wf.add_node("ask", lambda s: {"messages": [question]})
    wf.add_node("agent", Agent(real(), instructions="Be brief."))
    wf.start("ask").edge("ask", "agent").edge("agent", END)
    result = await wf.compile().run({})
    roles = [m["role"] for m in result.state["messages"]]
    show("roles", roles)
    show("the agent's reply", result.state["messages"][-1]["content"][0]["text"])
    check("the agent added its reply to the state", roles == ["user", "assistant"])


@feature("live-patterns", "With a real model", "Specialists and patterns with a real model",
         "One-line pipelines of built-in specialists work with a real model: a writer and critic loop, and a planner into a summarizer.", live=True)
async def live_patterns():
    from agentbuild import patterns

    review = await patterns.quick("writer <-> critic", model=real()).run(
        "Write a two-sentence product blurb for a standing desk. It must mention the price $249 and free delivery."
    )
    show("writer <-> critic", review.explain())
    check("it ended (approved, or the round cap) with an output", review.stop_reason in ("approved", "max_rounds") and bool(review.output))
    check("the blurb has the required facts", "$249" in review.output and "deliver" in review.output.lower())
    plan = await patterns.quick("planner > summarizer", model=real()).run("Goal: hold a small bake sale on Saturday")
    show("planner > summarizer", plan.explain())
    check("both steps ran", [t["step"] for t in plan.trace] == ["planner", "summarizer"])


@feature("live-coordinator", "With a real model", "A real coordinator picks the right specialist",
         "Given two specialists, the coordinator model chooses the one that fits and answers from it.", live=True)
async def live_coordinator():
    from agentbuild import patterns

    asked = []

    @tool(read_only=True)
    def get_order(order_id: str) -> dict:
        """Look up an order by id."""
        asked.append(order_id)
        return {"order_id": order_id, "status": "shipped"}

    orders = Agent(real(), instructions="You answer order status questions. Use get_order. Be brief.", tools=[get_order])
    jokes = Agent(real(), instructions="You tell one short, clean joke.")
    triage = patterns.coordinator(real(), {"orders": (orders, "Order status and delivery questions"), "jokes": (jokes, "Tells a joke")})
    result = await triage.run("Where is order A-1001?")
    show("answer", result.text)
    show("specialists the coordinator called", [m.get("name") or m.get("tool") for m in tool_results(result)])
    check("the order specialist's tool ran with the right id", asked == ["A-1001"])


PROVIDERS = {
    "gemini": {
        "label": "Gemini",
        "name": "gemini-2.5-flash",
        "model": lambda: Gemini("gemini-2.5-flash"),
        "broken": lambda: Gemini("gemini-no-such-model"),
        "embedder": lambda: Embedder.gemini("gemini-embedding-001"),
    },
    "openrouter-free": {
        "label": "OpenRouter (free model)",
        "name": "poolside/laguna-xs-2.1:free",
        "model": lambda: OpenRouter("poolside/laguna-xs-2.1:free"),
        "broken": lambda: OpenRouter("no-such-vendor/no-such-model"),
        "embedder": None,  # the free collection has no embedding models
    },
    "openrouter": {
        "label": "OpenRouter",
        "name": "google/gemini-2.5-flash",
        "model": lambda: OpenRouter("google/gemini-2.5-flash"),
        "broken": lambda: OpenRouter("no-such-vendor/no-such-model"),
        "embedder": lambda: Embedder.openai_compatible(
            "openai/text-embedding-3-small", base_url="https://openrouter.ai/api/v1", api_key=os.environ["OPENROUTER_API_KEY"]
        ),
    },
    "openai": {
        "label": "OpenAI",
        "name": "gpt-5-mini",
        "model": lambda: OpenAI("gpt-5-mini"),
        "broken": lambda: OpenAI("gpt-no-such-model"),
        "embedder": lambda: Embedder.openai(),
    },
}


# --------------------------------------------------------------------- main


def clean_source(fn) -> str:
    source = textwrap.dedent(inspect.getsource(fn))
    lines = source.splitlines()
    while lines and not lines[0].startswith(("def ", "async def ")):  # the registration decorator, however many lines
        lines.pop(0)
    return "\n".join(lines)


async def run_one(item: dict) -> dict:
    global _current
    _current = {"steps": [], "checks": []}
    started = time.perf_counter()
    error: Optional[str] = None
    try:
        await item["fn"]()
    except Exception as exc:  # noqa: BLE001 - a failing scenario is a result
        error = f"{type(exc).__name__}: {exc}"
    return {
        "id": item["id"],
        "group": item["group"],
        "title": item["title"],
        "claim": item["claim"],
        "code": clean_source(item["fn"]),
        "steps": _current["steps"],
        "checks": _current["checks"],
        "ok": error is None and all(c["ok"] for c in _current["checks"]) and bool(_current["checks"]),
        "error": error,
        "ms": round((time.perf_counter() - started) * 1000),
    }


async def run_attempts(item: dict, attempts: int) -> dict:
    """A real model can have an off day: a live scenario gets a second try, and the page says so."""
    result = await run_one(item)
    tried = 1
    while not result["ok"] and tried < attempts:
        result = await run_one(item)
        tried += 1
    result["attempts"] = tried
    return result


async def main() -> int:
    args = sys.argv[1:]
    provider = None
    if "--live" in args:
        i = args.index("--live")
        provider = args[i + 1]
        del args[i : i + 2]
    out_arg = next((a for a in args if a.endswith(".json")), None)
    only = {a for a in args if not a.endswith(".json")}
    root = Path(__file__).parent.parent.parent / "website" / "public" / "demo"
    if provider:
        load_keys()
        spec = PROVIDERS[provider]
        LIVE.update(model=spec["model"], broken=spec["broken"], embedder=spec["embedder"])
    out = Path(out_arg) if out_arg else root / ("live.json" if provider else "tour.json")
    results = []
    for item in FEATURES:
        if item["live"] != bool(provider) or (only and item["id"] not in only):
            continue
        result = await run_attempts(item, 2 if provider else 1)
        note = f" (attempt {result['attempts']})" if result["attempts"] > 1 else ""
        print(("ok  " if result["ok"] else "FAIL"), result["id"], f"{result['ms']} ms{note}", result["error"] or "", flush=True)
        results.append(result)
    data = {
        "ran_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "sdk": {"name": "agentbuild-sdk", "version": agentbuild.__version__, "language": "Python"},
        "machine": {"os": f"{platform.system()} {platform.release()}", "python": platform.python_version()},
        "model": {"provider": PROVIDERS[provider]["label"], "name": PROVIDERS[provider]["name"]} if provider else None,
        "features": results,
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(data, indent=1), encoding="utf-8")
    failed = [r["id"] for r in results if not r["ok"]]
    print(f"{len(results) - len(failed)}/{len(results)} passed -> {out}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
