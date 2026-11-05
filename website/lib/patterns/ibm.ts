// IBM's five classic agent types (https://www.ibm.com/think/topics/ai-agent-types):
// simple reflex, model-based reflex, goal-based, utility-based, learning.
// They describe how an agent decides. Four need no model; the utility-based
// agent asks one to explain its choice. Each runs its code sample on the
// engine (casts.ts).
import type { PatternDef } from "./engine";

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
};

// --------------------------------------------------------------- goal-based

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
};

// ------------------------------------------------------------ utility-based

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
  usesModel: true,
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
};

// ------------------------------------------------------------------ learning

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
};

export const IBM: PatternDef[] = [simpleReflex, modelReflex, goalBased, utilityBased, learning];
