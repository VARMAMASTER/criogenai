// Docs: concepts/errors-and-retries. Type-checked against the SDK.
import { Agent } from "criogenai";

declare const agent: Agent;

// [show catch]
import {
  CancelledError,
  CriogenError,
  LimitExceededError,
  ModelError,
  ThreadBusyError,
  ThreadSuspendedError,
} from "criogenai";

const chat = agent.thread("ticket-7");
try {
  const reply = await chat.send("Where is my order?", { signal: AbortSignal.timeout(60_000) });
} catch (error) {
  if (error instanceof ThreadSuspendedError) {
    // an approval is waiting: answer it first (approve, reject or decide)
  } else if (error instanceof ThreadBusyError) {
    // another run holds the thread (onBusy "reject", or the wait timed out)
  } else if (error instanceof LimitExceededError) {
    console.log(error.details.limit); // "model_calls", "tool_calls", "tokens", ...
  } else if (error instanceof ModelError) {
    const kind = (error.details.model_error as { kind: string }).kind; // "rate_limited", "auth", ...
    if (["rate_limited", "overloaded", "server", "timeout", "network"].includes(kind)) {
      await chat.retry(); // transient: continue from the failed step
    }
  } else if (error instanceof CancelledError) {
    // the signal fired: the run is saved, and retry() continues it
  } else if (error instanceof CriogenError && error.code === "thread_interrupted") {
    await chat.retry(); // the process died mid-run: continue it
  } else {
    throw error; // decide by the stable code, never by the message
  }
}
// [/show catch]

// [show retry]
const resilient = new Agent({
  model: "openai:gpt-5-mini",
  // Transient model errors (rate limits, overload, 5xx, timeouts, network)
  // are retried with backoff; Retry-After from the provider is honoured.
  modelRetry: { maxAttempts: 4, initialBackoffSecs: 1, maxBackoffSecs: 30 },
  maxRetryAfterSecs: 60, // never wait longer than this for one Retry-After
  // Then the next model is tried, in order.
  fallbacks: ["anthropic:claude-haiku-5-5", "ollama:llama3.2"],
  // Also move on for errors that another model may not have.
  fallbackOn: ["context_overflow", "content_filter"],
});
// [/show retry]
export {};
