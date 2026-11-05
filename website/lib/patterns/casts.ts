// How each pattern runs on the engine (design 0021): its code sample
// (snippets/typescript/patterns.ts), the diagram nodes its agents, tools and
// workflow nodes light up, and what the scripted model says for each agent
// when the visitor has no key. The scripts follow the samples' real control
// flow: typed outputs answer through `final_output`, coordinators call their
// specialists as tools, the swarm hands off with "HANDOFF: <name>".
import type { ModelCall, EngineSpec, Reply } from "./live";

/** The text of the request's user messages. */
const asked = (call: ModelCall) =>
  call.messages
    .filter((m) => m.role === "user")
    .flatMap((m) => m.content ?? [])
    .map((b) => b.text ?? "")
    .join("\n");

/** The value of the request's latest tool result. */
const lastResult = (call: ModelCall): unknown => {
  const tools = call.messages.filter((m) => m.role === "tool");
  const block = tools[tools.length - 1]?.content?.[0];
  return block?.value ?? block?.text;
};

const team = (text: string): "billing" | "technical" | "sales" =>
  /charge|refund|invoice|bill|paid|price|subscription/i.test(text) ? "billing" : /error|crash|bug|log ?in|install|slow|broken/i.test(text) ? "technical" : "sales";

export const CASTS: Record<string, EngineSpec> = {
  // ------------------------------------------------------------ reasoning
  react: {
    sample: "react",
    input: "user",
    answer: "answer",
    tools: { weather: "weather", calendar: "calendar" },
    actors: [
      {
        match: "Work in a loop: think about what you need next",
        node: "think",
        who: "agent",
        replies: [
          { say: "I need the forecast for Lisbon first.", call: [["weather", { city: "Lisbon" }]] },
          { say: "Sunny and warm. Now I check when you are free.", call: [["calendar", { day: "Saturday" }]] },
          "Yes. Saturday after 11:00 is ideal: 24 °C, sunny, 5% chance of rain. Bring sunscreen.",
        ],
      },
    ],
  },

  "chain-of-thought": {
    sample: "chainOfThought",
    input: "q",
    answer: "a",
    actors: [
      {
        match: "Think step by step. Write numbered steps",
        node: "reason",
        who: "agent",
        replies: [
          "1. Let c be coffees and m muffins: c + m = 4.\n2. The bill: 3c + 2m = 10.\n3. From step 1, m = 4 − c, so 3c + 8 − 2c = 10, and c = 2.\n4. Check: 2 coffees ($6) and 2 muffins ($4) make $10.\nANSWER: 2 coffees",
        ],
      },
    ],
  },

  "tree-of-thoughts": {
    sample: "treeOfThoughts",
    input: "p",
    answer: "grow",
    nodes: { begin: "propose", branch1: "t1", branch2: "t2", branch3: "t3", grow: "grow" },
    actors: [
      { match: "Approach 1:", node: "t1", who: "proposer", replies: ["“Stack Overflow Café”: every table has a rubber duck for debugging out loud."] },
      { match: "Approach 2:", node: "t2", who: "proposer", replies: ["“Commit & Brew”: drinks named after git commands, and a wall of merged pull requests."] },
      { match: "Approach 3:", node: "t3", who: "proposer", replies: ["“The Null Pointer”: the menu is empty until you say exactly what you want."] },
      {
        match: "Rate how promising the idea is",
        node: "judge",
        who: "judge",
        replies: (call) => {
          const idea = asked(call);
          return { output: { score: idea.includes("Commit & Brew") ? 9 : idea.includes("Stack Overflow") ? 7 : 5 } };
        },
      },
      {
        match: "Develop the idea into the final answer",
        node: "grow",
        who: "grower",
        replies: [
          "Commit & Brew: a café where the espresso is “git push”, the decaf is “git stash”, and a wall of framed, merged pull requests shows who shipped what this week. Free refills for anyone who fixes the Wi-Fi.",
        ],
      },
    ],
  },

  "plan-and-solve": {
    sample: "planAndSolve",
    input: "goal",
    answer: "done",
    nodes: { plan: "plan", execute: "exec", write: "solve" },
    actors: [
      {
        match: "Write a plan of three short steps",
        node: "plan",
        who: "planner",
        replies: [{ output: { steps: ["Check how well the house is insulated", "Compare running costs with the current boiler", "Look at the installation cost and grants"] } }],
      },
      {
        match: "Carry out one step of a plan",
        node: "exec",
        who: "executor",
        replies: (call) => {
          const now = asked(call).split("Now:").pop() ?? "";
          if (/insulat/i.test(now)) return "A 1970s house usually has thin cavity walls and loft insulation; it needs topping up first, or the heat pump must run hot and costs more.";
          if (/running costs/i.test(now)) return "With insulation done, a heat pump uses about a third of the energy of a gas boiler for the same heat; bills are similar to 20% lower.";
          return "Installation is about €10,000–14,000; many countries pay €3,000–7,500 of it, so it pays back in 8–12 years.";
        },
      },
      {
        match: "Write the final brief from the step results",
        node: "solve",
        who: "writer",
        replies: ["Yes, if you insulate first. Top up the loft and walls, then a heat pump heats the house for about the same or lower bills than gas, and grants cover a third to half of the €10–14k install. Without insulation, wait."],
      },
    ],
  },

  rewoo: {
    sample: "rewoo",
    input: "task",
    answer: "answer",
    nodes: { plan: "planner", work: "search", solve: "solver" },
    narrate: (node, phase, value, ctx) => {
      if (node === "work" && phase === "end") ctx.say("tools", "tool", `evidence: ${JSON.stringify(value.evidence)}`, "search");
    },
    actors: [
      {
        match: "Plan every tool call up front",
        node: "planner",
        who: "planner",
        replies: [
          {
            output: {
              calls: [
                { id: "E1", tool: "search", arg: "capital of France" },
                { id: "E2", tool: "search", arg: "capital of Japan" },
                { id: "E3", tool: "search", arg: "population of #E1" },
                { id: "E4", tool: "search", arg: "population of #E2" },
              ],
            },
          },
        ],
      },
      {
        match: "Answer from the plan and the evidence only",
        node: "solver",
        who: "solver",
        replies: ["Japan's: Tokyo has about 14 million people, Paris about 2.1 million, so the Japanese capital is more than six times bigger."],
      },
    ],
  },

  reflexion: {
    sample: "reflexion",
    input: "task",
    answer: "done",
    actors: [
      {
        match: "Write one launch tweet",
        node: "actor",
        who: "writer",
        replies: (call) => {
          const lessons = (asked(call).split("Lessons so far:")[1] ?? "").split(";").filter((l) => l.trim()).length;
          if (lessons === 0) return "Meet SoundVeil: silence the world and hear what matters. Noise-cancelling headphones, out now! #SoundVeil #Launch";
          if (lessons === 1) return "SoundVeil is here: noise-cancelling headphones for $49. Silence the world, hear what matters. #quiet";
          return "SoundVeil is here: noise-cancelling headphones for $49. Silence the noise, keep the music.";
        },
      },
      {
        match: "Say in one line what to do differently",
        node: "reflect",
        who: "reflector",
        replies: (call) => (/no price/.test(asked(call)) ? "Say the price, $49, in the tweet." : "Drop the hashtags; let the product speak."),
      },
    ],
  },

  // ---------------------------------------------------------- multi-agent
  router: {
    sample: "router",
    input: "req",
    answer: "out",
    nodes: { route: "route" },
    actors: [
      { match: "Classify the request: billing, technical or sales", node: "route", who: "router", replies: (call) => ({ output: { team: team(asked(call)) } }) },
      { match: "You are the billing support specialist", node: "billing", who: "billing", replies: ["Sorry about the double charge. I've refunded the second payment of this month; it shows on your card in 3–5 days."] },
      { match: "You are the technical support specialist", node: "technical", who: "technical", replies: ["Let's fix that: sign out on every device, update the app, then sign in again. If it still fails, send me the error text."] },
      { match: "You are the sales support specialist", node: "sales", who: "sales", replies: ["Happy to help you choose: the Team plan fits up to 10 people, and the first month is free."] },
    ],
  },

  supervisor: {
    sample: "supervisor",
    input: "req",
    answer: "out",
    actors: [
      {
        match: "Specialists you can call",
        node: "sup",
        who: "supervisor",
        replies: [
          { say: "First the facts.", call: [["researcher", { input: "Why do teams adopt TypeScript? Facts only." }]] },
          { say: "Now the text.", call: [["writer", { input: "Three short bullet points from these notes." }]] },
          { say: "A last check.", call: [["reviewer", { input: "Check the three bullet points for errors." }]] },
          "• Types catch mistakes before the code runs.\n• Editors autocomplete and refactor safely across a big codebase.\n• Types document the code, so new people get up to speed faster.",
        ],
      },
      { match: "Find facts. Be brief and precise", node: "researcher", who: "researcher", replies: ["Static types catch bugs at compile time; editor support (autocomplete, safe renames); types act as documentation; gradual adoption from JavaScript."] },
      { match: "Write clear, short text from notes", node: "writer", who: "writer", replies: ["• Types catch mistakes before the code runs.\n• Editors autocomplete and refactor safely.\n• Types document the code for new people."] },
      { match: "Check the text for errors", node: "reviewer", who: "reviewer", replies: ["Correct and clear. One suggestion: say “across a big codebase” in the second point."] },
    ],
  },

  hierarchical: {
    sample: "hierarchical",
    input: "goal",
    answer: "out",
    actors: [
      {
        match: "You manage a research team and a finance team",
        node: "mgr",
        who: "manager",
        replies: [
          { say: "I need both teams' view; they can work at the same time.", call: [["research", { input: "Footfall and competitors downtown for a bakery." }], ["finance", { input: "Rent, staff and break-even for a second shop downtown." }]] },
          "Open it, on two conditions: sign a lease under €4,000 a month, and start with the morning rush only (7–11) until sales pass €900 a day. Footfall is strong and the break-even is reachable in about 14 months.",
        ],
      },
      { match: "You lead the research team", node: "rlead", who: "research lead", replies: [{ call: [["footfall", { input: "Estimate downtown footfall and competitors." }]] }, "Research: about 6,000 people pass the site each weekday morning; two coffee chains nearby, no bakery."] },
      { match: "You lead the finance team", node: "flead", who: "finance lead", replies: [{ call: [["costs", { input: "Estimate rent, staff and break-even." }]] }, "Finance: €3,800 rent and €9,000 staff a month; break-even at about €850 of sales a day, in about 14 months."] },
      { match: "Estimate footfall and nearby competitors", node: "rworker", who: "footfall analyst", replies: ["About 6,000 people a weekday morning, mostly office workers; two coffee chains, no bakery within 400 m."] },
      { match: "Estimate rent, staff and break-even", node: "fworker", who: "cost analyst", replies: ["Rent about €3,800/month, three staff €9,000/month; break-even near €850/day."] },
    ],
  },

  collaboration: {
    sample: "collaboration",
    input: "in",
    answer: "out",
    actors: [
      { match: "List the key facts as three bullet points", node: "researcher", who: "researcher", replies: ["• Sunlight holds all colours.\n• Air scatters blue light much more than red (Rayleigh scattering).\n• So blue light reaches our eyes from every part of the sky."] },
      { match: "Write the answer for the audience", node: "writer", who: "writer", replies: ["Sunlight is a mix of every colour. When it hits the air, the tiny bits of air bounce blue light around much more than red light, so blue comes at us from all over the sky, and the sky looks blue."] },
      { match: "Make it shorter and clearer", node: "editor", who: "editor", replies: ["Sunlight has every colour in it. Air bounces the blue part around the most, so blue reaches your eyes from everywhere in the sky."] },
    ],
  },

  debate: {
    sample: "debate",
    input: "q",
    answer: "out",
    nodes: { for: "pro", against: "con", judge: "judge" },
    actors: [
      {
        match: "Argue FOR.",
        node: "pro",
        who: "for",
        replies: (call) =>
          /^for:/m.test(asked(call))
            ? "Costs grow with every provider you add; a gateway pays for itself once you have three. You also control logs, which matters for compliance."
            : "Yes: one gateway gives every product the same retries, keys and budgets, and you can switch models without touching the apps.",
      },
      {
        match: "Argue AGAINST.",
        node: "con",
        who: "against",
        replies: (call) =>
          /^against:/m.test(asked(call))
            ? "Compliance is real, but a managed gateway gives you logs too. Five people should ship the product, not run infrastructure."
            : "No: five people should not maintain infrastructure. Use an existing gateway; build only when its limits really hurt.",
      },
      { match: "Judge the debate", node: "judge", who: "judge", replies: ["Against wins. The deciding reason: with five people, every week spent running a gateway is a week not spent on the product, and managed gateways already cover retries, keys and logs."] },
    ],
  },

  parallel: {
    sample: "parallel",
    input: "in",
    answer: "out",
    actors: [
      { match: "Overall sentiment and one reason", node: "sentiment", who: "sentiment", replies: ["Mixed: happy with the battery, frustrated by being logged out."] },
      { match: "Topics mentioned", node: "topics", who: "topics", replies: ["battery life (+), app login (−)"] },
      { match: "Draft a two-sentence reply", node: "reply", who: "reply", replies: ["Thanks, we're glad the battery lasts! We've fixed the log-out bug in today's update; please update the app."] },
      { match: "Combine the analyses into a short report", node: "synth", who: "synthesizer", replies: ["Sentiment: mixed. Battery praised; the app's log-out bug is the one complaint. Reply drafted, pointing to today's fix."] },
    ],
  },

  swarm: {
    sample: "swarm",
    input: "user",
    answer: "done",
    tools: { lookup_order: "lookup" },
    actors: [
      { match: "Work out what the customer needs", node: "triage", who: "triage", replies: ["The customer sent headphones back and is still waiting for the money. The order team should confirm the return.\nHANDOFF: orders"] },
      {
        match: "Look up orders and returns",
        node: "orders",
        who: "orders",
        replies: [
          { say: "Let me find the order.", call: [["lookup_order", { customer: "the customer" }]] },
          "Order A-1001: the return arrived on 24 Oct and no refund was issued yet.\nHANDOFF: refunds",
        ],
      },
      { match: "Issue refunds for returned items", node: "refunds", who: "refunds", replies: ["Refund issued for order A-1001 (return received 24 Oct). It reaches your card in 3–5 business days. Sorry for the wait!\nFINAL"] },
    ],
  },

  "human-in-the-loop": {
    sample: "humanInTheLoop",
    input: "req",
    review: { ask: "person", approved: "send", rejected: "stop" },
    actors: [
      {
        match: "Draft a short refund email",
        node: "draft",
        who: "drafter",
        replies: ["Dear customer, we're sorry your order A-2077 arrived damaged. We have refunded $180 to your card; it arrives in 3–5 business days. Best regards, the support team"],
      },
    ],
  },

  // ------------------------------------------------------ IBM agent types
  "simple-reflex": {
    sample: "simpleReflex",
    settings: (p) => ({ target: p.target ?? 21 }),
    nodes: { sense: "sensor", heat: "heat", cool: "cool", idle: "idle" },
    narrate: (node, phase, value, ctx) => {
      if (node === "sense" && phase === "start") ctx.say("sensor", "state", `${value.temperature} °C`, "sensor");
      if (["heat", "cool", "idle"].includes(node) && phase === "start") ctx.say("rules", "state", `→ ${node}`, node);
    },
  },

  "model-based-reflex": {
    sample: "modelBasedReflex",
    nodes: { step: "model", done: "act" },
    narrate: (node, phase, value, ctx) => {
      if (node !== "step" || phase !== "start") return;
      const dirty = value.dirty as Record<string, boolean>;
      const here = String(value.at);
      ctx.say("world model", "state", `in room ${here} (${dirty[here] ? "dirty: clean it" : "clean"}); known clean: ${(value.known as string[]).join(", ") || "none"}`, "model");
    },
  },

  "goal-based": {
    sample: "goalBased",
    nodes: { fly: "plan", arrived: "arrive" },
    narrate: (node, phase, value, ctx) => {
      if (node === "fly" && phase === "start") ctx.say("drone", "state", `at ${value.at}: plan the shortest open route to ${value.goal}`, "plan");
      if (node === "fly" && phase === "end" && value.at) ctx.say("drone", "tool", `flies to ${(value as { at: string }).at}`, "act");
    },
  },

  "utility-based": {
    sample: "utilityBased",
    input: "options",
    answer: "book",
    settings: (p) => ({ price: p.price ?? 6, time: p.time ?? 4, comfort: p.comfort ?? 5 }),
    tools: { best_flight: "utility" },
    actors: [
      {
        match: "Book the best flight. Use best_flight",
        node: "choose",
        who: "agent",
        replies: (call, turn): Reply => {
          if (turn === 0) return { say: "I'll score the three flights with this traveller's weights.", call: [["best_flight", {}]] };
          const f = (lastResult(call) ?? {}) as { name?: string; price?: number; hours?: number; stops?: number };
          return `Booked “${f.name}”: €${f.price}, ${f.hours} h, ${f.stops} stop${f.stops === 1 ? "" : "s"}. For these weights it has the highest utility; the others lose on what you care about most.`;
        },
      },
    ],
  },

  learning: {
    sample: "learning",
    settings: (p) => ({ explore: p.explore ?? 20 }),
    storage: (() => {
      let n = 0;
      return async (op, key, value, ctx) => {
        if (op !== "remember") return;
        n += 1;
        const s = value as { n: number; mean: number };
        ctx.at("perf");
        ctx.at("env");
        ctx.at("critic");
        ctx.at("learn");
        if (n <= 6 || n % 8 === 0) {
          ctx.say(`conversation ${n}`, "state", `“${key}” rated: its mean is now ${s.mean.toFixed(2)} over ${s.n}`, "learn");
          await ctx.pause(n <= 6 ? 420 : 300);
        } else await ctx.pause(60);
        if (n >= 40) n = 0;
      };
    })(),
  },
};
