# The playground's code samples (website/app/playground): one region per
# pattern, type-checked by website/scripts/check-snippets.sh.
from typing import Any, Callable, Dict

from pydantic import BaseModel

from criogen import END, Agent, NodeContext, Workflow, goto, patterns, suspend, tool

MODEL = "openai:gpt-5-mini"


async def react() -> None:
    # [show react]
    @tool
    def weather(city: str) -> str:
        """Today's forecast for a city."""
        return f"{city}: 24 °C, sunny, 5% chance of rain"

    @tool
    def calendar(day: str) -> str:
        """The user's plans for Saturday or Sunday."""
        return "Saturday: free after 11:00"

    # Think → call a tool → read the result → repeat, at most 6 model calls.
    agent = patterns.react("openai:gpt-5-mini", [weather, calendar], max_steps=6)
    result = await agent.run("Is this weekend good for a picnic in Lisbon?")
    print(result.text)
    # [/show react]


async def chain_of_thought() -> None:
    # [show chain-of-thought]
    agent = Agent(
        "openai:gpt-5-mini",
        instructions="Think step by step. Write numbered steps, then a last line: ANSWER: <answer>.",
    )
    result = await agent.run("Coffee is $3, muffins $2. Ana buys 4 items for $10. How many coffees?")
    print((result.text or "").split("ANSWER:")[-1].strip())  # 2
    # [/show chain-of-thought]


async def tree_of_thoughts() -> None:
    # [show tree-of-thoughts]
    class Score(BaseModel):
        score: int  # 1-10

    proposer = Agent(MODEL, instructions="Propose one approach to the problem, with an example.")
    judge = Agent(MODEL, instructions="Rate how promising the idea is, 1 to 10.", output=Score)
    grower = Agent(MODEL, instructions="Develop the idea into the final answer.")

    def branch(n: int) -> Callable[[Dict[str, Any]], Any]:
        async def think(state: Dict[str, Any]) -> Dict[str, Any]:
            idea = await proposer.run(f"{state['input']}\nApproach {n}: make it unlike the obvious one.")
            rated = await judge.run(f"Problem: {state['input']}\nIdea: {idea.text}")
            return {"ideas": [{"idea": idea.text, "score": rated.output.score}]}

        return think

    async def grow(state: Dict[str, Any]) -> Dict[str, Any]:
        best = max(state["ideas"], key=lambda i: i["score"])  # prune the rest
        return {"output": (await grower.run(f"{state['input']}\nIdea: {best['idea']}")).text}

    wf = Workflow(reducers={"ideas": "append"})
    wf.add_node("begin", lambda state: None)
    names = [f"branch{n}" for n in (1, 2, 3)]
    for n, name in enumerate(names, 1):
        wf.add_node(name, branch(n))
        wf.edge("begin", name)  # the three branches run at the same time
    wf.add_node("grow", grow)
    wf.join(names, "grow").edge("grow", END).start("begin")
    result = await wf.compile().run({"input": "Name a coffee shop for software developers."})
    print(result.state["output"])
    # [/show tree-of-thoughts]


async def plan_and_solve() -> None:
    # [show plan-and-solve]
    class Plan(BaseModel):
        steps: list[str]

    planner = Agent(MODEL, instructions="Write a plan of three short steps.", output=Plan)
    executor = Agent(MODEL, instructions="Carry out one step of a plan. At most two sentences.")
    writer = Agent(MODEL, instructions="Write the final brief from the step results.")

    async def plan(state: Dict[str, Any]) -> Dict[str, Any]:
        return {"steps": (await planner.run(state["input"])).output.steps, "results": []}

    async def execute(state: Dict[str, Any]) -> Any:
        done = state["results"]
        step = state["steps"][len(done)]
        out = await executor.run(f"Task: {state['input']}\nResults so far: {done}\nNow: {step}")
        results = [*done, out.text]
        # One step per pass; loop until the plan is done.
        return goto("execute" if len(results) < len(state["steps"]) else "write", {"results": results})

    async def write(state: Dict[str, Any]) -> Dict[str, Any]:
        return {"output": (await writer.run(f"{state['input']}\n{state['results']}")).text}

    wf = Workflow()
    wf.add_node("plan", plan)
    wf.add_node("execute", execute)
    wf.add_node("write", write)
    wf.start("plan").edge("plan", "execute").goto("execute", ["execute", "write"]).edge("write", END)
    result = await wf.compile().run({"input": "Is a heat pump a good idea for a 1970s house?"}, max_steps=20)
    print(result.state["output"])
    # [/show plan-and-solve]


async def rewoo() -> None:
    # [show rewoo]
    class Call(BaseModel):
        id: str  # "E1"
        tool: str  # "search" or "calc"
        arg: str  # may use earlier results: "population of #E1"

    class Plan(BaseModel):
        calls: list[Call]

    def search(query: str) -> str:
        return {"capital of France": "Paris", "population of Paris": "2.1 million"}.get(query, "no result")

    def calc(expression: str) -> str:
        return "11.9"  # your calculator here

    planner = Agent(MODEL, instructions="Plan every tool call up front. Tools: search, calc.", output=Plan)
    solver = Agent(MODEL, instructions="Answer from the plan and the evidence only.")

    async def plan(state: Dict[str, Any]) -> Dict[str, Any]:
        return {"calls": [c.model_dump() for c in (await planner.run(state["input"])).output.calls]}

    def work(state: Dict[str, Any]) -> Dict[str, Any]:
        # No model here: run the tools, filling in #E placeholders.
        evidence: Dict[str, str] = {}
        for call in state["calls"]:
            arg = call["arg"]
            for key, value in evidence.items():
                arg = arg.replace(f"#{key}", value)
            evidence[call["id"]] = (calc if call["tool"] == "calc" else search)(arg)
        return {"evidence": evidence}

    async def solve(state: Dict[str, Any]) -> Dict[str, Any]:
        out = await solver.run(f"Task: {state['input']}\nEvidence: {state['evidence']}")
        return {"output": out.text}

    wf = Workflow()
    wf.add_node("plan", plan)
    wf.add_node("work", work)
    wf.add_node("solve", solve)
    wf.start("plan").edge("plan", "work").edge("work", "solve").edge("solve", END)
    result = await wf.compile().run({"input": "Which capital has more people, France's or Japan's?"})
    print(result.state["output"])  # two model calls, however many tools
    # [/show rewoo]


async def reflexion() -> None:
    # [show reflexion]
    writer = Agent(MODEL, instructions="Write one launch tweet. Follow every lesson you are given.")
    reflector = Agent(MODEL, instructions="Say in one line what to do differently next time.")
    lessons: list[str] = []

    def failures(text: str) -> list[str]:  # the evaluator is plain code
        checks = {"over 200 characters": len(text) > 200, "no price": "$49" not in text, "hashtags": "#" in text}
        return [name for name, failed in checks.items() if failed]

    async def attempt(text: str) -> str:
        task = f"{text}\nLessons so far: {lessons}" if lessons else text
        draft = (await writer.run(task)).text or ""
        if failed := failures(draft):
            lessons.append((await reflector.run(f"Tweet: {draft}\nFailed: {failed}")).text or "")
        return draft

    flow = patterns.loop([attempt], until=lambda text, _round: not failures(text), max_iterations=3)
    result = await flow.run("Launch tweet for SoundVeil, $49 noise-cancelling headphones.")
    print(result.output, result.stop_reason)  # "condition" once the checks pass
    # [/show reflexion]


async def router() -> None:
    # [show router]
    class Route(BaseModel):
        team: str  # "billing" | "technical" | "sales"

    classify = Agent(MODEL, instructions="Classify the request: billing, technical or sales.", output=Route)
    teams = {name: Agent(MODEL, instructions=f"You are the {name} support specialist.") for name in ("billing", "technical", "sales")}

    async def route(state: Dict[str, Any]) -> Dict[str, Any]:
        return {"team": (await classify.run(state["input"])).output.team}

    wf = Workflow()
    wf.add_node("route", route)
    for name, agent in teams.items():
        wf.add_node(name, agent)  # an agent node reads and writes `messages`
        wf.edge(name, END)
    wf.start("route").route("route", lambda s: s["team"], {n: n for n in teams})
    result = await wf.compile().run({"input": "I was charged twice", "messages": [{"role": "user", "content": "I was charged twice"}]})
    # [/show router]


async def supervisor() -> None:
    # [show supervisor]
    researcher = Agent(MODEL, instructions="Find facts. Be brief and precise.")
    writer = Agent(MODEL, instructions="Write clear, short text from notes.")
    reviewer = Agent(MODEL, instructions="Check the text for errors.")

    # The supervisor calls workers as tools, in any order, until it can answer.
    boss = patterns.coordinator(
        MODEL,
        {
            "researcher": (researcher, "finds facts"),
            "writer": (writer, "writes text"),
            "reviewer": (reviewer, "checks text"),
        },
        limits={"model_calls": 8},
    )
    result = await boss.run("Three bullet points on why teams adopt TypeScript.")
    print(result.text)
    # [/show supervisor]


async def hierarchical() -> None:
    # [show hierarchical]
    footfall = Agent(MODEL, instructions="Estimate footfall and nearby competitors.")
    costs = Agent(MODEL, instructions="Estimate rent, staff and break-even.")

    # Nested mappings become team leads (sub-coordinators) with their own workers.
    manager = patterns.hierarchical(
        MODEL,
        {
            "research": {"_description": "market research", "footfall": footfall},
            "finance": {"_description": "money questions", "costs": costs},
        },
        instructions="You manage a research team and a finance team. Decide from their reports.",
    )
    result = await manager.run("Should our bakery open a second shop downtown?")
    print(result.text)
    # [/show hierarchical]


async def collaboration() -> None:
    # [show collaboration]
    researcher = Agent(MODEL, instructions="List the key facts as three bullet points.")
    writer = Agent(MODEL, instructions="Write the answer for the audience, from these facts only.")
    editor = Agent(MODEL, instructions="Make it shorter and clearer. Keep the meaning.")

    # Each output is the next one's input.
    flow = patterns.sequential([("researcher", researcher), ("writer", writer), ("editor", editor)])
    result = await flow.run("Explain why the sky is blue, for a 10-year-old.")
    print(result.output)
    print(result.explain())  # each step and what it returned
    # [/show collaboration]


async def debate() -> None:
    # [show debate]
    sides = {
        "for": Agent(MODEL, instructions="Argue FOR. Answer the other side in two sentences."),
        "against": Agent(MODEL, instructions="Argue AGAINST. Answer the other side in two sentences."),
    }
    judge = Agent(MODEL, instructions="Judge the debate: the verdict and the deciding reason.")

    def speaker(name: str, rounds: int = 2) -> Callable[[Dict[str, Any]], Any]:
        async def speak(state: Dict[str, Any]) -> Any:
            out = await sides[name].run("\n".join([state["input"], *state["log"]]))
            log = [*state["log"], f"{name}: {out.text}"]
            other = "against" if name == "for" else "for"
            return goto("judge" if len(log) >= 2 * rounds else other, {"log": log})

        return speak

    async def decide(state: Dict[str, Any]) -> Dict[str, Any]:
        return {"output": (await judge.run("\n".join(state["log"]))).text}

    wf = Workflow()
    wf.add_node("for", speaker("for"))
    wf.add_node("against", speaker("against"))
    wf.add_node("judge", decide)
    wf.goto("for", ["against", "judge"]).goto("against", ["for", "judge"])
    wf.start("for").edge("judge", END)
    result = await wf.compile().run({"input": "Should a 5-person startup build its own LLM gateway?", "log": []})
    print(result.state["output"])
    # [/show debate]


async def parallel() -> None:
    # [show parallel]
    flow = patterns.parallel(
        {
            "sentiment": Agent(MODEL, instructions="Overall sentiment and one reason."),
            "topics": Agent(MODEL, instructions="Topics mentioned, with + or − each."),
            "reply": Agent(MODEL, instructions="Draft a two-sentence reply to the customer."),
        },
        synthesizer=Agent(MODEL, instructions="Combine the analyses into a short report."),
    )
    result = await flow.run("Battery lasts forever, but the app keeps logging me out.")
    print(result.output)  # the branches ran at the same time
    # [/show parallel]


async def swarm() -> None:
    # [show swarm]
    @tool(read_only=True)
    def lookup_order(customer: str) -> dict:
        """The customer's latest order and its return status."""
        return {"order": "A-1001", "return": "received 24 Oct", "refund": None}

    team = patterns.swarm(
        {
            "triage": Agent(MODEL, instructions="Work out what the customer needs."),
            "orders": Agent(MODEL, instructions="Look up orders and returns.", tools=[lookup_order]),
            "refunds": Agent(MODEL, instructions="Issue refunds for returned items."),
        },
        start="triage",
        max_handoffs=6,
    )
    # Each agent ends with "HANDOFF: <name>" or "FINAL"; no one is in charge.
    result = await team.run("I returned my headphones two weeks ago and have no refund.")
    print(result.output, result.stop_reason)
    # [/show swarm]


async def human_in_the_loop() -> None:
    # [show human-in-the-loop]
    drafter = Agent(MODEL, instructions="Draft a short refund email to the customer.")
    flow = patterns.human_review(("draft", drafter))

    paused = await flow.run("Refund $180 for damaged order A-2077.")
    print(paused.suspended, paused.waiting_for)  # True, {"draft": ..., "question": ...}

    # Later, after a person has read the draft (with compile(storage), even days later):
    done = await flow.resume(paused, "approve")  # or {"decision": "edit", "text": ...} / {"decision": "reject"}
    print(done.output, done.stop_reason)
    # [/show human-in-the-loop]


async def simple_reflex() -> None:
    # [show simple-reflex]
    target = 21.0

    def rule(state: Dict[str, Any]) -> str:  # the condition-action rules
        t = state["temperature"]
        return "heat" if t < target - 1 else "cool" if t > target + 1 else "idle"

    def act(action: str) -> Callable[[Dict[str, Any]], Dict[str, Any]]:
        return lambda state: {"action": action}

    wf = Workflow()
    wf.add_node("sense", lambda state: None)
    for action in ("heat", "cool", "idle"):
        wf.add_node(action, act(action))
        wf.edge(action, END)
    wf.start("sense").route("sense", rule, {a: a for a in ("heat", "cool", "idle")})
    app = wf.compile()
    for reading in (17.5, 21.0, 23.6):
        print(reading, (await app.run({"temperature": reading})).state["action"])
    # [/show simple-reflex]


async def model_based_reflex() -> None:
    # [show model-based-reflex]
    rooms = ["A", "B", "C", "D"]

    def step(state: Dict[str, Any]) -> Any:
        # The internal model: rooms known to be clean, kept in the run state.
        known = set(state["known"])
        here = state["at"]
        if state["dirty"].get(here):
            return goto("step", {"dirty": {**state["dirty"], here: False}, "known": [*known, here]})
        known.add(here)
        if known == set(rooms):
            return goto("done", {"known": sorted(known)})
        nearest = next(r for r in rooms if r not in known)
        return goto("step", {"at": nearest, "known": sorted(known)})

    wf = Workflow()
    wf.add_node("step", step)
    wf.add_node("done", lambda state: {"output": "all rooms clean"})
    wf.start("step").goto("step", ["step", "done"]).edge("done", END)
    world = {"at": "A", "known": [], "dirty": {"A": True, "B": False, "C": True, "D": True}}
    print((await wf.compile().run(world, max_steps=40)).state["output"])
    # [/show model-based-reflex]


async def goal_based() -> None:
    # [show goal-based]
    roads = {"Depot": ["Market", "Mill"], "Market": ["Park", "Bridge"], "Mill": ["Bridge"], "Bridge": ["Clinic"], "Park": ["Clinic"]}

    def plan(here: str, goal: str, closed: list) -> list:
        paths = [[here]]
        while paths:  # breadth-first search: the shortest route first
            path = paths.pop(0)
            if path[-1] == goal:
                return path
            paths += [[*path, n] for n in roads.get(path[-1], []) if n not in path and [path[-1], n] not in closed]
        return []

    def fly(state: Dict[str, Any]) -> Any:
        route = plan(state["at"], state["goal"], state["closed"])  # replans every hop
        if len(route) < 2:
            return goto("arrived", {})
        return goto("fly", {"at": route[1], "log": [*state["log"], route[1]]})

    wf = Workflow()
    wf.add_node("fly", fly)
    wf.add_node("arrived", lambda state: {"output": " → ".join(state["log"])})
    wf.start("fly").goto("fly", ["fly", "arrived"]).edge("arrived", END)
    start = {"at": "Depot", "goal": "Clinic", "closed": [["Market", "Bridge"]], "log": ["Depot"]}
    print((await wf.compile().run(start, max_steps=20)).state["output"])
    # [/show goal-based]


async def utility_based() -> None:
    # [show utility-based]
    flights = [
        {"name": "Budget, 1 stop", "price": 180, "hours": 9.5, "stops": 1},
        {"name": "Direct, early", "price": 340, "hours": 5, "stops": 0},
        {"name": "Night, 2 stops", "price": 140, "hours": 14, "stops": 2},
    ]
    weights = {"price": 0.4, "time": 0.3, "stops": 0.3}  # what this traveller cares about

    def utility(f: Dict[str, Any]) -> float:
        return weights["price"] * (1 - f["price"] / 500) + weights["time"] * (1 - f["hours"] / 15) + weights["stops"] * (1 - f["stops"] / 2)

    @tool(read_only=True)
    def best_flight() -> dict:
        """The flight with the highest utility for this traveller."""
        return max(flights, key=utility)

    agent = Agent(MODEL, instructions="Book the best flight. Use best_flight; explain the trade-off.", tools=[best_flight])
    print((await agent.run("Book Lisbon to Berlin.")).text)
    # [/show utility-based]


async def learning() -> None:
    # [show learning]
    import random

    from criogen import Storage

    storage = Storage("sqlite://learning.db")  # what the agent has learned survives restarts
    styles = ["short", "detailed", "friendly + steps"]

    async def known(style: str) -> Dict[str, Any]:
        item = await storage.recall("styles", style)
        return item["value"] if item else {"n": 0, "mean": 0.0}

    async def reply(question: str, rating_of: Callable[[str], float]) -> None:
        scores = {s: await known(s) for s in styles}
        explore = random.random() < 0.2  # the problem generator: sometimes try something new
        style = random.choice(styles) if explore else max(styles, key=lambda s: scores[s]["mean"])
        reward = rating_of(style)  # the critic: a customer rating, a test, a metric
        s = scores[style]
        s["n"] += 1
        s["mean"] += (reward - s["mean"]) / s["n"]  # the learning element
        await storage.remember("styles", style, s)
    # [/show learning]
