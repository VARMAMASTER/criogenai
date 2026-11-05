// The playground's code samples (website/app/playground): one region per
// pattern, type-checked by website/scripts/check-snippets.sh.
import { END, Agent, Storage, Workflow, goto, patterns, tool } from "criogen";
import { z } from "zod";

const MODEL = "openai:gpt-5-mini";
type State = Record<string, any>;

export async function react() {
  // [show react]
  const weather = tool({
    name: "weather",
    description: "Today's forecast for a city.",
    input: z.object({ city: z.string() }),
    run: ({ city }) => `${city}: 24 °C, sunny, 5% chance of rain`,
  });
  const calendar = tool({
    name: "calendar",
    description: "The user's plans for Saturday or Sunday.",
    input: z.object({ day: z.string() }),
    run: () => "Saturday: free after 11:00",
  });

  // Think → call a tool → read the result → repeat, at most 6 model calls.
  const agent = patterns.react("openai:gpt-5-mini", [weather, calendar], { maxSteps: 6 });
  const result = await agent.run("Is this weekend good for a picnic in Lisbon?");
  console.log(result.text);
  // [/show react]
}

export async function chainOfThought() {
  // [show chain-of-thought]
  const agent = new Agent({
    model: "openai:gpt-5-mini",
    instructions: "Think step by step. Write numbered steps, then a last line: ANSWER: <answer>.",
  });
  const result = await agent.run("Coffee is $3, muffins $2. Ana buys 4 items for $10. How many coffees?");
  console.log((result.text ?? "").split("ANSWER:").pop()?.trim()); // 2
  // [/show chain-of-thought]
}

export async function treeOfThoughts() {
  // [show tree-of-thoughts]
  const proposer = new Agent({ model: MODEL, instructions: "Propose one approach to the problem, with an example." });
  const judge = new Agent<{ score: number }>({
    model: MODEL,
    instructions: "Rate how promising the idea is, 1 to 10.",
    output: { type: "object", properties: { score: { type: "integer", minimum: 1, maximum: 10 } }, required: ["score"] },
  });
  const grower = new Agent({ model: MODEL, instructions: "Develop the idea into the final answer." });

  const branch = (n: number) => async (state: State) => {
    const idea = await proposer.run(`${state.input}\nApproach ${n}: make it unlike the obvious one.`);
    const rated = await judge.run(`Problem: ${state.input}\nIdea: ${idea.text}`);
    return { ideas: [{ idea: idea.text, score: rated.output?.score ?? 0 }] };
  };

  const names = ["branch1", "branch2", "branch3"];
  const wf = new Workflow({ reducers: { ideas: "append" } }).node("begin", () => undefined).start("begin");
  names.forEach((name, i) => wf.node(name, branch(i + 1)).edge("begin", name)); // the branches run at once
  wf.node("grow", async (state) => {
    const best = (state.ideas as { idea: string; score: number }[]).sort((a, b) => b.score - a.score)[0]; // prune the rest
    return { output: (await grower.run(`${state.input}\nIdea: ${best.idea}`)).text };
  });
  wf.join(names, "grow").edge("grow", END);
  const result = await wf.compile().run({ input: "Name a coffee shop for software developers." });
  console.log(result.state.output);
  // [/show tree-of-thoughts]
}

export async function planAndSolve() {
  // [show plan-and-solve]
  const planner = new Agent<{ steps: string[] }>({
    model: MODEL,
    instructions: "Write a plan of three short steps.",
    output: { type: "object", properties: { steps: { type: "array", items: { type: "string" } } }, required: ["steps"] },
  });
  const executor = new Agent({ model: MODEL, instructions: "Carry out one step of a plan. At most two sentences." });
  const writer = new Agent({ model: MODEL, instructions: "Write the final brief from the step results." });

  const app = new Workflow()
    .node("plan", async (state) => ({ steps: (await planner.run(String(state.input))).output?.steps ?? [], results: [] }))
    .node("execute", async (state: State) => {
      const step = state.steps[state.results.length];
      const out = await executor.run(`Task: ${state.input}\nResults so far: ${state.results}\nNow: ${step}`);
      const results = [...state.results, out.text];
      // One step per pass; loop until the plan is done.
      return goto(results.length < state.steps.length ? "execute" : "write", { results });
    })
    .node("write", async (state) => ({ output: (await writer.run(`${state.input}\n${state.results}`)).text }))
    .start("plan")
    .edge("plan", "execute")
    .goto("execute", ["execute", "write"])
    .edge("write", END)
    .compile();
  const result = await app.run({ input: "Is a heat pump a good idea for a 1970s house?" }, { maxSteps: 20 });
  console.log(result.state.output);
  // [/show plan-and-solve]
}

export async function rewoo() {
  // [show rewoo]
  type Call = { id: string; tool: "search" | "calc"; arg: string }; // arg may use "#E1"
  const planner = new Agent<{ calls: Call[] }>({
    model: MODEL,
    instructions: "Plan every tool call up front. Tools: search, calc. Ids E1, E2…; args may use #E1.",
    output: {
      type: "object",
      properties: {
        calls: {
          type: "array",
          items: {
            type: "object",
            properties: { id: { type: "string" }, tool: { enum: ["search", "calc"] }, arg: { type: "string" } },
            required: ["id", "tool", "arg"],
          },
        },
      },
      required: ["calls"],
    },
  });
  const solver = new Agent({ model: MODEL, instructions: "Answer from the plan and the evidence only." });
  const facts: Record<string, string> = { "capital of France": "Paris", "population of Paris": "2.1 million" };
  const tools = { search: (q: string) => facts[q] ?? "no result", calc: (_expr: string) => "11.9" /* your calculator */ };

  const app = new Workflow()
    .node("plan", async (state) => ({ calls: (await planner.run(String(state.input))).output?.calls ?? [] }))
    .node("work", (state: State) => {
      // No model here: run the tools, filling in #E placeholders.
      const evidence: Record<string, string> = {};
      for (const call of state.calls as Call[]) {
        const arg = Object.entries(evidence).reduce((a, [k, v]) => a.replaceAll(`#${k}`, v), call.arg);
        evidence[call.id] = tools[call.tool](arg);
      }
      return { evidence };
    })
    .node("solve", async (state) => ({ output: (await solver.run(`Task: ${state.input}\nEvidence: ${JSON.stringify(state.evidence)}`)).text }))
    .start("plan")
    .edge("plan", "work")
    .edge("work", "solve")
    .edge("solve", END)
    .compile();
  const result = await app.run({ input: "Which capital has more people, France's or Japan's?" });
  console.log(result.state.output); // two model calls, however many tools
  // [/show rewoo]
}

export async function reflexion() {
  // [show reflexion]
  const writer = new Agent({ model: MODEL, instructions: "Write one launch tweet. Follow every lesson you are given." });
  const reflector = new Agent({ model: MODEL, instructions: "Say in one line what to do differently next time." });
  const lessons: string[] = [];

  const failures = (text: string) => // the evaluator is plain code
    Object.entries({ "over 200 characters": text.length > 200, "no price": !text.includes("$49"), hashtags: text.includes("#") })
      .filter(([, failed]) => failed)
      .map(([name]) => name);

  const attempt = async (task: string) => {
    const draft = (await writer.run(lessons.length ? `${task}\nLessons so far: ${lessons.join("; ")}` : task)).text ?? "";
    const failed = failures(draft);
    if (failed.length) lessons.push((await reflector.run(`Tweet: ${draft}\nFailed: ${failed}`)).text ?? "");
    return draft;
  };

  const flow = patterns.loop([attempt], (text) => failures(text).length === 0, { maxIterations: 3 });
  const result = await flow.run("Launch tweet for SoundVeil, $49 noise-cancelling headphones.");
  console.log(result.output, result.stopReason); // "condition" once the checks pass
  // [/show reflexion]
}

export async function router() {
  // [show router]
  const teams = ["billing", "technical", "sales"] as const;
  const classify = new Agent<{ team: (typeof teams)[number] }>({
    model: MODEL,
    instructions: "Classify the request: billing, technical or sales.",
    output: { type: "object", properties: { team: { enum: [...teams] } }, required: ["team"] },
  });

  const wf = new Workflow().node("route", async (state) => ({ team: (await classify.run(String(state.input))).output?.team }));
  for (const name of teams) {
    wf.addNode(name, new Agent({ model: MODEL, instructions: `You are the ${name} support specialist.` })); // reads and writes `messages`
    wf.edge(name, END);
  }
  wf.start("route").route("route", (s) => String(s.team), Object.fromEntries(teams.map((n) => [n, n])));
  const text = "I was charged twice";
  const result = await wf.compile().run({ input: text, messages: [{ role: "user", content: text }] });
  // [/show router]
}

export async function supervisor() {
  // [show supervisor]
  const researcher = new Agent({ model: MODEL, instructions: "Find facts. Be brief and precise." });
  const writer = new Agent({ model: MODEL, instructions: "Write clear, short text from notes." });
  const reviewer = new Agent({ model: MODEL, instructions: "Check the text for errors." });

  // The supervisor calls workers as tools, in any order, until it can answer.
  const boss = patterns.coordinator(
    MODEL,
    { researcher: [researcher, "finds facts"], writer: [writer, "writes text"], reviewer: [reviewer, "checks text"] },
    { limits: { modelCalls: 8 } },
  );
  const result = await boss.run("Three bullet points on why teams adopt TypeScript.");
  console.log(result.text);
  // [/show supervisor]
}

export async function hierarchical() {
  // [show hierarchical]
  const footfall = new Agent({ model: MODEL, instructions: "Estimate footfall and nearby competitors." });
  const costs = new Agent({ model: MODEL, instructions: "Estimate rent, staff and break-even." });

  // Nested objects become team leads (sub-coordinators) with their own workers.
  const manager = patterns.hierarchical(
    MODEL,
    {
      research: { _description: "market research", footfall },
      finance: { _description: "money questions", costs },
    },
    { instructions: "You manage a research team and a finance team. Decide from their reports." },
  );
  const result = await manager.run("Should our bakery open a second shop downtown?");
  console.log(result.text);
  // [/show hierarchical]
}

export async function collaboration() {
  // [show collaboration]
  const researcher = new Agent({ model: MODEL, instructions: "List the key facts as three bullet points." });
  const writer = new Agent({ model: MODEL, instructions: "Write the answer for the audience, from these facts only." });
  const editor = new Agent({ model: MODEL, instructions: "Make it shorter and clearer. Keep the meaning." });

  // Each output is the next one's input.
  const flow = patterns.sequential([["researcher", researcher], ["writer", writer], ["editor", editor]]);
  const result = await flow.run("Explain why the sky is blue, for a 10-year-old.");
  console.log(result.output);
  console.log(result.explain()); // each step and what it returned
  // [/show collaboration]
}

export async function debate() {
  // [show debate]
  const sides: Record<string, Agent> = {
    for: new Agent({ model: MODEL, instructions: "Argue FOR. Answer the other side in two sentences." }),
    against: new Agent({ model: MODEL, instructions: "Argue AGAINST. Answer the other side in two sentences." }),
  };
  const judge = new Agent({ model: MODEL, instructions: "Judge the debate: the verdict and the deciding reason." });

  const speaker = (name: string, rounds = 2) => async (state: State) => {
    const out = await sides[name].run([state.input, ...state.log].join("\n"));
    const log = [...state.log, `${name}: ${out.text}`];
    return goto(log.length >= 2 * rounds ? "judge" : name === "for" ? "against" : "for", { log });
  };

  const app = new Workflow()
    .node("for", speaker("for"))
    .node("against", speaker("against"))
    .node("judge", async (state) => ({ output: (await judge.run((state.log as string[]).join("\n"))).text }))
    .goto("for", ["against", "judge"])
    .goto("against", ["for", "judge"])
    .start("for")
    .edge("judge", END)
    .compile();
  const result = await app.run({ input: "Should a 5-person startup build its own LLM gateway?", log: [] });
  console.log(result.state.output);
  // [/show debate]
}

export async function parallel() {
  // [show parallel]
  const flow = patterns.parallel(
    {
      sentiment: new Agent({ model: MODEL, instructions: "Overall sentiment and one reason." }),
      topics: new Agent({ model: MODEL, instructions: "Topics mentioned, with + or − each." }),
      reply: new Agent({ model: MODEL, instructions: "Draft a two-sentence reply to the customer." }),
    },
    { synthesizer: new Agent({ model: MODEL, instructions: "Combine the analyses into a short report." }) },
  );
  const result = await flow.run("Battery lasts forever, but the app keeps logging me out.");
  console.log(result.output); // the branches ran at the same time
  // [/show parallel]
}

export async function swarm() {
  // [show swarm]
  const lookupOrder = tool({
    name: "lookup_order",
    description: "The customer's latest order and its return status.",
    input: z.object({ customer: z.string() }),
    readOnly: true,
    run: () => ({ order: "A-1001", return: "received 24 Oct", refund: null }),
  });

  const team = patterns.swarm(
    {
      triage: new Agent({ model: MODEL, instructions: "Work out what the customer needs." }),
      orders: new Agent({ model: MODEL, instructions: "Look up orders and returns.", tools: [lookupOrder] }),
      refunds: new Agent({ model: MODEL, instructions: "Issue refunds for returned items." }),
    },
    { start: "triage", maxHandoffs: 6 },
  );
  // Each agent ends with "HANDOFF: <name>" or "FINAL"; no one is in charge.
  const result = await team.run("I returned my headphones two weeks ago and have no refund.");
  console.log(result.output, result.stopReason);
  // [/show swarm]
}

export async function humanInTheLoop() {
  // [show human-in-the-loop]
  const drafter = new Agent({ model: MODEL, instructions: "Draft a short refund email to the customer." });
  const flow = patterns.humanReview(["draft", drafter]);

  const paused = await flow.run("Refund $180 for damaged order A-2077.");
  console.log(paused.suspended, paused.waitingFor); // true, { draft, question }

  // Later, after a person has read the draft (with compile(storage), even days later):
  const done = await flow.resume(paused, "approve"); // or { decision: "edit", text } / { decision: "reject" }
  console.log(done.output, done.stopReason);
  // [/show human-in-the-loop]
}

export async function simpleReflex() {
  // [show simple-reflex]
  const target = 21;
  const rule = (state: State) => (state.temperature < target - 1 ? "heat" : state.temperature > target + 1 ? "cool" : "idle");

  const wf = new Workflow().node("sense", () => undefined).start("sense");
  for (const action of ["heat", "cool", "idle"]) wf.node(action, () => ({ action })).edge(action, END);
  wf.route("sense", rule, { heat: "heat", cool: "cool", idle: "idle" }); // the condition-action rules
  const app = wf.compile();
  for (const temperature of [17.5, 21, 23.6]) console.log(temperature, (await app.run({ temperature })).state.action);
  // [/show simple-reflex]
}

export async function modelBasedReflex() {
  // [show model-based-reflex]
  const rooms = ["A", "B", "C", "D"];
  const app = new Workflow()
    .node("step", (state: State) => {
      // The internal model: rooms known to be clean, kept in the run state.
      const known = new Set<string>(state.known);
      const here: string = state.at;
      if (state.dirty[here]) return goto("step", { dirty: { ...state.dirty, [here]: false }, known: [...known, here] });
      known.add(here);
      if (rooms.every((r) => known.has(r))) return goto("done", { known: [...known] });
      return goto("step", { at: rooms.find((r) => !known.has(r)), known: [...known] });
    })
    .node("done", () => ({ output: "all rooms clean" }))
    .start("step")
    .goto("step", ["step", "done"])
    .edge("done", END)
    .compile();
  const world = { at: "A", known: [], dirty: { A: true, B: false, C: true, D: true } };
  console.log((await app.run(world, { maxSteps: 40 })).state.output);
  // [/show model-based-reflex]
}

export async function goalBased() {
  // [show goal-based]
  const roads: Record<string, string[]> = { Depot: ["Market", "Mill"], Market: ["Park", "Bridge"], Mill: ["Bridge"], Bridge: ["Clinic"], Park: ["Clinic"] };

  const plan = (here: string, goal: string, closed: string[]): string[] => {
    const paths = [[here]];
    while (paths.length) {
      // breadth-first search: the shortest route first
      const path = paths.shift()!;
      const last = path[path.length - 1];
      if (last === goal) return path;
      for (const n of roads[last] ?? []) if (!path.includes(n) && !closed.includes(`${last}-${n}`)) paths.push([...path, n]);
    }
    return [];
  };

  const app = new Workflow()
    .node("fly", (state: State) => {
      const route = plan(state.at, state.goal, state.closed); // replans every hop
      return route.length < 2 ? goto("arrived") : goto("fly", { at: route[1], log: [...state.log, route[1]] });
    })
    .node("arrived", (state) => ({ output: (state.log as string[]).join(" → ") }))
    .start("fly")
    .goto("fly", ["fly", "arrived"])
    .edge("arrived", END)
    .compile();
  const start = { at: "Depot", goal: "Clinic", closed: ["Market-Bridge"], log: ["Depot"] };
  console.log((await app.run(start, { maxSteps: 20 })).state.output);
  // [/show goal-based]
}

export async function utilityBased() {
  // [show utility-based]
  const flights = [
    { name: "Budget, 1 stop", price: 180, hours: 9.5, stops: 1 },
    { name: "Direct, early", price: 340, hours: 5, stops: 0 },
    { name: "Night, 2 stops", price: 140, hours: 14, stops: 2 },
  ];
  const weights = { price: 0.4, time: 0.3, stops: 0.3 }; // what this traveller cares about
  const utility = (f: (typeof flights)[number]) =>
    weights.price * (1 - f.price / 500) + weights.time * (1 - f.hours / 15) + weights.stops * (1 - f.stops / 2);

  const bestFlight = tool({
    name: "best_flight",
    description: "The flight with the highest utility for this traveller.",
    input: z.object({}),
    readOnly: true,
    run: () => flights.reduce((a, b) => (utility(b) > utility(a) ? b : a)),
  });
  const agent = new Agent({ model: MODEL, instructions: "Book the best flight. Use best_flight; explain the trade-off.", tools: [bestFlight] });
  console.log((await agent.run("Book Lisbon to Berlin.")).text);
  // [/show utility-based]
}

export async function learning() {
  // [show learning]
  const storage = new Storage("sqlite://learning.db"); // what the agent has learned survives restarts
  const styles = ["short", "detailed", "friendly + steps"];
  type Score = { n: number; mean: number };

  const known = async (style: string): Promise<Score> =>
    ((await storage.recall("styles", style))?.value as Score | undefined) ?? { n: 0, mean: 0 };

  async function reply(question: string, ratingOf: (style: string) => number) {
    const scores = Object.fromEntries(await Promise.all(styles.map(async (s) => [s, await known(s)] as const)));
    const explore = Math.random() < 0.2; // the problem generator: sometimes try something new
    const style = explore ? styles[Math.floor(Math.random() * styles.length)] : styles.reduce((a, b) => (scores[b].mean > scores[a].mean ? b : a));
    const reward = ratingOf(style); // the critic: a customer rating, a test, a metric
    const s = scores[style];
    s.n += 1;
    s.mean += (reward - s.mean) / s.n; // the learning element
    await storage.remember("styles", style, s);
  }
  // [/show learning]
  void reply;
}
