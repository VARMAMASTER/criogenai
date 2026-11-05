// IBM's five classic agent types (https://www.ibm.com/think/topics/ai-agent-types):
// simple reflex, model-based reflex, goal-based, utility-based, learning.
// They describe how an agent decides; none of these needs a model, so they
// run the same with or without a key.
import { rng, type PatternDef } from "./engine";

// ------------------------------------------------------------ simple reflex

const simpleReflex: PatternDef = {
  id: "simple-reflex",
  title: "Simple reflex agent",
  group: "IBM agent types",
  short: "If this, then that: rules on the current reading only.",
  what: [
    "A simple reflex agent maps what it senses right now to an action with fixed condition-action rules. It keeps no memory and has no goal beyond its rules: the same reading always gives the same action.",
    "A thermostat is the classic example. Change the target with the slider and run it again.",
  ],
  use: ["The current reading is enough to act", "Rules are known and stable", "Speed and predictability matter most"],
  avoid: ["The world is only partly visible (it cannot remember)", "Situations the rules do not cover"],
  task: "Keep the living room comfortable.",
  usesModel: false,
  criogen: { kind: "primitives", api: "Workflow.route (a router is a rule)" },
  params: [{ id: "target", label: "Target °C", min: 16, max: 26, value: 21 }],
  graph: {
    w: 640,
    h: 200,
    nodes: [
      { id: "sensor", label: "Sensor", x: 70, y: 100, kind: "io" },
      { id: "rules", label: "Rules", x: 250, y: 100, kind: "logic" },
      { id: "heat", label: "Heat", x: 440, y: 40, kind: "tool" },
      { id: "idle", label: "Idle", x: 440, y: 100, kind: "tool" },
      { id: "cool", label: "Cool", x: 440, y: 160, kind: "tool" },
    ],
    edges: [
      ["sensor", "rules", "reading"],
      ["rules", "heat", "< target − 1"],
      ["rules", "idle"],
      ["rules", "cool", "> target + 1"],
    ],
  },
  async run(ctx) {
    const target = ctx.params.target ?? 21;
    const readings = [17.5, 19.2, 21.0, 23.6, 22.4, 20.6, 19.4];
    const counts = { heat: 0, idle: 0, cool: 0 };
    ctx.say("rules", "note", `IF t < ${target - 1} THEN heat · IF t > ${target + 1} THEN cool · ELSE idle`);
    for (const t of readings) {
      ctx.at("sensor");
      ctx.say("sensor", "state", `${t.toFixed(1)} °C`, "sensor");
      await ctx.pause(350);
      ctx.at("rules");
      await ctx.pause(250);
      const action = t < target - 1 ? "heat" : t > target + 1 ? "cool" : "idle";
      counts[action]++;
      ctx.at(action);
      ctx.say("actuator", "tool", action, action);
      await ctx.pause(350);
    }
    return `7 readings → heat ${counts.heat}×, idle ${counts.idle}×, cool ${counts.cool}×. No memory and no model: each action came from the reading alone.`;
  },
};

// ------------------------------------------------------- model-based reflex

const modelReflex: PatternDef = {
  id: "model-based-reflex",
  title: "Model-based reflex agent",
  group: "IBM agent types",
  short: "Rules plus an internal picture of what it cannot see.",
  what: [
    "A model-based reflex agent still acts on rules, but it keeps an internal model of the world, updated from what it senses and what its own actions do. That lets it act sensibly when it can only see part of the world.",
    "This robot vacuum only senses the room it is in. Its memory of which rooms are clean is what lets it stop; a simple reflex vacuum would never know it was done.",
  ],
  use: ["The world is only partly observable", "State changes slowly and predictably", "Rules still describe what to do"],
  avoid: ["Choices between competing goals (goal- or utility-based)"],
  task: "Clean all four rooms.",
  usesModel: false,
  criogen: { kind: "primitives", api: "Workflow state (the internal model) + route" },
  graph: {
    w: 640,
    h: 220,
    nodes: [
      { id: "sensor", label: "Sensor", x: 70, y: 110, kind: "io" },
      { id: "model", label: "World model", x: 230, y: 110, kind: "memory" },
      { id: "rules", label: "Rules", x: 390, y: 110, kind: "logic" },
      { id: "act", label: "Act", x: 550, y: 110, kind: "tool" },
    ],
    edges: [
      ["sensor", "model", "percept"],
      ["model", "rules", "state"],
      ["rules", "act"],
      ["act", "model", "what my action changed"],
    ],
  },
  async run(ctx) {
    const rooms = ["A", "B", "C", "D"];
    const dirty: Record<string, boolean> = { A: true, B: false, C: true, D: true };
    const known: Record<string, "?" | "clean"> = { A: "?", B: "?", C: "?", D: "?" };
    let at = 0;
    const map = () => rooms.map((r, i) => `${i === at ? "[" : " "}${r}:${known[r] === "clean" ? "✓" : "?"}${i === at ? "]" : " "}`).join(" ");
    for (let step = 0; step < 14; step++) {
      const room = rooms[at];
      ctx.at("sensor");
      ctx.say("sensor", "state", `In ${room}: ${dirty[room] ? "dirty" : "clean"}`, "sensor");
      await ctx.pause(300);
      ctx.at("model");
      if (!dirty[room]) known[room] = "clean";
      ctx.say("model", "state", map(), "model");
      await ctx.pause(300);
      ctx.at("rules");
      await ctx.pause(200);
      ctx.at("act");
      if (dirty[room]) {
        dirty[room] = false;
        known[room] = "clean";
        ctx.say("act", "tool", `suck (${room})`, "act");
      } else if (rooms.every((r) => known[r] === "clean")) {
        ctx.say("act", "tool", "stop: the model says every room is clean", "act");
        ctx.at("model");
        return `All four rooms clean after ${step + 1} steps. It stopped because its internal model knew every room was clean, something no single reading could tell it.`;
      } else {
        at = rooms.findIndex((r) => known[r] !== "clean");
        ctx.say("act", "tool", `move to ${rooms[at]} (the nearest room not known to be clean)`, "act");
      }
      await ctx.pause(250);
    }
    return "Stopped at the step cap.";
  },
};

// --------------------------------------------------------------- goal-based

const MAP: Record<string, string[]> = {
  Depot: ["Market", "Mill"],
  Mill: ["Depot", "Bridge"],
  Market: ["Depot", "Park", "Bridge"],
  Bridge: ["Mill", "Market", "Clinic"],
  Park: ["Market", "Clinic"],
  Clinic: ["Bridge", "Park"],
};

function route(from: string, to: string, closed: Set<string>): string[] | null {
  const seen = new Set([from]);
  const queue: string[][] = [[from]];
  while (queue.length) {
    const path = queue.shift()!;
    const last = path[path.length - 1];
    if (last === to) return path;
    for (const next of MAP[last]) {
      const road = [last, next].sort().join("–");
      if (!seen.has(next) && !closed.has(road)) {
        seen.add(next);
        queue.push([...path, next]);
      }
    }
  }
  return null;
}

const goalBased: PatternDef = {
  id: "goal-based",
  title: "Goal-based agent",
  group: "IBM agent types",
  short: "Searches for actions that reach a goal, and replans.",
  what: [
    "A goal-based agent knows what it wants to reach and looks ahead: it searches for a sequence of actions that gets there, then follows it. When the world changes it plans again instead of following a stale rule.",
    "This delivery drone finds the shortest route to the clinic; a road closes on the way and it replans. With a language model, the planner step is often the model itself.",
  ],
  use: ["A clear goal with many ways to reach it", "The world changes and plans must adapt", "Navigation, scheduling, task planning"],
  avoid: ["Several goals that trade off (utility-based)", "Search spaces too big to plan in time"],
  task: "Deliver the package from the Depot to the Clinic.",
  usesModel: false,
  criogen: { kind: "primitives", api: "Workflow loop: plan → act → check (goto)" },
  graph: {
    w: 640,
    h: 220,
    nodes: [
      { id: "goal", label: "Goal: Clinic", x: 240, y: 40, kind: "io" },
      { id: "sense", label: "Sense", x: 70, y: 140, kind: "io" },
      { id: "plan", label: "Plan (search)", x: 240, y: 140, kind: "logic" },
      { id: "act", label: "Fly", x: 420, y: 140, kind: "tool" },
      { id: "arrive", label: "Arrived", x: 570, y: 140, kind: "io" },
    ],
    edges: [
      ["goal", "plan"],
      ["sense", "plan"],
      ["plan", "act", "next hop"],
      ["act", "sense", "new position"],
      ["act", "arrive", "goal reached"],
    ],
  },
  async run(ctx) {
    const closed = new Set<string>();
    let here = "Depot";
    let replans = 0;
    for (let step = 0; step < 10; step++) {
      ctx.at("sense");
      if (here === "Market" && !closed.size) {
        closed.add("Bridge–Market");
        ctx.say("sense", "error", "Road Market–Bridge just closed.", "sense");
      } else ctx.say("sense", "state", `At ${here}`, "sense");
      await ctx.pause(300);
      ctx.at("plan");
      const path = route(here, "Clinic", closed);
      if (!path) return "No route left to the Clinic.";
      if (step === 0 || closed.size > replans) {
        if (step > 0) replans++;
        ctx.say("planner", "note", `${step === 0 ? "Plan" : "Replan"}: ${path.join(" → ")} (${path.length - 1} hops)`, "plan");
      }
      await ctx.pause(300);
      here = path[1];
      ctx.at("act");
      ctx.say("drone", "tool", `fly to ${here}`, "act");
      await ctx.pause(350);
      if (here === "Clinic") {
        ctx.at("arrive");
        return `Delivered after ${step + 1} hops and ${replans} replan. The agent acted on its goal, not on a fixed rule.`;
      }
    }
    return "Stopped at the step cap.";
  },
};

// ------------------------------------------------------------ utility-based

const FLIGHTS = [
  { name: "Budget, 1 stop", price: 180, hours: 9.5, stops: 1 },
  { name: "Direct, early", price: 340, hours: 5, stops: 0 },
  { name: "Night, 2 stops", price: 140, hours: 14, stops: 2 },
  { name: "Direct, flexible", price: 460, hours: 5, stops: 0 },
];

const utilityBased: PatternDef = {
  id: "utility-based",
  title: "Utility-based agent",
  group: "IBM agent types",
  short: "Scores every option by how good it is, picks the best.",
  what: [
    "A utility-based agent does not just reach a goal, it asks how well each option serves it. A utility function turns price, time and comfort into one score, and the agent picks the highest.",
    "Move the sliders to change what you care about and run it again: the same flights, a different choice.",
  ],
  use: ["Trade-offs between several criteria", "Many options that all reach the goal", "Recommendations, pricing, scheduling"],
  avoid: ["Preferences you cannot express as weights", "Only one acceptable option"],
  task: "Book a flight from Lisbon to Berlin.",
  usesModel: false,
  criogen: { kind: "primitives", api: "A tool or node that scores options" },
  params: [
    { id: "price", label: "Price matters", min: 0, max: 10, value: 6 },
    { id: "time", label: "Time matters", min: 0, max: 10, value: 4 },
    { id: "comfort", label: "Fewer stops matter", min: 0, max: 10, value: 5 },
  ],
  graph: {
    w: 640,
    h: 200,
    nodes: [
      { id: "options", label: "Options", x: 70, y: 100, kind: "io" },
      { id: "utility", label: "Utility", x: 250, y: 100, kind: "logic" },
      { id: "choose", label: "Choose max", x: 420, y: 100, kind: "logic" },
      { id: "book", label: "Book", x: 570, y: 100, kind: "tool" },
    ],
    edges: [
      ["options", "utility"],
      ["utility", "choose", "scores"],
      ["choose", "book"],
    ],
  },
  async run(ctx) {
    const w = { price: ctx.params.price ?? 6, time: ctx.params.time ?? 4, comfort: ctx.params.comfort ?? 5 };
    const total = w.price + w.time + w.comfort || 1;
    ctx.say("utility", "note", `U = ${(w.price / total).toFixed(2)}·cheap + ${(w.time / total).toFixed(2)}·fast + ${(w.comfort / total).toFixed(2)}·few stops (each 0–1)`);
    const minP = Math.min(...FLIGHTS.map((f) => f.price));
    const maxP = Math.max(...FLIGHTS.map((f) => f.price));
    const minH = Math.min(...FLIGHTS.map((f) => f.hours));
    const maxH = Math.max(...FLIGHTS.map((f) => f.hours));
    const scored = [];
    for (const f of FLIGHTS) {
      ctx.at("options");
      await ctx.pause(200);
      const cheap = 1 - (f.price - minP) / (maxP - minP);
      const fast = 1 - (f.hours - minH) / (maxH - minH);
      const few = 1 - f.stops / 2;
      const u = (w.price * cheap + w.time * fast + w.comfort * few) / total;
      scored.push({ f, u });
      ctx.at("utility");
      ctx.say("utility", "state", `${f.name}: $${f.price}, ${f.hours} h, ${f.stops} stop${f.stops === 1 ? "" : "s"} → U = ${u.toFixed(2)}`, "utility");
      await ctx.pause(300);
    }
    ctx.at("choose");
    await ctx.pause(300);
    const best = scored.reduce((a, b) => (b.u > a.u ? b : a));
    ctx.at("book");
    ctx.say("booking", "tool", `book “${best.f.name}”`, "book");
    return `Booked “${best.f.name}” ($${best.f.price}, ${best.f.hours} h): the highest utility, ${best.u.toFixed(2)}, for these weights.`;
  },
};

// ------------------------------------------------------------------ learning

const STYLES = [
  { name: "short", p: 0.45 },
  { name: "detailed", p: 0.6 },
  { name: "friendly + steps", p: 0.8 },
];

const learning: PatternDef = {
  id: "learning",
  title: "Learning agent",
  group: "IBM agent types",
  short: "A critic scores its actions; it gets better with experience.",
  what: [
    "A learning agent improves from feedback. The performance element acts; a critic judges the outcome; the learning element updates what the agent believes works; a problem generator makes it try something new now and then, so it does not settle too early.",
    "Here a support bot learns which reply style customers rate best over 40 conversations, without being told. Raise exploration to try other styles more often.",
  ],
  use: ["Feedback arrives over time (ratings, outcomes, tests)", "The best action is not known in advance", "Conditions drift and the agent should keep up"],
  avoid: ["Mistakes are expensive while it learns", "There is no reliable feedback signal"],
  task: "Learn which reply style gets the best customer ratings.",
  usesModel: false,
  criogen: { kind: "primitives", api: "Memory (stores what was learned) + a hook that scores runs" },
  params: [{ id: "explore", label: "Exploration %", min: 0, max: 60, step: 5, value: 20 }],
  graph: {
    w: 640,
    h: 240,
    nodes: [
      { id: "perf", label: "Act (choose style)", x: 150, y: 60, kind: "logic" },
      { id: "env", label: "Customer", x: 420, y: 60, kind: "io" },
      { id: "critic", label: "Critic (rating)", x: 420, y: 185, kind: "logic" },
      { id: "learn", label: "Learn (update)", x: 150, y: 185, kind: "memory" },
      { id: "explore", label: "Try new", x: 560, y: 120, kind: "logic" },
    ],
    edges: [
      ["perf", "env", "reply"],
      ["env", "critic", "outcome"],
      ["critic", "learn", "reward"],
      ["learn", "perf", "better estimates"],
      ["explore", "perf"],
    ],
  },
  async run(ctx) {
    const eps = (ctx.params.explore ?? 20) / 100;
    const rand = rng(7);
    const value = STYLES.map(() => 0);
    const tries = STYLES.map(() => 0);
    let good = 0;
    const show = () => STYLES.map((s, i) => `${s.name} ${tries[i] ? value[i].toFixed(2) : "–"} (${tries[i]})`).join(" · ");
    for (let ep = 1; ep <= 40; ep++) {
      const exploring = rand() < eps || tries.every((t) => t === 0);
      const i = exploring ? Math.floor(rand() * STYLES.length) : value.indexOf(Math.max(...value));
      if (exploring) ctx.at("explore");
      ctx.at("perf");
      ctx.at("env");
      const liked = rand() < STYLES[i].p;
      good += liked ? 1 : 0;
      ctx.at("critic");
      tries[i]++;
      value[i] += ((liked ? 1 : 0) - value[i]) / tries[i];
      ctx.at("learn");
      if (ep <= 6 || ep % 8 === 0) {
        ctx.say(`conversation ${ep}`, "state", `${exploring ? "explore" : "exploit"}: ${STYLES[i].name} → ${liked ? "👍" : "👎"}   |   ${show()}`, "learn");
        await ctx.pause(ep <= 6 ? 420 : 300);
      } else await ctx.pause(60);
    }
    const best = STYLES[value.indexOf(Math.max(...value))].name;
    return `After 40 conversations it prefers “${best}” (estimated rating ${Math.max(...value).toFixed(2)}); ${good}/40 customers were happy. Nobody told it the answer: the critic's feedback did.`;
  },
};

export const IBM: PatternDef[] = [simpleReflex, modelReflex, goalBased, utilityBased, learning];
