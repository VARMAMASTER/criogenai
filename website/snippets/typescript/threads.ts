import { Agent } from "criogen";

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

// [show recover]
import { CriogenError, ThreadSuspendedError } from "criogen";

const thread = agent.thread("user-42");
let answer;
try {
  answer = await thread.send("Book my follow-up visit");
} catch (error) {
  if (error instanceof ThreadSuspendedError) {
    answer = await thread.reject("Cancelled by the user"); // or approve() / decide(...)
  } else if (error instanceof CriogenError && error.code === "thread_interrupted") {
    answer = await thread.retry(); // a crash left the last run unfinished; finished tool calls are not run again
  } else {
    throw error;
  }
}

// The stored conversation (the same message format as result.messages).
for (const message of await thread.messages()) {
  const content = message.content as { text?: string }[];
  console.log(message.role, content[0]?.text ?? "");
}
// [/show recover]
