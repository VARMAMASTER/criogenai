import { Agent } from "agentbuild-sdk";

declare const agent: Agent;

// [show]
const chat = agent.thread("user-42");
await chat.send("Hi, I am Ada");
const reply = await chat.send("What is my name?"); // remembers the conversation

const history = await chat.history(20); // checkpoints, newest first
await chat.rewind(String(history[1].id)); // go back one step

// After a crash mid-run: continue where it stopped.
// Finished tool calls are not run again.
await chat.retry();

// Every call takes an AbortSignal: cancelling stops the run, and it is still saved.
await chat.send("Long question", { signal: AbortSignal.timeout(30_000) });

await chat.delete();
// [/show]
export {};
