// Multi-agent patterns: router, supervisor, hierarchy, collaboration,
// debate, parallel fan-out, swarm, and a person in the loop.
import type { PatternDef } from "./engine";

// ----------------------------------------------------------------- Router

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
  criogenai: { kind: "primitives", api: "Workflow.route" },
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
};

// -------------------------------------------------------------- Supervisor

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
  criogenai: { kind: "helper", api: "patterns.coordinator" },
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
  criogenai: { kind: "helper", api: "patterns.hierarchical" },
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
  criogenai: { kind: "helper", api: "patterns.sequential" },
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
  criogenai: { kind: "primitives", api: "Workflow: two agent nodes in a loop + judge (or patterns.review_and_critique)" },
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
  criogenai: { kind: "helper", api: "patterns.parallel" },
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
};

// ------------------------------------------------------------------- Swarm

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
  criogenai: { kind: "helper", api: "patterns.swarm" },
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
};

// --------------------------------------------------------- Human in the loop

const human: PatternDef = {
  id: "human-in-the-loop",
  title: "Human in the loop",
  group: "Multi-agent",
  short: "The agent drafts; a person approves before anything happens.",
  what: [
    "The run pauses before a step that matters, here sending a refund email, and waits for a person to approve, edit or reject the draft. Nothing is sent until they decide.",
    "In CriogenAI the pause is durable: it is saved in the database, so the decision can come from another process, days later, and is recorded in the audit trail.",
  ],
  use: ["Money, messages to customers, deletions, anything hard to undo", "Regulated work that needs a named approver", "Building trust in a new agent"],
  avoid: ["High-volume, low-risk steps (people become the bottleneck)"],
  task: "A customer asks for a $180 refund on a damaged order (A-2077).",
  usesModel: true,
  criogenai: { kind: "helper", api: "patterns.human_review (Approval on tools)" },
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
};

export const MULTI: PatternDef[] = [router, supervisor, hierarchical, collaboration, debate, parallel, swarm, human];
