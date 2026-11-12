// [show]
import { Agent, Embedder, Memory, Storage } from "criogenai";

const storage = new Storage("sqlite://agent.db");
const agent = new Agent({
  model: "openai:gpt-5-mini",
  storage,
  // One memory per user: users/<user_id>. Facts are recalled before each
  // model call; the model gets remember, forget and search tools.
  memory: Memory.byState("user_id", "users"),
});

await agent.thread("monday").send("I'm vegetarian", { state: { user_id: "42" } });
// A later conversation recalls it.
await agent.thread("friday").send("Dinner ideas?", { state: { user_id: "42" } });

// Your app can read and write memory too.
await storage.remember("users/42", "allergy", "peanuts");
const facts = await storage.search("users/42", "allergy");
await storage.forgetAll("users/42"); // everything about one user
// [/show]

// [show semantic]
const meaning = Memory.byState("patient", "patients").semantic(Embedder.openai("text-embedding-3-small"));
// Or keep everything on your own servers: Embedder.ollama("nomic-embed-text")
const clinician = new Agent({ model: "openai:gpt-5-mini", memory: meaning });
// [/show semantic]
