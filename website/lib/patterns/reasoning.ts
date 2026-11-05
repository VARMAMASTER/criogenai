// Single-agent reasoning patterns: ReAct, Chain of Thought, Tree of
// Thoughts, Plan-and-Solve, ReWOO and Reflexion.
import { after, field, score, type PatternDef } from "./engine";

// ------------------------------------------------------------------ tools

const FORECAST: Record<string, string> = {
  lisbon: "24 °C, sunny, wind 8 km/h, 5% chance of rain",
  london: "14 °C, showers from 15:00, 70% chance of rain",
  oslo: "9 °C, cloudy, 20% chance of rain",
};
function weather(city: string): string {
  const key = city.trim().toLowerCase().split(/[ ,]/)[0];
  return `${city.trim() || "?"}: ${FORECAST[key] ?? "19 °C, partly cloudy, 15% chance of rain"}`;
}
function calendar(day: string): string {
  return /sun/i.test(day) ? "Sunday: family lunch 13:00–16:00, free in the morning" : "Saturday: free after 11:00, dinner at 20:00";
}

const FACTS: { keys: string[]; value: string }[] = [
  { keys: ["capital", "france"], value: "Paris" },
  { keys: ["capital", "japan"], value: "Tokyo" },
  { keys: ["population", "paris"], value: "about 2.1 million (city proper)" },
  { keys: ["population", "tokyo"], value: "about 14 million (Tokyo Metropolis)" },
  { keys: ["population", "france"], value: "about 68 million" },
  { keys: ["population", "japan"], value: "about 124 million" },
];
/** A tiny knowledge base: the fact whose keywords all appear in the query. */
function search(query: string): string {
  const q = query.toLowerCase();
  return FACTS.find((f) => f.keys.every((k) => q.includes(k)))?.value ?? "no result";
}
/** Arithmetic only: digits, operators and brackets, nothing else is evaluated. */
function calc(expr: string): string {
  const clean = expr.trim();
  if (!clean || clean.length > 80 || !/^[\d.+\-*/() ]+$/.test(clean)) return "cannot compute (numbers and + - * / only)";
  try {
    const value = Function(`"use strict"; return (${clean});`)() as unknown;
    return typeof value === "number" && Number.isFinite(value) ? String(Math.round(value * 100) / 100) : "cannot compute";
  } catch {
    return "cannot compute";
  }
}

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
  async run(ctx, input) {
    const system = [
      "You are a ReAct agent. Tools: weather(city) returns today's forecast; calendar(day) lists the user's plans for Saturday or Sunday.",
      "Every reply has exactly two lines:",
      "THOUGHT: <one short sentence>",
      "then either ACTION: <tool>: <input>   or   FINAL: <the answer for the user>",
    ].join("\n");
    const script = [
      "THOUGHT: I need the forecast for Lisbon first.\nACTION: weather: Lisbon",
      "THOUGHT: Sunny and warm. Now I check when the user is free.\nACTION: calendar: Saturday",
      "THOUGHT: Saturday afternoon is free and the weather is good.\nFINAL: Yes. Saturday after 11:00 is ideal: 24 °C, sunny, 5% chance of rain. Bring sunscreen.",
    ];
    ctx.at("user");
    ctx.say("user", "user", input, "user");
    let transcript = `Question: ${input}`;
    for (let turn = 0; turn < 6; turn++) {
      const reply = await ctx.ask({ node: "think", who: "agent", system, prompt: transcript, scripted: script[Math.min(turn, script.length - 1)] });
      const final = after(reply, "FINAL");
      const action = field(reply, "ACTION");
      if (final != null || !action) {
        ctx.at("answer");
        return final ?? reply;
      }
      const [name, ...rest] = action.split(":");
      const arg = rest.join(":").trim().replace(/^\(|\)$/g, "") || name.replace(/^.*\(|\)$/g, "");
      const tool = name.trim().toLowerCase().startsWith("cal") ? "calendar" : name.trim().toLowerCase().startsWith("wea") ? "weather" : null;
      const observation = tool
        ? await ctx.tool({ node: tool, name: `${tool}()`, input: arg, run: tool === "weather" ? weather : calendar })
        : `unknown tool "${name.trim()}"`;
      transcript += `\n${reply}\nOBSERVATION: ${observation}`;
    }
    ctx.at("answer");
    return "Stopped after 6 turns without an answer.";
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
  async run(ctx, input) {
    ctx.at("q");
    ctx.say("user", "user", input, "q");
    const reply = await ctx.ask({
      node: "reason",
      who: "model",
      system: "Think step by step. Write your reasoning as short numbered steps, then a last line: ANSWER: <the answer>.",
      prompt: input,
      scripted:
        "1. Let c be coffees and m muffins: c + m = 4.\n2. The cost: 3c + 2m = 10.\n3. From step 1, m = 4 − c, so 3c + 8 − 2c = 10.\n4. That gives c = 2, and m = 2.\n5. Check: 2 × $3 + 2 × $2 = $10.\nANSWER: 2 coffees (and 2 muffins).",
    });
    ctx.at("a");
    return field(reply, "ANSWER") ?? reply;
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
  async run(ctx, input) {
    ctx.at("p");
    ctx.say("user", "user", input, "p");
    const ideas = await ctx.ask({
      node: "propose",
      who: "proposer",
      system: "Propose exactly three clearly different approaches to the problem, one per line, numbered 1. 2. 3. Each line: the approach and one example. No other text.",
      prompt: input,
      scripted:
        "1. A pun on programming terms, e.g. “Java the Hutt”\n2. A developer ritual, e.g. “Stand-up”\n3. A keyboard shortcut, e.g. “Ctrl+Brew”",
    });
    const lines = [...ideas.matchAll(/^\s*\**\s*([1-3])[.)]\s*(.+)$/gm)].map((m) => m[2].trim());
    while (lines.length < 3) lines.push(`(no idea ${lines.length + 1})`);
    const scripted = [
      "SCORE: 6. Fun, but it borrows a film character and is hard to trademark.",
      "SCORE: 7. Relatable for developers, a little generic for a café.",
      "SCORE: 9. Short, memorable, clearly coffee and clearly for developers.",
    ];
    const scores: number[] = [];
    for (let i = 0; i < 3; i++) {
      ctx.at(`t${i + 1}`);
      const reply = await ctx.ask({
        node: "judge",
        who: "evaluator",
        system: "You evaluate one idea for the problem. First line: SCORE: <1-10>, then one sentence why.",
        prompt: `Problem: ${input}\nIdea: ${lines[i]}`,
        scripted: scripted[i],
      });
      scores.push(score(field(reply, "SCORE") ?? reply, 1, 10, 5));
    }
    const best = scores.indexOf(Math.max(...scores));
    ctx.say("tree", "state", `Scores: ${scores.map((s, i) => `idea ${i + 1} = ${s}`).join(", ")}. Growing idea ${best + 1}; the others are pruned.`);
    ctx.at(`t${best + 1}`);
    const final = await ctx.ask({
      node: "grow",
      who: "developer",
      system: "Develop the chosen idea into a final answer: the name, a one-line tagline, and why it works, in at most three sentences.",
      prompt: `Problem: ${input}\nChosen idea: ${lines[best]}`,
      scripted: "Ctrl+Brew. Tagline: “Commit to coffee.” It reads as a keyboard shortcut every developer knows, says coffee in one word, and fits on a cup.",
    });
    return final;
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
  async run(ctx, input) {
    ctx.at("goal");
    ctx.say("user", "user", input, "goal");
    const plan = await ctx.ask({
      node: "plan",
      who: "planner",
      system: "Write a plan of exactly three short numbered steps (1. 2. 3.) to complete the task. Steps only, no other text.",
      prompt: input,
      scripted: "1. List what makes 1970s houses different (insulation, radiators).\n2. Explain how a heat pump performs in those conditions.\n3. Give the conditions under which it is worth it.",
    });
    const steps = [...plan.matchAll(/^\s*\**\s*(\d)[.)]\s*(.+)$/gm)].map((m) => m[2].trim()).slice(0, 4);
    if (!steps.length) steps.push(input);
    const scripted = [
      "Most have thin wall insulation, single or early double glazing, and radiators sized for 70 °C water.",
      "Heat pumps are most efficient at 35–50 °C flow temperatures; with small radiators and heat loss they may struggle on cold days.",
      "Worth it after loft and wall insulation, with a few radiators upsized; otherwise running costs can exceed gas.",
    ];
    const results: string[] = [];
    for (let i = 0; i < steps.length; i++) {
      const out = await ctx.ask({
        node: "exec",
        who: `executor (step ${i + 1})`,
        system: "Carry out one step of a plan. Answer in at most two sentences.",
        prompt: `Task: ${input}\nResults so far:\n${results.join("\n") || "(none)"}\nNow do step ${i + 1}: ${steps[i]}`,
        scripted: scripted[Math.min(i, scripted.length - 1)],
      });
      results.push(`${i + 1}. ${out}`);
      ctx.at("notes");
    }
    const answer = await ctx.ask({
      node: "solve",
      who: "writer",
      system: "Write the final brief from the step results, in at most four sentences, plain language.",
      prompt: `Task: ${input}\nStep results:\n${results.join("\n")}`,
      scripted:
        "Yes, with preparation. A 1970s house usually loses heat fast and has radiators sized for hot water, while heat pumps work best with warm water. Insulate the loft and walls first and upsize a few radiators. Then a heat pump is efficient and usually cheaper to run than gas.",
    });
    ctx.at("done");
    return answer;
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
  async run(ctx, input) {
    ctx.at("task");
    ctx.say("user", "user", input, "task");
    const plan = await ctx.ask({
      node: "planner",
      who: "planner",
      system: [
        "Plan all tool calls needed, before any runs. Tools: search[query] looks up a fact (capitals, populations); calc[expression] does arithmetic.",
        "Write one line per call as: #E1 = search[...]. Later lines may use earlier results, like search[population of #E1].",
        "In calc, use the numbers of earlier results via their #E names only if they are plain numbers; otherwise write the numbers you expect, in millions.",
        "Only the #E lines, nothing else.",
      ].join("\n"),
      prompt: input,
      scripted: "#E1 = search[capital of France]\n#E2 = search[capital of Japan]\n#E3 = search[population of #E1]\n#E4 = search[population of #E2]\n#E5 = calc[14 - 2.1]",
    });
    const evidence: Record<string, string> = {};
    const steps = [...plan.matchAll(/#E(\d+)\s*=\s*(\w+)\s*\[([^\]]*)\]/g)].slice(0, 8);
    if (!steps.length) ctx.say("worker", "note", "The plan had no #E lines; the solver works from the question alone.");
    for (const [, n, toolName, rawArg] of steps) {
      const arg = rawArg.replace(/#E(\d+)/g, (_, k: string) => evidence[`#E${k}`] ?? `#E${k}`);
      const isCalc = toolName.toLowerCase().startsWith("calc");
      evidence[`#E${n}`] = await ctx.tool({ node: isCalc ? "calc" : "search", name: `#E${n} ${isCalc ? "calc" : "search"}`, input: arg, run: isCalc ? calc : search });
    }
    ctx.say("worker", "state", `Evidence: ${Object.entries(evidence).map(([k, v]) => `${k} = ${v}`).join("; ") || "none"}`);
    const answer = await ctx.ask({
      node: "solver",
      who: "solver",
      system: "Answer the task from the plan and the evidence only. At most two sentences.",
      prompt: `Task: ${input}\nPlan:\n${plan}\nEvidence:\n${Object.entries(evidence).map(([k, v]) => `${k} = ${v}`).join("\n")}`,
      scripted: "Tokyo, with about 14 million people against about 2.1 million in Paris (city proper): roughly 11.9 million more.",
    });
    ctx.at("answer");
    return answer;
  },
};

// --------------------------------------------------------------- Reflexion

const CHECKS: [string, (t: string) => boolean][] = [
  ["at most 200 characters", (t) => t.length <= 200],
  ["mentions the price $49", (t) => t.includes("$49")],
  ["no hashtags", (t) => !t.includes("#")],
  ["no stacked exclamation marks", (t) => !/!!/.test(t)],
];

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
  async run(ctx, input) {
    ctx.at("task");
    ctx.say("user", "user", input, "task");
    ctx.say("checks", "note", `The evaluator checks: ${CHECKS.map(([n]) => n).join(", ")}.`);
    const lessons: string[] = [];
    const attempts = [
      "🎧 Meet SoundVeil, our BEST noise-cancelling headphones EVER!! Crystal sound, 30 h battery and all-day comfort for just $49. Grab yours today before they're gone!!! #audio #launch",
      "Meet SoundVeil: noise-cancelling headphones with a 30-hour battery, for $49. Quiet when you want it.",
    ];
    let text = "";
    for (let round = 0; round < 3; round++) {
      text = await ctx.ask({
        node: "actor",
        who: `writer (attempt ${round + 1})`,
        system: "Write one tweet for the task. Reply with the tweet only.",
        prompt: lessons.length ? `${input}\n\nLessons from earlier attempts:\n${lessons.map((l) => `- ${l}`).join("\n")}` : input,
        scripted: attempts[Math.min(round, attempts.length - 1)],
      });
      text = text.replace(/^["“]|["”]$/g, "").trim();
      ctx.at("eval");
      await ctx.pause(300);
      const failed = CHECKS.filter(([, ok]) => !ok(text)).map(([n]) => n);
      ctx.say("checks", failed.length ? "error" : "state", failed.length ? `Failed: ${failed.join(", ")} (${text.length} characters).` : `All four checks pass (${text.length} characters).`, "eval");
      if (!failed.length) {
        ctx.at("done");
        return text;
      }
      if (round === 2) break;
      const lesson = await ctx.ask({
        node: "reflect",
        who: "self-reflection",
        system: "An attempt failed some checks. Write one line starting LESSON: that says what to do differently next time.",
        prompt: `Task: ${input}\nAttempt: ${text}\nFailed checks: ${failed.join(", ")}`,
        scripted: "LESSON: Drop hashtags and stacked exclamation marks; keep one concrete benefit and the price, under 200 characters.",
      });
      lessons.push(field(lesson, "LESSON") ?? lesson);
      ctx.at("memory");
      ctx.say("memory", "state", `Lessons kept: ${lessons.length}`, "memory");
    }
    ctx.at("done");
    return `${text}\n\n(Still failing after 3 attempts: the loop stops at its cap.)`;
  },
};

export const REASONING: PatternDef[] = [react, cot, tot, planSolve, rewoo, reflexion];
