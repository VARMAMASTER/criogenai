# 0018: Calling a model directly from the SDKs (`Gateway`)

Status: **Accepted** (roadmap task 16; the user asked to build every
feature planned for Python and TypeScript). It makes
[0007](0007-public-api.md) "Level 0" concrete. The name `Gateway` comes
from 0007; it runs in your process and has nothing to do with the
`agentbuild-gateway` server (0006 §12), which you reach like any
OpenAI-compatible endpoint.

## 1. What it is for

One model call without an agent: summarize a text, classify a ticket,
draft a reply, embed documents. The same retries and fallback models as an
agent (0003 §5, 0017 §2), the same model names and objects, the same error
classes.

## 2. API

```python
from agentbuild import Gateway, Retry

gw = Gateway(fallbacks=["gemini:gemini-2.5-flash"], retry=Retry(max_attempts=4))
reply = await gw.chat("openai:gpt-5-mini", "Summarize: …", instructions="Be brief.")
reply.text, reply.message, reply.usage, reply.stop_reason, reply.model

stream = gw.stream("openai:gpt-5-mini", "Write a haiku")
async for chunk in stream:
    print(chunk.text, end="")
stream.reply            # after the loop: the whole reply

vectors = await gw.embed(Embedder.openai(), ["first text", "second text"])
reply = gw.chat_sync(…); vectors = gw.embed_sync(…)
```

```ts
const gw = new Gateway({ fallbacks: ["gemini:gemini-2.5-flash"], retry: { maxAttempts: 4 } });
const reply = await gw.chat("openai:gpt-5-mini", "Summarize: …", { instructions: "Be brief.", signal });
const stream = gw.stream("openai:gpt-5-mini", "Write a haiku");
for await (const chunk of stream) process.stdout.write(chunk.text);
const whole = await stream.reply();
const vectors = await gw.embed(Embedder.openai(), ["first text", "second text"]);
```

- `Gateway(...)` options (all optional): `fallbacks`, `fallback_on`,
  `retry`, `max_retry_after_secs`: the agent's settings with the agent's
  defaults. A local server or a gateway is a model object
  (`OpenAICompatible("llama3.2", base_url=…)`), which replaces 0007's
  `endpoints={…}` map.
- `chat(model, input, *, instructions, temperature, max_tokens, output)`:
  `input` is text or a list of messages (the transcript format of
  `result.messages`; `content` may be a plain string as a shorthand for
  one text block). `instructions` is the system message. `output` is a
  JSON Schema: the model answers with JSON (`reply.output`, parsed), using
  the provider's JSON mode where it has one.
- `reply`: `text` (all text blocks), `message` (the assistant message),
  `output` (with `output`), `usage`, `stop_reason`, and `model`
  (`"provider:model"` that answered, which differs from the one asked for
  after a fallback).
- `stream(...)`: the same arguments; text chunks as they arrive. A retry
  after part of a reply was streamed starts over: the stream yields a
  `restart` chunk (`chunk.restart` / `chunk.type === "restart"`) so a UI
  drops what it showed.
- `embed(embedder, texts)`: one vector per text, in order.
- Cancellation: Python task cancellation, `AbortSignal` in TypeScript;
  the HTTP request stops.
- Errors: `ModelError` (`model_failed`, with the provider error's
  `kind` in `details`) after retries and fallbacks; `ValidationError`
  for a bad request (unknown model, invalid messages).

## 3. Rust

The retry and fallback chain the agent uses becomes public as
`agentbuild_agent::ResilientModel`, a `Model` itself:

```rust
let model = ResilientModel::new(primary)
    .fallback(backup)
    .retry(RetryPolicy::new().max_attempts(4))
    .fallback_on(ModelErrorKind::ContextOverflow)
    .max_retry_after(Duration::from_secs(60));
let response = model.invoke(&request).await?;
```

It needs no run: no events, no deadline. Each attempt streams a new
message id, which is how the bridges notice a restart (§2). The SDK
bridges build one per
call from the gateway's settings (models are cached by their spec) and
call `invoke` or `stream`. Nothing about retries, fallbacks, JSON mode or
streaming is implemented in Python or TypeScript (rule 3).

## 4. Not in this slice

Tools in `chat` (use an agent), images in and out, a token budget across
calls, rate limits (the gateway server's job, 0006 §7).

## 5. Tests

Simulated models only: a text reply; messages in and the shorthand; a
retried failure (`fail_first`) and a fallback (`reply.model` is the
fallback); JSON output; a stream's chunks add up to the reply; a
cancelled call; embeddings with the fake embedder; a bad model name.
