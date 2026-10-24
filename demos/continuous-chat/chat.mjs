// The chat itself: one agent, and one saved conversation (a "thread") per
// person. The conversation lives in a SQLite file, so closing the program and
// opening it again continues where it stopped: the model still knows
// everything said before, and `thread.messages()` gives the messages back.

import { Agent, FakeModel, Storage } from "@varmamaster/sdk";

/** Which model: `CHAT_MODEL=gemini:gemini-2.5-flash`, `openai:gpt-5-mini`, … or `fake` (offline). */
export function pickModel(name = process.env.CHAT_MODEL) {
  if (name === "fake") {
    // Offline: so the program can be tried without a key.
    return new FakeModel(["I am the offline model. I keep the conversation, but I always answer the same."]);
  }
  if (name) return name;
  if (process.env.GEMINI_API_KEY) return "gemini:gemini-2.5-flash";
  if (process.env.OPENAI_API_KEY) return "openai:gpt-5-mini";
  return null;
}

/** A safe thread name: letters, digits, dash and underscore. */
export function cleanName(name) {
  const clean = String(name ?? "").trim();
  if (clean.length > 40 || !/^[A-Za-z0-9_-]+$/.test(clean)) {
    throw new Error("a name is letters, digits, - and _ (up to 40)");
  }
  return clean;
}

/** The text of a message as the SDK stores it (a list of blocks). */
function textOf(message) {
  const content = message.content;
  if (typeof content === "string") return content;
  return (content ?? []).map((block) => block.text ?? "").join("");
}

export function createChat({ file = "chat.db", model = pickModel() } = {}) {
  if (!model) {
    throw new Error("no model: set CHAT_MODEL (or GEMINI_API_KEY / OPENAI_API_KEY in .env), or CHAT_MODEL=fake");
  }
  const storage = new Storage({ url: `sqlite://${file}` });
  const agent = new Agent({
    model,
    storage,
    instructions:
      "You are a friendly assistant in a long-running chat. You remember everything said earlier in this " +
      "conversation. Keep answers short unless asked for detail.",
  });
  /** One person's conversation. The same name always gets the same conversation back. */
  const thread = (name) => agent.thread(`chat:${cleanName(name)}`);
  return {
    thread,
    /** Sends `text`, calls `onText` with each piece of the answer, returns the whole answer. */
    async say(name, text, onText = () => {}) {
      const stream = thread(name).stream(text);
      for await (const event of stream) {
        if (event.type === "text") onText(event.text);
      }
      return (await stream.reply()).text ?? "";
    },
    /** The last `limit` exchanges, oldest first: `[{ you, bot }]`, read from the saved conversation. */
    async recent(name, limit = 8) {
      const turns = [];
      for (const message of await thread(name).messages()) {
        if (message.role === "user") turns.push({ you: textOf(message), bot: "" });
        else if (message.role === "assistant" && turns.length > 0) turns[turns.length - 1].bot += textOf(message);
      }
      return turns.slice(-limit);
    },
    /** Forgets the conversation. */
    forget: (name) => thread(name).delete(),
    /** How many steps the conversation has saved (0: a new one). */
    steps: async (name) => (await thread(name).history(1))[0]?.version ?? 0,
    close: () => storage.close(),
  };
}
