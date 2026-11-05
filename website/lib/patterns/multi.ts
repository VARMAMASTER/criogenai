// Multi-agent patterns: router, supervisor, hierarchy, collaboration,
// debate, parallel fan-out, swarm, and a person in the loop.
import { after, field, pick, type PatternDef } from "./engine";

// ----------------------------------------------------------------- Router

const ROUTES = ["billing", "technical", "sales"];

const router: PatternDef = {
  id: "router",
  title: "Router",
  group: "Multi-agent",
  short: "One call picks the specialist; only that one runs.",
  what: [
    "A router reads the request once and sends it down exactly one branch. The specialists never see requests that are not theirs, and the branches that are not chosen cost nothing.",
    "The choice can be a model call, as here, or plain code (keywords, a classifier). Either way the paths are fixed in the graph, so the run is easy to predict and test.",
  ],
  use: ["Clear categories of request", "Specialists with different tools or permissions", "Predictable cost: one routing call plus one branch"],
  avoid: ["A request needs several specialists (Supervisor)", "Categories overlap heavily"],
  task: "I was charged twice for my subscription this month.",
  usesModel: true,
  criogen: { kind: "primitives", api: "Workflow.route" },
  graph: {
    w: 640,
    h: 240,
    nodes: [
      { id: "req", label: "Request", x: 60, y: 120, kind: "io" },
      { id: "route", label: "Router", x: 200, y: 120, kind: "model" },
      { id: "billing", label: "Billing", x: 380, y: 45, kind: "agent" },
      { id: "technical", label: "Technical", x: 380, y: 120, kind: "agent" },
      { id: "sales", label: "Sales", x: 380, y: 195, kind: "agent" },
      { id: "out", label: "Reply", x: 570, y: 120, kind: "io" },
    ],
    edges: [
      ["req", "route"],
      ["route", "billing", "billing"],
      ["route", "technical", "technical"],
      ["route", "sales", "sales"],
      ["billing", "out"],
      ["technical", "out"],
      ["sales", "out"],
    ],
  },
  async run(ctx, input) {
    ctx.at("req");
    ctx.say("user", "user", input, "req");
    const choice = await ctx.ask({
      node: "route",
      who: "router",
      system: `Classify the customer request into exactly one of: ${ROUTES.join(", ")}. Reply with one line: ROUTE: <name>.`,
      prompt: input,
      scripted: "ROUTE: billing",
    });
    const name = pick(field(choice, "ROUTE") ?? choice, ROUTES, "technical");
    ctx.say("router", "state", `→ ${name}. The other two branches do not run.`);
    const reply = await ctx.ask({
      node: name,
      who: `${name} agent`,
      system: `You are the ${name} support specialist. Reply to the customer in at most three sentences.`,
      prompt: input,
      scripted:
        "Sorry about that. I can see two charges of $12 on 3 November; the second is a duplicate and I have refunded it. It will be back on your card within 5 working days.",
    });
    ctx.at("out");
    return reply;
  },
};

// -------------------------------------------------------------- Supervisor

const WORKERS = ["researcher", "writer", "reviewer"];

const supervisor: PatternDef = {
  id: "supervisor",
  title: "Supervisor",
  group: "Multi-agent",
  short: "A coordinator calls workers in turn until done.",
  what: [
    "A supervisor agent holds the goal and decides, turn by turn, which worker to call next and with what instruction. Workers report back; the supervisor reads the result and either calls another worker or answers.",
    "Unlike a router it can call several workers, in any order, more than once. The price is a model call for every decision.",
  ],
  use: ["Requests that need several specialists", "The order of work is not known in advance", "Workers with separate prompts, tools or models"],
  avoid: ["A fixed order always works (a sequential pipeline is cheaper)", "Tight latency budgets"],
  task: "Give me three bullet points on why teams adopt TypeScript.",
  usesModel: true,
  criogen: { kind: "helper", api: "patterns.coordinator" },
  graph: {
    w: 640,
    h: 240,
    nodes: [
      { id: "req", label: "Request", x: 60, y: 120, kind: "io" },
      { id: "sup", label: "Supervisor", x: 220, y: 120, kind: "model" },
      { id: "researcher", label: "Researcher", x: 410, y: 45, kind: "agent" },
      { id: "writer", label: "Writer", x: 410, y: 120, kind: "agent" },
      { id: "reviewer", label: "Reviewer", x: 410, y: 195, kind: "agent" },
      { id: "out", label: "Answer", x: 580, y: 120, kind: "io" },
    ],
    edges: [
      ["req", "sup"],
      ["sup", "researcher"],
      ["researcher", "sup"],
      ["sup", "writer"],
      ["writer", "sup"],
      ["sup", "reviewer"],
      ["reviewer", "sup"],
      ["sup", "out", "final"],
    ],
  },
  async run(ctx, input) {
    ctx.at("req");
    ctx.say("user", "user", input, "req");
    const system = [
      `You are a supervisor with workers: researcher (finds facts), writer (writes text), reviewer (checks text).`,
      "Each turn reply with one line: CALL: <worker>: <instruction>   or   FINAL: <the answer for the user>.",
      "Call at most three workers in total, then answer.",
    ].join("\n");
    const script = [
      "CALL: researcher: Find the main reasons teams adopt TypeScript.",
      "CALL: writer: Turn the research into exactly three short bullet points.",
      "FINAL: ",
    ];
    const workerScript: Record<string, string> = {
      researcher: "Main reasons: type errors caught before running; editor autocomplete and safe refactors; types document APIs; gradual adoption from JavaScript.",
      writer: "• Catches type errors before code runs\n• Makes refactoring safe, with autocomplete that knows your code\n• Types document the code, and you can adopt them file by file",
      reviewer: "Looks good: three bullets, accurate, no jargon.",
    };
    let history = `Request: ${input}`;
    let lastWork = "";
    for (let turn = 0; turn < 5; turn++) {
      const reply = await ctx.ask({ node: "sup", who: "supervisor", system, prompt: history, scripted: script[Math.min(turn, script.length - 1)] });
      const final = after(reply, "FINAL");
      const call = field(reply, "CALL");
      if (final != null || !call) {
        ctx.at("out");
        return final?.trim() || lastWork || reply;
      }
      const worker = pick(call.split(":")[0], WORKERS, "writer");
      const instruction = call.split(":").slice(1).join(":").trim() || call;
      const work = await ctx.ask({
        node: worker,
        who: worker,
        system: `You are the ${worker}. Do only what the instruction says, briefly.`,
        prompt: `${instruction}\n\nContext so far:\n${history}`,
        scripted: workerScript[worker],
      });
      lastWork = work;
      history += `\n${worker} replied: ${work}`;
    }
    ctx.at("out");
    return lastWork;
  },
};

// ------------------------------------------------------------ Hierarchical

const hierarchical: PatternDef = {
  id: "hierarchical",
  title: "Hierarchical teams",
  group: "Multi-agent",
  short: "A manager splits work across team leads, who delegate.",
  what: [
    "A hierarchy is supervisors of supervisors. A manager splits the goal into team briefs; each team lead turns its brief into a task for its own worker; results flow back up and the manager decides.",
    "Each agent sees only its level: the manager never reads raw worker output it does not need, which keeps prompts short as the system grows.",
  ],
  use: ["Big problems with separable areas (research, finance, legal…)", "Teams with their own tools and rules", "Keeping each prompt small"],
  avoid: ["Small tasks: every level adds calls and latency", "Areas that must constantly talk to each other"],
  task: "Should our bakery open a second shop downtown?",
  usesModel: true,
  criogen: { kind: "helper", api: "patterns.hierarchical" },
  graph: {
    w: 640,
    h: 250,
    nodes: [
      { id: "goal", label: "Goal", x: 60, y: 125, kind: "io" },
      { id: "mgr", label: "Manager", x: 190, y: 125, kind: "model" },
      { id: "rlead", label: "Research lead", x: 345, y: 60, kind: "agent" },
      { id: "flead", label: "Finance lead", x: 345, y: 190, kind: "agent" },
      { id: "rworker", label: "Footfall analyst", x: 510, y: 60, kind: "agent" },
      { id: "fworker", label: "Cost analyst", x: 510, y: 190, kind: "agent" },
      { id: "out", label: "Decision", x: 190, y: 220, kind: "io" },
    ],
    edges: [
      ["goal", "mgr"],
      ["mgr", "rlead"],
      ["mgr", "flead"],
      ["rlead", "rworker"],
      ["flead", "fworker"],
      ["rworker", "rlead"],
      ["fworker", "flead"],
      ["rlead", "mgr"],
      ["flead", "mgr"],
      ["mgr", "out"],
    ],
  },
  async run(ctx, input) {
    ctx.at("goal");
    ctx.say("user", "user", input, "goal");
    const split = await ctx.ask({
      node: "mgr",
      who: "manager",
      system: "You manage two teams: research and finance. Split the goal into one brief per team, as two lines: RESEARCH: <brief> and FINANCE: <brief>.",
      prompt: input,
      scripted: "RESEARCH: Estimate morning and lunchtime footfall downtown and who the competitors are.\nFINANCE: Estimate rent, staffing and break-even for a small downtown shop.",
    });
    const teams = [
      {
        lead: "rlead",
        worker: "rworker",
        name: "research",
        brief: field(split, "RESEARCH") ?? input,
        leadScript: "TASK: Count weekday footfall near the station from 7 to 10 and 12 to 14, and list bakeries within 300 m.",
        workerScript: "About 2,400 people pass per weekday morning; two bakeries nearby, both closed before 8:00.",
      },
      {
        lead: "flead",
        worker: "fworker",
        name: "finance",
        brief: field(split, "FINANCE") ?? input,
        leadScript: "TASK: Work out monthly costs for a 40 m² shop with two staff and the daily sales needed to break even.",
        workerScript: "Rent $3,200 and staff $6,800 a month; break-even at about 330 sales a day at an average of $5.",
      },
    ];
    const reports: string[] = [];
    for (const t of teams) {
      const task = await ctx.ask({
        node: t.lead,
        who: `${t.name} lead`,
        system: "You lead a team of one analyst. Turn the brief into one precise task for them: one line, TASK: <task>.",
        prompt: t.brief,
        scripted: t.leadScript,
      });
      const result = await ctx.ask({
        node: t.worker,
        who: t.name === "research" ? "footfall analyst" : "cost analyst",
        system: "You are an analyst. Do the task and answer with plausible estimates in at most two sentences.",
        prompt: field(task, "TASK") ?? task,
        scripted: t.workerScript,
      });
      ctx.at(t.lead);
      reports.push(`${t.name}: ${result}`);
    }
    const decision = await ctx.ask({
      node: "mgr",
      who: "manager",
      system: "Decide from your teams' reports. Answer in at most three sentences, starting with Yes, No or Not yet.",
      prompt: `Goal: ${input}\nReports:\n${reports.join("\n")}`,
      scripted: "Yes, with an early opening. 2,400 morning passers-by and no bakery open before 8:00 is a gap; 330 sales a day is about 14% of them. Open at 6:30 and review after three months.",
    });
    ctx.at("out");
    return decision;
  },
};

// ----------------------------------------------------------- Collaboration

const collaboration: PatternDef = {
  id: "collaboration",
  title: "Multi-agent collaboration",
  group: "Multi-agent",
  short: "Specialists pass the work along a pipeline.",
  what: [
    "Collaboration in its simplest form: each agent has one job and hands its output to the next. Here a researcher gathers facts, a writer turns them into text for the audience, and an editor tightens it.",
    "Each agent can have its own prompt, model and tools. Because the order is fixed, the run is predictable and each stage is easy to test on its own.",
  ],
  use: ["Work that naturally flows through stages", "Different skills per stage (or a cheaper model for one)", "Easy-to-test, predictable runs"],
  avoid: ["Stages that need to go back and forth (Debate, Reflexion)", "When one capable agent can do it all in one call"],
  task: "Explain why the sky is blue, for a 10-year-old.",
  usesModel: true,
  criogen: { kind: "helper", api: "patterns.sequential" },
  graph: {
    w: 640,
    h: 180,
    nodes: [
      { id: "in", label: "Request", x: 60, y: 90, kind: "io" },
      { id: "researcher", label: "Researcher", x: 190, y: 90, kind: "agent" },
      { id: "writer", label: "Writer", x: 330, y: 90, kind: "agent" },
      { id: "editor", label: "Editor", x: 470, y: 90, kind: "agent" },
      { id: "out", label: "Answer", x: 590, y: 90, kind: "io" },
    ],
    edges: [
      ["in", "researcher"],
      ["researcher", "writer", "facts"],
      ["writer", "editor", "draft"],
      ["editor", "out"],
    ],
  },
  async run(ctx, input) {
    ctx.at("in");
    ctx.say("user", "user", input, "in");
    const facts = await ctx.ask({
      node: "researcher",
      who: "researcher",
      system: "List the key facts needed to answer, as three short bullet points. Facts only.",
      prompt: input,
      scripted: "• Sunlight contains all colours.\n• Air molecules scatter short (blue) wavelengths much more than long (red) ones: Rayleigh scattering.\n• Scattered blue light reaches our eyes from every direction of the sky.",
    });
    const draft = await ctx.ask({
      node: "writer",
      who: "writer",
      system: "Write the answer for the audience in the request, using only these facts. At most five sentences.",
      prompt: `Request: ${input}\nFacts:\n${facts}`,
      scripted:
        "Sunlight looks white, but it is really all the colours mixed together. When it hits the tiny bits of air above us, the blue part bounces around much more than the red part. That bounced blue light comes at us from all over the sky. So wherever you look up, you see blue!",
    });
    const final = await ctx.ask({
      node: "editor",
      who: "editor",
      system: "Edit the draft: keep the meaning, make it shorter and clearer. Reply with the edited text only.",
      prompt: draft,
      scripted: "Sunlight is all the colours mixed together. When it hits the tiny bits of air above us, blue light bounces around far more than red. That bouncing blue reaches your eyes from every part of the sky, so the sky looks blue.",
    });
    ctx.at("out");
    return final;
  },
};

// ------------------------------------------------------------------ Debate

const debate: PatternDef = {
  id: "debate",
  title: "Debate",
  group: "Multi-agent",
  short: "Two agents argue opposite sides; a judge decides.",
  what: [
    "Two agents take opposite positions and answer each other for a fixed number of rounds; a judge reads the exchange and decides. Each side is pushed to find the other's weak points, which a single agent tends to skip.",
    "Useful when the risk is a one-sided answer: decisions, reviews, policies.",
  ],
  use: ["Decisions with real trade-offs", "Stress-testing a plan or an argument", "Reducing one-sided answers"],
  avoid: ["Questions with a single factual answer", "Tight budgets: rounds × sides calls"],
  task: "Should a 5-person startup build its own LLM gateway?",
  usesModel: true,
  criogen: { kind: "primitives", api: "Workflow: two agent nodes in a loop + judge (or patterns.review_and_critique)" },
  graph: {
    w: 640,
    h: 230,
    nodes: [
      { id: "q", label: "Question", x: 60, y: 115, kind: "io" },
      { id: "pro", label: "For", x: 240, y: 50, kind: "agent" },
      { id: "con", label: "Against", x: 240, y: 180, kind: "agent" },
      { id: "judge", label: "Judge", x: 430, y: 115, kind: "model" },
      { id: "out", label: "Verdict", x: 580, y: 115, kind: "io" },
    ],
    edges: [
      ["q", "pro"],
      ["pro", "con", "rebut"],
      ["con", "pro", "rebut"],
      ["pro", "judge"],
      ["con", "judge"],
      ["judge", "out"],
    ],
  },
  async run(ctx, input) {
    ctx.at("q");
    ctx.say("user", "user", input, "q");
    const script = {
      pro: [
        "For: one gateway gives every product one place for keys, fallbacks and spend limits. Off-the-shelf options charge per request and still need configuring.",
        "Owning it also means no third party sees prompts, which matters for customer data.",
      ],
      con: [
        "Against: five people should ship product, not infrastructure. A gateway needs rate limits, streaming, retries and on-call; that is weeks of work and ongoing upkeep.",
        "Privacy is solvable with a self-hosted open gateway; building one from scratch is the costly path.",
      ],
    };
    let log = `Question: ${input}`;
    for (let round = 0; round < 2; round++) {
      for (const side of ["pro", "con"] as const) {
        const out = await ctx.ask({
          node: side,
          who: side === "pro" ? "for" : "against",
          system: `You argue ${side === "pro" ? "FOR" : "AGAINST"} in a debate. Reply to the other side in at most two sentences.`,
          prompt: log,
          scripted: script[side][round],
        });
        log += `\n${side === "pro" ? "For" : "Against"}: ${out}`;
      }
    }
    const verdict = await ctx.ask({
      node: "judge",
      who: "judge",
      system: "Judge the debate. Give the verdict and the deciding reason in at most two sentences.",
      prompt: log,
      scripted: "Against wins: at five people the upkeep outweighs the savings. Use a self-hosted existing gateway now and revisit when traffic or compliance demands it.",
    });
    ctx.at("out");
    return verdict;
  },
};

// ---------------------------------------------------------------- Parallel

const parallel: PatternDef = {
  id: "parallel",
  title: "Parallel fan-out",
  group: "Multi-agent",
  short: "Several agents work at once; one combines.",
  what: [
    "Fan-out sends the same input to several agents at the same time and waits for all of them; a synthesizer combines their answers. The wall-clock time is that of the slowest branch, not the sum.",
    "Each branch does one narrow job, so its prompt stays short and its output is easy to check.",
  ],
  use: ["Independent analyses of the same input", "Latency matters: branches overlap", "Ensembles: several opinions, one answer"],
  avoid: ["Branches that need each other's results (chain them instead)", "Rate-limited APIs: all calls land at once"],
  task: "Review: “Battery lasts forever, but the app keeps logging me out and support took 4 days to reply.”",
  usesModel: true,
  criogen: { kind: "helper", api: "patterns.parallel" },
  graph: {
    w: 640,
    h: 240,
    nodes: [
      { id: "in", label: "Input", x: 60, y: 120, kind: "io" },
      { id: "sentiment", label: "Sentiment", x: 240, y: 45, kind: "agent" },
      { id: "topics", label: "Topics", x: 240, y: 120, kind: "agent" },
      { id: "reply", label: "Reply draft", x: 240, y: 195, kind: "agent" },
      { id: "synth", label: "Synthesizer", x: 430, y: 120, kind: "model" },
      { id: "out", label: "Report", x: 580, y: 120, kind: "io" },
    ],
    edges: [
      ["in", "sentiment"],
      ["in", "topics"],
      ["in", "reply"],
      ["sentiment", "synth"],
      ["topics", "synth"],
      ["reply", "synth"],
      ["synth", "out"],
    ],
  },
  async run(ctx, input) {
    ctx.at("in");
    ctx.say("user", "user", input, "in");
    const branches = [
      { id: "sentiment", system: "Give the overall sentiment (positive, mixed, negative) and one reason. One line.", scripted: "Mixed: loves the battery, frustrated by logouts and slow support." },
      { id: "topics", system: "List the topics mentioned, comma-separated, with + or − for each.", scripted: "battery +, app login −, support response time −" },
      { id: "reply", system: "Draft a two-sentence reply from the company to this customer.", scripted: "Thank you, we are glad the battery delivers! Sorry about the logouts and the wait: a fix ships this week, and I have flagged your account for a follow-up today." },
    ];
    ctx.say("fan-out", "note", "All three branches start now, together.");
    const results = await Promise.all(branches.map((b) => ctx.ask({ node: b.id, who: b.id, system: b.system, prompt: input, scripted: b.scripted })));
    const report = await ctx.ask({
      node: "synth",
      who: "synthesizer",
      system: "Combine the three analyses into a short report for the product team: one line each for sentiment, topics, and suggested reply.",
      prompt: branches.map((b, i) => `${b.id}: ${results[i]}`).join("\n"),
      scripted: `Sentiment: mixed.\nTopics: battery (+), app login (−), support speed (−).\nReply: ${branches[2].scripted}`,
    });
    ctx.at("out");
    return report;
  },
};

// ------------------------------------------------------------------- Swarm

const SWARM = ["triage", "orders", "refunds"];

const swarm: PatternDef = {
  id: "swarm",
  title: "Swarm",
  group: "Multi-agent",
  short: "Agents hand off to each other; no one is in charge.",
  what: [
    "In a swarm there is no supervisor. Each agent does its part and either hands the conversation to a peer it knows about or answers. Control moves around the team the way a call moves between departments.",
    "Hand-offs are capped and checked: a hand-off to a name that does not exist ends the run instead of looping.",
  ],
  use: ["Conversations that move between departments", "Agents that each own a slice of tools and data", "No single agent should see everything"],
  avoid: ["You need one place that decides (Supervisor)", "Strict audit of who decided what (a central coordinator is easier to trace)"],
  task: "I returned my headphones two weeks ago and still have no refund.",
  usesModel: true,
  criogen: { kind: "helper", api: "patterns.swarm" },
  graph: {
    w: 640,
    h: 240,
    nodes: [
      { id: "user", label: "Customer", x: 60, y: 120, kind: "io" },
      { id: "triage", label: "Triage", x: 210, y: 120, kind: "agent" },
      { id: "orders", label: "Orders", x: 370, y: 50, kind: "agent" },
      { id: "lookup", label: "lookup_order()", x: 540, y: 50, kind: "tool" },
      { id: "refunds", label: "Refunds", x: 370, y: 190, kind: "agent" },
      { id: "done", label: "Answer", x: 540, y: 190, kind: "io" },
    ],
    edges: [
      ["user", "triage"],
      ["triage", "orders", "handoff"],
      ["orders", "lookup"],
      ["lookup", "orders"],
      ["orders", "refunds", "handoff"],
      ["refunds", "triage"],
      ["refunds", "done", "final"],
    ],
  },
  async run(ctx, input) {
    ctx.at("user");
    ctx.say("user", "user", input, "user");
    const script: Record<string, string> = {
      triage: "This is about a returned order. Passing to the orders team.\nHANDOFF: orders",
      orders: "Order A-1001: the return arrived at the warehouse on 24 October and passed inspection, but no refund was created.\nHANDOFF: refunds",
      refunds: "I have issued the refund of $49 to your original card today; it shows within 3–5 working days. Sorry for the wait.\nFINAL",
    };
    let current = "triage";
    let work = "";
    for (let hop = 0; hop < 6; hop++) {
      let extra = "";
      if (current === "orders") {
        const found = await ctx.tool({
          node: "lookup",
          name: "lookup_order()",
          input: "customer's latest return",
          run: () => "A-1001 · headphones $49 · return received 24 Oct · inspection passed · refund: none",
        });
        extra = `\nTool lookup_order returned: ${found}`;
      }
      const others = SWARM.filter((n) => n !== current);
      const reply = await ctx.ask({
        node: current,
        who: current,
        system: `You are the ${current} agent of a support team. You can hand off to: ${others.join(", ")}. Do your part in at most two sentences, then end with exactly one line: HANDOFF: <name>, or FINAL if the customer has their answer.`,
        prompt: `Customer: ${input}\nWork so far: ${work || "(none)"}${extra}`,
        scripted: script[current],
      });
      const lines = reply.trim().split("\n").map((l) => l.trim()).filter(Boolean);
      const tail = lines[lines.length - 1] ?? "FINAL";
      work += `\n${current}: ${lines.slice(0, -1).join(" ") || reply}`;
      const target = field(tail, "HANDOFF");
      if (target == null) {
        ctx.at("done");
        return lines.slice(0, -1).join(" ") || reply;
      }
      const next = SWARM.find((n) => target.toLowerCase().includes(n));
      if (!next || next === current) {
        ctx.say("swarm", "error", `Hand-off to unknown agent “${target}”: the run stops (unknown_handoff).`);
        ctx.at("done");
        return lines.slice(0, -1).join(" ") || reply;
      }
      ctx.say("swarm", "state", `${current} → ${next}`);
      current = next;
    }
    ctx.say("swarm", "error", "Hand-off cap reached: the run stops (max_handoffs).");
    ctx.at("done");
    return work.trim();
  },
};

// --------------------------------------------------------- Human in the loop

const human: PatternDef = {
  id: "human-in-the-loop",
  title: "Human in the loop",
  group: "Multi-agent",
  short: "The agent drafts; a person approves before anything happens.",
  what: [
    "The run pauses before a step that matters, here sending a refund email, and waits for a person to approve, edit or reject the draft. Nothing is sent until they decide.",
    "In Criogen the pause is durable: it is saved in the database, so the decision can come from another process, days later, and is recorded in the audit trail.",
  ],
  use: ["Money, messages to customers, deletions, anything hard to undo", "Regulated work that needs a named approver", "Building trust in a new agent"],
  avoid: ["High-volume, low-risk steps (people become the bottleneck)"],
  task: "A customer asks for a $180 refund on a damaged order (A-2077).",
  usesModel: true,
  criogen: { kind: "helper", api: "patterns.human_review (Approval on tools)" },
  graph: {
    w: 640,
    h: 220,
    nodes: [
      { id: "req", label: "Request", x: 60, y: 110, kind: "io" },
      { id: "draft", label: "Draft", x: 200, y: 110, kind: "model" },
      { id: "person", label: "You decide", x: 350, y: 110, kind: "human" },
      { id: "send", label: "send_email()", x: 500, y: 50, kind: "tool" },
      { id: "stop", label: "Not sent", x: 500, y: 175, kind: "io" },
    ],
    edges: [
      ["req", "draft"],
      ["draft", "person", "pause"],
      ["person", "send", "approve / edit"],
      ["person", "stop", "reject"],
    ],
  },
  async run(ctx, input) {
    ctx.at("req");
    ctx.say("user", "user", input, "req");
    const draft = await ctx.ask({
      node: "draft",
      who: "agent",
      system: "Draft a short email (at most four sentences) to the customer approving the refund. Email body only.",
      prompt: input,
      scripted:
        "Hello, I am sorry your order A-2077 arrived damaged. I have approved a full refund of $180 to your original payment method; it will appear within 5 working days. You do not need to send the item back.",
    });
    ctx.at("person");
    ctx.say("run", "human", "Paused. Nothing has been sent. Approve, edit or reject the draft below.", "person");
    const decision = await ctx.human(draft);
    if (decision.kind === "reject") {
      ctx.say("you", "human", "Rejected.", "person");
      ctx.at("stop");
      return "Rejected: the email was not sent and no refund was issued.";
    }
    const text = decision.kind === "edit" ? decision.text : draft;
    ctx.say("you", "human", decision.kind === "edit" ? "Edited and approved." : "Approved.", "person");
    await ctx.tool({ node: "send", name: "send_email()", input: `to customer of A-2077 (${text.length} characters)`, run: () => "queued (simulated: nothing leaves your browser)" });
    return text;
  },
};

export const MULTI: PatternDef[] = [router, supervisor, hierarchical, collaboration, debate, parallel, swarm, human];
