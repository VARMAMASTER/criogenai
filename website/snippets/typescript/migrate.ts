// Docs: from-langgraph. Type-checked against the SDK.
import { tool } from "criogenai";

const getWeather = tool<{ city: string }>({
  name: "get_weather",
  description: "The weather in a city.",
  input: { type: "object", properties: { city: { type: "string" } }, required: ["city"] },
  run: ({ city }) => `Sunny in ${city}`,
});

// [show graph]
import { END, Workflow } from "criogenai";

const app = new Workflow({ reducers: { log: "append" } }) // like a reducer on a state channel
  .node("plan", () => ({ steps: ["search", "write"], log: ["planned"] }))
  .node("write", (state) => ({ answer: `Done in ${(state.steps as string[]).length} steps`, log: ["written"] }))
  .start("plan") // START -> plan
  .route("plan", (state) => ((state.steps as string[]).length ? "write" : "stop"), { write: "write", stop: END })
  .edge("write", END)
  .compile(); // validates the graph; no state schema needed

const result = await app.run({ question: "..." });
console.log(result.state.answer, result.state.log);
// [/show graph]

// [show agent]
import { Agent, Storage } from "criogenai";

const agent = new Agent({
  model: "openai:gpt-5-mini",
  tools: [getWeather],
  instructions: "Answer briefly.",
  storage: new Storage("sqlite://chat.db"), // the checkpointer
});
// thread_id in the config becomes a thread handle
const reply = await agent.thread("user-1").send("Weather in Paris?");
console.log(reply.text);
// [/show agent]

// [show interrupt]
const careful = new Agent({
  model: "openai:gpt-5-mini",
  tools: [getWeather],
  approve: ["get_weather"], // pause before this tool runs: no interrupt() call to write
  storage: new Storage("sqlite://chat.db"),
});
const chat = careful.thread("user-2");
let answer = await chat.send("Weather in Paris?");
if (answer.needsApproval) {
  // like an __interrupt__ in the result
  answer = await chat.approve(); // like resuming with a Command
}
// [/show interrupt]

// [show store]
import { Memory } from "criogenai";

const storage = new Storage("sqlite://chat.db");
await storage.remember("users/1", "language", "French"); // namespace as a path, not an array
const facts = await storage.search("users/1", "language");

// Or let the agent recall and save facts itself, per user:
const remembering = new Agent({ model: "openai:gpt-5-mini", storage, memory: Memory.byState("user_id", "users") });
await remembering.thread("user-3").send("Bonjour", { state: { user_id: "1" } });
// [/show store]
export {};
