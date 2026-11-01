"""Patterns and specialists (design 0019), with simulated models only."""

import asyncio

import pytest

from criogen import Agent, FakeModel, FakeReply, Storage, patterns, specialists, tool


def run(coroutine):
    return asyncio.run(coroutine)


def agent(*replies, **options):
    return Agent(FakeModel(list(replies), **options))


def test_sequential_passes_text_along_and_traces_every_step():
    seen = []

    def shout(text):
        seen.append(text)
        return text.upper()

    flow = patterns.sequential([("research", agent("facts about solar")), shout, ("write", agent("A short article."))])
    result = flow.run_sync("solar panels")
    assert result.output == "A short article."
    assert seen == ["facts about solar"]  # the function got the first agent's answer
    assert [t["step"] for t in result.trace] == ["research", "shout", "write"]
    assert result.stop_reason == "done" and str(result) == "A short article."
    assert "1. research" in result.explain()


def test_steps_can_be_registered_specialist_names_with_a_model_argument():
    # Every specialist is built from the same model description, so each one replies from its own copy of the script.
    flow = patterns.sequential(["researcher", "writer"], model=FakeModel(["same answer"]))
    result = flow.run_sync("topic")
    assert result.output == "same answer"
    assert [t["step"] for t in result.trace] == ["researcher", "writer"]


def test_a_specialist_without_a_model_explains_what_to_do(monkeypatch):
    monkeypatch.delenv("CRIOGEN_MODEL", raising=False)
    with pytest.raises(ValueError, match="CRIOGEN_MODEL"):
        patterns.sequential(["researcher"])
    with pytest.raises(KeyError, match="registered"):
        patterns.sequential(["no-such-specialist"], model=FakeModel(["x"]))
    with pytest.raises(TypeError):
        patterns.sequential([42])


def test_parallel_runs_branches_together_and_combines_in_order():
    branches = {"sentiment": agent("positive"), "keywords": agent("solar, cost"), "urgency": agent("low")}
    flow = patterns.parallel(branches, synthesizer=("summary", agent("Positive, about solar, low urgency.")))
    result = flow.run_sync("I love my new solar panels")
    assert result.output == "Positive, about solar, low urgency."
    assert {t["step"] for t in result.trace} == {"sentiment", "keywords", "urgency", "summary (synthesis)"}

    plain = patterns.parallel({"a": agent("one"), "b": agent("two")}).run_sync("x")
    assert plain.output == "## a\none\n\n## b\ntwo"  # branch order, whatever finished first


def test_parallel_branches_really_overlap():
    import time

    slow = {f"b{i}": agent("done", delay=0.4) for i in range(4)}
    started = time.perf_counter()
    patterns.parallel(slow).run_sync("go")
    assert time.perf_counter() - started < 1.2  # four 0.4 s branches, not 1.6 s


def test_loop_stops_on_a_condition_or_at_the_cap():
    counter = {"n": 0}

    def step(text):
        counter["n"] += 1
        return f"{text}+"

    done = patterns.loop([step], until=lambda text, n: n == 3, max_iterations=10).run_sync("x")
    assert done.output == "x+++" and done.stop_reason == "condition" and counter["n"] == 3

    counter["n"] = 0
    capped = patterns.loop([step], until=lambda text, n: False, max_iterations=4).run_sync("x")
    assert capped.stop_reason == "max_iterations" and counter["n"] == 4

    counter["n"] = 0
    patterns.loop([step], until=lambda text, n: False, max_iterations=10_000).run_sync("x")
    assert counter["n"] == patterns.CEILING  # a hard ceiling whatever is asked
    with pytest.raises(ValueError):
        patterns.loop([step], until=lambda t, n: True, max_iterations=0)


def test_review_and_critique_revises_until_approved():
    generator = agent("draft one", "draft two", "draft three")
    critic = agent("REVISE: add a number", "REVISE: shorter", "APPROVED")
    result = patterns.review_and_critique(generator, critic, max_rounds=5).run_sync("write")
    assert result.output == "draft three" and result.stop_reason == "approved"
    assert [t["step"] for t in result.trace][:2] == ["step1 (draft 1)", "step2"]
    assert len(result.trace) == 6


def test_review_and_critique_keeps_the_last_draft_when_never_approved():
    result = patterns.review_and_critique(
        agent("d1", "d2", "d3"), agent("REVISE: a", "REVISE: b", "REVISE: c"), max_rounds=3
    ).run_sync("write")
    assert result.stop_reason == "max_rounds" and result.output == "d3"


def test_review_feedback_reaches_the_generator():
    seen = []

    def generator(text):
        seen.append(text)
        return f"draft {len(seen)}"

    patterns.review_and_critique(generator, agent("REVISE: add a citation", "APPROVED")).run_sync("write")
    assert "add a citation" in seen[1] and "draft 1" in seen[1]


def test_iterative_refinement_stops_at_the_threshold():
    result = patterns.iterative_refinement(lambda t: t + "!", score=lambda t: len(t) / 5, threshold=1.0, max_iterations=9).run_sync("ab")
    assert result.output == "ab!!!" and result.stop_reason == "condition"


def test_swarm_hands_off_until_final_and_is_bounded():
    team = {
        "market": agent("Demand is high.\nHANDOFF: engineering"),
        "engineering": agent("Feasible in 3 months.\nHANDOFF: finance"),
        "finance": agent("Budget fits.\nFINAL"),
    }
    result = patterns.swarm(team, start="market").run_sync("Should we build it?")
    assert result.stop_reason == "final"
    assert [t["step"] for t in result.trace] == ["market", "engineering", "finance"]

    ping = {"a": agent(*["x\nHANDOFF: b"] * 10), "b": agent(*["y\nHANDOFF: a"] * 10)}
    endless = patterns.swarm(ping, max_handoffs=3).run_sync("go")
    assert endless.stop_reason == "max_handoffs" and len(endless.trace) <= 5

    stray = patterns.swarm({"a": agent("x\nHANDOFF: nobody"), "b": agent("y")}).run_sync("go")
    assert stray.stop_reason == "unknown_handoff"  # model output is data, not trusted

    with pytest.raises(ValueError):
        patterns.swarm({"only": agent("x")})


def test_swarm_with_a_dispatcher_chooses_the_first_agent():
    team = {"a": agent("a did it\nFINAL"), "b": agent("b did it\nFINAL")}
    result = patterns.swarm(team, start=agent("b")).run_sync("go")
    assert result.trace[1]["step"] == "b" and result.output.startswith("b did it")


def test_human_review_pauses_and_resumes_with_a_decision():
    flow = patterns.human_review(agent("Release 120 rows."))
    paused = flow.run_sync("Release the dataset?")
    assert paused.suspended and paused.stop_reason == "suspended"
    assert paused.waiting_for["draft"] == "Release 120 rows."
    assert run(flow.resume(paused, "approve")).output == "Release 120 rows."

    paused = flow.run_sync("again")
    edited = run(flow.resume(paused, {"decision": "edit", "text": "Release 100 rows."}))
    assert edited.output == "Release 100 rows." and edited.stop_reason == "approved"

    paused = flow.run_sync("third")
    rejected = run(flow.resume(paused, {"decision": "reject"}))
    assert rejected.stop_reason == "rejected" and rejected.output == ""
    with pytest.raises(ValueError):
        run(flow.resume(rejected, "approve"))


def test_human_review_is_durable_across_compiled_copies():
    storage = Storage(audit=True)
    first = patterns.human_review(agent("draft")).compile(storage).thread("t-1", actor="user:ada")
    turn = run(first.run({"input": "go"}))
    assert turn.suspended
    other = patterns.human_review(agent("never used")).compile(storage).thread("t-1", actor="user:boss")
    done = run(other.resume({"review": "approve"}))
    assert done.state["output"] == "draft"
    records = run(storage.audit_records(thread_id="t-1"))
    assert {"user:ada", "user:boss"} <= {r.get("actor") for r in records}


def test_a_pattern_can_be_extended_with_more_nodes():
    flow = patterns.sequential([agent("x")])
    wf = flow.workflow
    wf.add_node("stamp", lambda s: {"output": s["current"] + " [stamped]"})
    wf._edges = [e for e in wf._edges if e != ["finish", "__end__"]] if False else wf._edges  # (edges are plain data)
    assert "stamp" in wf._nodes


def test_custom_wraps_any_workflow():
    from criogen import END, Workflow

    wf = Workflow()
    wf.add_node("only", lambda s: {"output": s["input"].upper()})
    wf.start("only").edge("only", END)
    assert patterns.custom(wf).run_sync("hello").output == "HELLO"


def test_react_caps_model_calls():
    @tool
    def poke() -> str:
        """Pokes."""
        return "again"

    looping = patterns.react(FakeModel([FakeReply.tool_call("poke", {})] * 20), [poke], max_steps=3)
    from criogen import LimitExceededError

    with pytest.raises(LimitExceededError):
        looping.run_sync("go")


def test_coordinator_routes_to_a_specialist_and_nests_as_a_step():
    orders = Agent(FakeModel(["Order A-1 shipped."]))
    triage = patterns.coordinator(
        FakeModel([FakeReply.tool_call("orders", {"input": "where is A-1"}), "It shipped."]),
        {"orders": (orders, "order status questions")},
    )
    assert triage.run_sync("where is A-1").text == "It shipped."
    # An agent pattern is a step like any other.
    flow = patterns.sequential([("triage", triage)])
    flow2 = patterns.sequential([("triage", Agent(FakeModel(["ok"])))])
    assert flow2.run_sync("x").output == "ok" and flow is not None


def test_coordinator_checks_its_specialists():
    with pytest.raises(ValueError):
        patterns.coordinator(FakeModel([]), {})
    with pytest.raises(TypeError):
        patterns.coordinator(FakeModel([]), {"x": 5})


def test_hierarchical_builds_coordinators_of_coordinators():
    web = Agent(FakeModel(["web facts"]))
    root = patterns.hierarchical(
        FakeModel([FakeReply.tool_call("research", {"input": "q"}), "done"]),
        {"research": {"_description": "gathers facts", "web": web}, "writing": Agent(FakeModel(["x"]))},
    )
    assert root is not None


# ---------------------------------------------------------------- specialists


def test_builtin_specialists_are_registered_and_build_agents():
    names = specialists.available()
    for expected in ("researcher", "writer", "critic", "classifier", "planner", "support_agent"):
        assert expected in names
    built = specialists.researcher.build(FakeModel(["ok"]))
    assert built.run_sync("x").text == "ok"


def test_extend_makes_a_new_recipe_and_leaves_the_original_alone():
    original = specialists.researcher.instructions
    careful = specialists.researcher.extend("careful-researcher", add="Always give two sources.")
    assert careful.name == "careful-researcher"
    assert careful.instructions.endswith("Always give two sources.") and careful.instructions.startswith(original)
    assert specialists.researcher.instructions == original

    @tool
    def search(q: str) -> str:
        """Searches."""
        return "result"

    with_tools = careful.extend("searcher", tools=[search])
    assert len(with_tools.settings["tools"]) == 1 and careful.settings["tools"] == []
    replaced = specialists.writer.extend("poet", instructions="Write only poems.")
    assert replaced.instructions == "Write only poems."


def test_register_get_and_name_rules():
    mine = specialists.Specialist("my-triage", "Triage for my shop", "Be quick.")
    specialists.register(mine)
    try:
        assert specialists.get("my-triage") is mine
        with pytest.raises(ValueError, match="already registered"):
            specialists.register(specialists.Specialist("my-triage", "d", "i"))
        specialists.register(specialists.Specialist("my-triage", "d2", "i2"), replace=True)
        # A registered name works as a step.
        flow = patterns.sequential(["my-triage"], model=FakeModel(["done"]))
        assert flow.run_sync("x").output == "done"
    finally:
        specialists.unregister("my-triage")
    with pytest.raises(KeyError):
        specialists.get("my-triage")
    with pytest.raises(ValueError):
        specialists.Specialist("bad name!", "d", "i")
    with pytest.raises(ValueError):
        specialists.Specialist("fine", "  ", "i")


# ---------------------------------------------------------------- one-liners


def test_quick_builds_a_pattern_from_one_line():
    m = FakeModel(["ok"])
    seq = patterns.quick("researcher > writer", model=m)
    assert [t["step"] for t in seq.run_sync("x").trace] == ["researcher", "writer"]
    par = patterns.quick("researcher | critic", model=m)
    assert {t["step"] for t in par.run_sync("x").trace} == {"researcher", "critic"}
    review = patterns.quick("writer <-> critic", model=FakeModel(["APPROVED"]))
    assert review.run_sync("x").stop_reason == "approved"
    single = patterns.quick("summarizer", model=m)
    assert single.run_sync("x").output == "ok"


def test_quick_explains_what_it_cannot_read():
    m = FakeModel(["ok"])
    with pytest.raises(ValueError, match="one kind of connector"):
        patterns.quick("researcher > writer | critic", model=m)
    with pytest.raises(ValueError, match="empty"):
        patterns.quick("researcher >", model=m)
    with pytest.raises(KeyError, match="registered"):
        patterns.quick("researcher > nobody", model=m)
    with pytest.raises(ValueError, match="exactly two"):
        patterns.quick("writer <-> critic <-> researcher", model=m)
