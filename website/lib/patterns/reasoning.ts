// Single-agent reasoning patterns: ReAct, Chain of Thought, Tree of
// Thoughts, Plan-and-Solve, ReWOO and Reflexion.
import type { PatternDef } from "./engine";

// ------------------------------------------------------------------ ReAct

const react: PatternDef = {
  id: "react",
  title: "ReAct",
  group: "Reasoning",
  short: "Think, act with a tool, observe, repeat.",
  what: [
    "ReAct (reason + act) runs one model in a loop. Each turn it writes a short thought, picks a tool, reads what the tool returned, and decides again, until it can answer.",
    "It is the default shape of a tool-calling agent: flexible, easy to follow in a trace, and it stops as soon as it has enough.",
  ],
  use: ["The steps depend on what the tools return", "A handful of tools, one goal", "You want a readable trace of why each call happened"],
  avoid: ["The steps are always the same (a fixed workflow is cheaper)", "Many independent lookups (ReWOO or parallel calls do them at once)"],
  task: "Is this weekend good for a picnic in Lisbon?",
  usesModel: true,
  criogen: { kind: "helper", api: "patterns.react" },
  graph: {
    w: 640,
    h: 240,
    nodes: [
      { id: "user", label: "Question", x: 70, y: 120, kind: "io" },
      { id: "think", label: "Reason", x: 240, y: 120, kind: "model" },
      { id: "weather", label: "weather()", x: 420, y: 55, kind: "tool" },
      { id: "calendar", label: "calendar()", x: 420, y: 185, kind: "tool" },
      { id: "answer", label: "Answer", x: 575, y: 120, kind: "io" },
    ],
    edges: [
      ["user", "think"],
      ["think", "weather", "act"],
      ["weather", "think", "observe"],
      ["think", "calendar", "act"],
      ["calendar", "think", "observe"],
      ["think", "answer", "final"],
    ],
  },
};

// -------------------------------------------------------- Chain of Thought

const cot: PatternDef = {
  id: "chain-of-thought",
  title: "Chain of Thought",
  group: "Reasoning",
  short: "Reason step by step in one call, then answer.",
  what: [
    "Chain of Thought asks the model to write its reasoning as steps before the answer. There are no tools and no loop: one call, with the work shown.",
    "Writing the steps tends to help on arithmetic, logic and multi-part questions, and it makes a wrong answer easy to diagnose.",
  ],
  use: ["Word problems, logic, multi-step questions", "You want to see how the answer was reached", "Cheapest option: a single model call"],
  avoid: ["The answer needs fresh facts (use tools: ReAct)", "Many plausible approaches (Tree of Thoughts explores them)"],
  task: "A café sells coffee for $3 and muffins for $2. Ana buys 4 items and pays $10. How many coffees did she buy?",
  usesModel: true,
  criogen: { kind: "helper", api: "Agent (instructions)" },
  graph: {
    w: 640,
    h: 200,
    nodes: [
      { id: "q", label: "Question", x: 80, y: 100, kind: "io" },
      { id: "reason", label: "Step-by-step", x: 320, y: 100, kind: "model" },
      { id: "a", label: "Answer", x: 560, y: 100, kind: "io" },
    ],
    edges: [
      ["q", "reason"],
      ["reason", "a"],
    ],
  },
};

// -------------------------------------------------------- Tree of Thoughts

const tot: PatternDef = {
  id: "tree-of-thoughts",
  title: "Tree of Thoughts",
  group: "Reasoning",
  short: "Branch into several ideas, score them, grow the best.",
  what: [
    "Tree of Thoughts treats reasoning as a search. The model proposes several different approaches, an evaluator scores each one, and only the most promising branch is developed into the answer.",
    "It costs more calls than Chain of Thought, but it does not bet everything on the first idea.",
  ],
  use: ["Creative or open problems with many possible directions", "Puzzles where a first guess is often a dead end", "You can say what a good branch looks like"],
  avoid: ["One obvious approach (Chain of Thought is enough)", "Tight latency or cost budgets: every branch is extra calls"],
  task: "Name a coffee shop for software developers.",
  usesModel: true,
  criogen: { kind: "primitives", api: "Workflow: fan-out, join, route" },
  graph: {
    w: 640,
    h: 240,
    nodes: [
      { id: "p", label: "Problem", x: 60, y: 120, kind: "io" },
      { id: "propose", label: "Propose", x: 185, y: 120, kind: "model" },
      { id: "t1", label: "Idea 1", x: 320, y: 45, kind: "logic" },
      { id: "t2", label: "Idea 2", x: 320, y: 120, kind: "logic" },
      { id: "t3", label: "Idea 3", x: 320, y: 195, kind: "logic" },
      { id: "judge", label: "Score", x: 455, y: 120, kind: "model" },
      { id: "grow", label: "Grow best", x: 580, y: 120, kind: "model" },
    ],
    edges: [
      ["p", "propose"],
      ["propose", "t1"],
      ["propose", "t2"],
      ["propose", "t3"],
      ["t1", "judge"],
      ["t2", "judge"],
      ["t3", "judge"],
      ["judge", "grow", "best"],
    ],
  },
};

// -------------------------------------------------------- Plan-and-Solve

const planSolve: PatternDef = {
  id: "plan-and-solve",
  title: "Plan-and-Solve",
  group: "Reasoning",
  short: "Write a plan first, then carry out each step.",
  what: [
    "Plan-and-Solve splits the job in two. A planner writes the steps before any work starts; an executor then does one step at a time, each with the results so far; a last call writes the answer.",
    "Planning first keeps long tasks on track and makes the work easy to review: you can read, edit or approve the plan.",
  ],
  use: ["Longer tasks with several distinct parts", "You want to review or approve a plan before work", "Steps are known up front"],
  avoid: ["Each step depends on surprises from the last (ReAct adapts as it goes)", "Very short tasks: the plan costs a call"],
  task: "Write a short brief: is a heat pump a good idea for a 1970s house?",
  usesModel: true,
  criogen: { kind: "primitives", api: "Workflow + Agent nodes (or patterns.sequential)" },
  graph: {
    w: 640,
    h: 220,
    nodes: [
      { id: "goal", label: "Goal", x: 60, y: 110, kind: "io" },
      { id: "plan", label: "Planner", x: 190, y: 110, kind: "model" },
      { id: "exec", label: "Executor", x: 340, y: 110, kind: "model" },
      { id: "notes", label: "Results", x: 340, y: 195, kind: "memory" },
      { id: "solve", label: "Write-up", x: 490, y: 110, kind: "model" },
      { id: "done", label: "Answer", x: 595, y: 110, kind: "io" },
    ],
    edges: [
      ["goal", "plan"],
      ["plan", "exec", "steps"],
      ["exec", "notes"],
      ["notes", "exec", "next step"],
      ["exec", "solve"],
      ["solve", "done"],
    ],
  },
};

// ------------------------------------------------------------------ ReWOO

const rewoo: PatternDef = {
  id: "rewoo",
  title: "ReWOO",
  group: "Reasoning",
  short: "Plan every tool call up front; run them; solve once.",
  what: [
    "ReWOO (reasoning without observation) writes the whole plan, tool calls included, in one model call, using placeholders like #E1 for results that do not exist yet. Workers then run the tools with no model involved, and a solver writes the answer from the evidence.",
    "Two model calls, however many tools: cheaper and faster than ReAct when the lookups are predictable.",
  ],
  use: ["Lookups you can plan before seeing results", "Cost-sensitive pipelines: the model is called twice", "Tool results that should not steer the plan"],
  avoid: ["The next step depends on what a tool returns (ReAct)", "Tools that often fail: the plan cannot adapt mid-way"],
  task: "Which has more people: the capital of France or the capital of Japan, and by how much?",
  usesModel: true,
  criogen: { kind: "primitives", api: "Workflow: planner node, tool nodes, solver node" },
  graph: {
    w: 640,
    h: 240,
    nodes: [
      { id: "task", label: "Task", x: 60, y: 120, kind: "io" },
      { id: "planner", label: "Planner", x: 185, y: 120, kind: "model" },
      { id: "search", label: "search()", x: 335, y: 60, kind: "tool" },
      { id: "calc", label: "calc()", x: 335, y: 180, kind: "tool" },
      { id: "solver", label: "Solver", x: 475, y: 120, kind: "model" },
      { id: "answer", label: "Answer", x: 590, y: 120, kind: "io" },
    ],
    edges: [
      ["task", "planner"],
      ["planner", "search", "#E1…"],
      ["planner", "calc"],
      ["search", "solver"],
      ["calc", "solver"],
      ["solver", "answer"],
    ],
  },
};

// --------------------------------------------------------------- Reflexion

const reflexion: PatternDef = {
  id: "reflexion",
  title: "Reflexion",
  group: "Reasoning",
  short: "Try, test, write a lesson, try again with it.",
  what: [
    "Reflexion adds memory of mistakes. The agent attempts the task, an evaluator checks the result, and on failure the agent writes a short lesson in its own words. The next attempt sees every lesson so far.",
    "The evaluator here is real code, four checks on the tweet, so the loop ends on facts, not on the model's opinion of itself.",
  ],
  use: ["You can check the result automatically (tests, rules, a schema)", "Mistakes repeat in recognisable ways", "A few retries are affordable"],
  avoid: ["No reliable check exists (the loop just guesses)", "One attempt is usually right"],
  task: "Write a launch tweet for our $49 noise-cancelling headphones, SoundVeil.",
  usesModel: true,
  criogen: { kind: "helper", api: "patterns.loop (or review_and_critique)" },
  graph: {
    w: 640,
    h: 240,
    nodes: [
      { id: "task", label: "Task", x: 60, y: 90, kind: "io" },
      { id: "actor", label: "Attempt", x: 200, y: 90, kind: "model" },
      { id: "eval", label: "Checks", x: 350, y: 90, kind: "logic" },
      { id: "reflect", label: "Reflect", x: 350, y: 195, kind: "model" },
      { id: "memory", label: "Lessons", x: 200, y: 195, kind: "memory" },
      { id: "done", label: "Answer", x: 545, y: 90, kind: "io" },
    ],
    edges: [
      ["task", "actor"],
      ["actor", "eval"],
      ["eval", "reflect", "fail"],
      ["reflect", "memory"],
      ["memory", "actor", "retry"],
      ["eval", "done", "pass"],
    ],
  },
};

export const REASONING: PatternDef[] = [react, cot, tot, planSolve, rewoo, reflexion];
