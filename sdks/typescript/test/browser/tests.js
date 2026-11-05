// The browser engine (design 0021) in a real browser: the Rust engine as
// WebAssembly, driven by the same TypeScript API as on servers. Simulated
// models (FakeModel) and a local mock provider only. Each timing claim is
// one that would fail if it were false (rule 7).

import * as criogen from "/criogen.js";

const {
  Agent,
  FakeModel,
  FakeReply,
  FunctionModel,
  Gateway,
  Storage,
  Telemetry,
  Workflow,
  END,
  anthropic,
  goto,
  suspend,
  isCriogenError,
  openaiCompatible,
  ready,
  tool,
  web,
} = criogen;

/** The mock provider's key (a test value, see server.mjs). */
const KEY = "sk-browser-test-key-0123456789abcdef";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function equal(actual, expected, what) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${what}: got ${JSON.stringify(actual)}, expected ${JSON.stringify(expected)}`);
  }
}

async function rejects(promise, check, what) {
  try {
    await promise;
  } catch (error) {
    if (!check(error)) throw new Error(`${what}: wrong error: ${error?.stack ?? error}`);
    return error;
  }
  throw new Error(`${what}: did not fail`);
}

function throws(fn, check, what) {
  try {
    fn();
  } catch (error) {
    if (!check(error)) throw new Error(`${what}: wrong error: ${error?.stack ?? error}`);
    return error;
  }
  throw new Error(`${what}: did not throw`);
}

const add = tool({
  name: "add",
  description: "Adds two numbers.",
  input: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] },
  run: ({ a, b }) => a + b,
});

const wait = tool({
  name: "wait",
  description: "Waits.",
  input: { type: "object", properties: { ms: { type: "number" } }, required: ["ms"] },
  run: async ({ ms }) => {
    await sleep(ms);
    return `waited ${ms}`;
  },
});

export async function run({ provider }) {
  const results = [];
  const check = async (name, fn) => {
    const started = performance.now();
    try {
      await fn();
      results.push({ name, ok: true, ms: Math.round(performance.now() - started) });
    } catch (error) {
      // Firefox and WebKit stacks leave out the message.
      const text = error instanceof Error ? `${error.message}
${error.stack ?? ""}` : String(error);
      results.push({ name, ok: false, ms: Math.round(performance.now() - started), error: text });
    }
  };
  const stats = async () => (await fetch(`${provider}/stats`)).json();

  // Before ready(): the engine is not loaded, and the error says what to do.
  await check("before ready(), first use says to call ready()", async () => {
    throws(() => new Agent({ model: new FakeModel(["x"]) }), (e) => isCriogenError(e, "native_unavailable") && /ready\(\)/.test(e.message), "agent before ready");
  });

  await check("ready() loads the WebAssembly engine", async () => {
    const report = await ready();
    assert(report.ok, `not ready: ${JSON.stringify(report)}`);
    equal(report.engine.arch, "wasm32", "arch");
    equal(report.engine.platform, "browser", "platform");
    // Loading again is free.
    assert((await ready()).ok, "second ready()");
  });

  await check("an agent calls a JavaScript tool and answers", async () => {
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("add", { a: 2, b: 3 }), "the sum is 5"]), tools: [add] });
    const result = await agent.run("what is 2 + 3?");
    equal(result.text, "the sum is 5", "text");
    equal(result.toolCalls, 1, "tool calls");
    equal(result.modelCalls, 2, "model calls");
    const toolResult = result.messages.find((m) => m.role === "tool");
    assert(toolResult?.content?.[0]?.value === 5, `the tool result: ${JSON.stringify(toolResult)}`);
  });

  await check("three tool calls run at the same time (300 ms each, about 300 ms together)", async () => {
    const calls = [["wait", { ms: 300 }], ["wait", { ms: 300 }], ["wait", { ms: 300 }]];
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCalls(calls), "done"]), tools: [wait] });
    const started = performance.now();
    const result = await agent.run("wait three times");
    const took = performance.now() - started;
    equal(result.toolCalls, 3, "tool calls");
    assert(took >= 290, `finished before the tools: ${took} ms`);
    assert(took < 800, `not concurrent: ${took} ms (one after another takes 900)`);
  });

  await check("a tool that hangs times out, and the model is told", async () => {
    const hang = tool({ name: "hang", description: "Never answers.", input: { type: "object", properties: {} }, run: () => new Promise(() => {}) });
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("hang"), "recovered"]), tools: [hang], limits: { toolTimeoutSecs: 0.3 } });
    const started = performance.now();
    const result = await agent.run("try it");
    const took = performance.now() - started;
    equal(result.text, "recovered", "text");
    assert(took >= 290 && took < 3000, `timeout took ${took} ms`);
    assert(/tim(e|ed) ?out/i.test(JSON.stringify(result.messages)), "the model saw a timeout");
  });

  await check("an AbortSignal cancels a run at once", async () => {
    const agent = new Agent({ model: new FakeModel(["too late"], { delayMs: 5000 }) });
    const controller = new AbortController();
    setTimeout(() => controller.abort(), 100);
    const started = performance.now();
    await rejects(agent.run("hi", { signal: controller.signal }), (e) => isCriogenError(e, "cancelled"), "cancelled run");
    const took = performance.now() - started;
    assert(took < 1500, `cancel took ${took} ms`);
  });

  await check("a thread streams events as they happen", async () => {
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("add", { a: 1, b: 1 }), "two"]), tools: [add] });
    const stream = agent.thread("stream-1").stream("1 + 1?");
    const types = [];
    for await (const event of stream) types.push(event.type);
    const reply = await stream.reply();
    equal(reply.text, "two", "reply");
    assert(types.includes("tool_call_started") && types.includes("tool_call_finished"), `events: ${types}`);
    assert(types.indexOf("tool_call_started") < types.indexOf("tool_call_finished"), "events in order");
  });

  await check("leaving a stream early cancels its run", async () => {
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("wait", { ms: 2000 }), "never"]), tools: [wait] });
    const stream = agent.thread("stream-2").stream("go");
    for await (const event of stream) {
      if (event.type === "tool_call_started") break;
    }
    await rejects(stream.reply(), (e) => isCriogenError(e, "cancelled"), "reply after leaving");
  });

  await check("approval pauses before a tool; approve() runs it", async () => {
    const deleted = [];
    const remove = tool({
      name: "delete",
      description: "Deletes a record.",
      input: { type: "object", properties: { id: { type: "number" } }, required: ["id"] },
      destructive: true,
      run: ({ id }) => {
        deleted.push(id);
        return "deleted";
      },
    });
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("delete", { id: 7 }), "record 7 is gone"]), tools: [remove], approve: ["delete"] });
    const thread = agent.thread("approval-1");
    const paused = await thread.send("delete record 7");
    equal(paused.needsApproval, true, "needs approval");
    equal(paused.approvals[0].tool, "delete", "the call waiting");
    equal(deleted, [], "nothing ran before approval");
    const done = await thread.approve();
    equal(done.text, "record 7 is gone", "after approval");
    equal(deleted, [7], "ran once, after approval");
  });

  await check("typed output is parsed and checked", async () => {
    const output = { type: "object", properties: { category: { type: "string" }, refund: { type: "boolean" } }, required: ["category", "refund"], additionalProperties: false };
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("final_output", { category: "billing", refund: true })]), output });
    equal((await agent.run("charged twice")).output, { category: "billing", refund: true }, "output");
  });

  await check("hooks see the run and can change a tool's result", async () => {
    const seen = [];
    const agent = new Agent({
      model: new FakeModel([FakeReply.toolCall("add", { a: 2, b: 2 }), "ok"]),
      tools: [add],
      hooks: [
        {
          beforeModel(ctx) {
            seen.push(`model ${ctx.modelCalls}`);
          },
          afterTool(_call, result) {
            seen.push(`tool ${result}`);
            return "four (checked)";
          },
        },
      ],
    });
    const result = await agent.run("2 + 2");
    assert(JSON.stringify(result.messages).includes("four (checked)"), "the changed result");
    assert(seen.includes("tool 4") && seen.some((s) => s.startsWith("model")), `hook calls: ${seen}`);
  });

  await check("a tool that throws: the model sees it, or the run fails with the cause", async () => {
    const boom = tool({ name: "boom", description: "Throws.", input: { type: "object", properties: {} }, run: () => { throw new RangeError("out of range"); } });
    const told = new Agent({ model: new FakeModel([FakeReply.toolCall("boom"), "handled"]), tools: [boom] });
    const result = await told.run("go");
    equal(result.text, "handled", "default: back to the model");
    assert(JSON.stringify(result.messages).includes("out of range"), "the model saw the error");
    const strict = new Agent({ model: new FakeModel([FakeReply.toolCall("boom"), "never"]), tools: [boom], onToolException: "fail" });
    const error = await rejects(strict.run("go"), (e) => isCriogenError(e, "tool_failed"), "fail mode");
    assert(error.cause instanceof RangeError, "the cause is what the tool threw");
  });

  await check("threads remember in memory, and rewind", async () => {
    const agent = new Agent({ model: new FakeModel(["first answer", "second answer"]) });
    const thread = agent.thread("memory-1");
    await thread.send("one");
    const second = await thread.send("two");
    equal(second.text, "second answer", "second turn");
    const messages = await thread.messages();
    assert(messages.length >= 4, `messages: ${messages.length}`);
    const history = await thread.history();
    assert(history.length >= 2, `checkpoints: ${history.length}`);
    await thread.rewind(history[history.length - 1].checkpoint_id ?? history[history.length - 1].id);
    assert((await thread.messages()).length < messages.length, "rewound");
  });

  await check("a workflow routes, fans out and joins", async () => {
    const wf = new Workflow({ reducers: { log: "append" } })
      .node("classify", () => ({ log: ["classify"] }))
      .node("fan", () => undefined)
      .node("searchWeb", async () => {
        await sleep(20);
        return { log: ["web"], web: 1 };
      })
      .node("searchDocs", () => ({ log: ["docs"], docs: 2 }))
      .node("answer", (s) => ({ answer: s.web + s.docs }))
      .node("refuse", () => ({ answer: null }))
      .start("classify")
      .route("classify", (s) => (s.question ? "ok" : "empty"), { ok: "fan", empty: "refuse" })
      .edge("fan", "searchWeb")
      .edge("fan", "searchDocs")
      .join(["searchWeb", "searchDocs"], "answer")
      .edge("answer", END)
      .edge("refuse", END);
    const app = wf.compile();
    const result = await app.run({ question: "why?" });
    equal(result.state.answer, 3, "joined answer");
    equal(result.state.log.length, 3, "appended log");
    equal((await app.run({ question: "" })).state.answer, null, "the other route");
  });

  await check("fifty workflow nodes run at the same time (200 ms each)", async () => {
    const wf = new Workflow({ reducers: { done: "append" } }).node("start", () => undefined).start("start");
    const names = [];
    for (let i = 0; i < 50; i++) {
      const name = `n${i}`;
      names.push(name);
      wf.node(name, async () => {
        await sleep(200);
        return { done: [i] };
      });
      wf.edge("start", name);
    }
    wf.node("end", (s) => ({ count: s.done.length })).join(names, "end").edge("end", END);
    const started = performance.now();
    const result = await wf.compile().run({});
    const took = performance.now() - started;
    equal(result.state.count, 50, "all ran");
    assert(took < 1500, `not concurrent: ${took} ms (one after another takes 10 s)`);
  });

  await check("goto, and a suspended workflow resumes from its snapshot", async () => {
    const paid = [];
    const wf = new Workflow()
      .node("check", (s) => (s.amount > 10 ? goto("approve", { status: "waiting" }) : goto("pay")))
      .node("approve", (_s, ctx) => (ctx.resume === undefined ? suspend({ refund: 40 }) : { approved: ctx.resume === "yes" }))
      .node("pay", (s) => {
        paid.push(s.amount);
        return { paid: true };
      })
      .start("check")
      .goto("check", ["approve", "pay"])
      .edge("approve", "pay")
      .edge("pay", END);
    const app = wf.compile();
    const paused = await app.run({ amount: 40 });
    equal(paused.suspended, true, "paused");
    equal(paused.interrupts[0].payload, { refund: 40 }, "the question");
    const resumed = await app.resume(paused.snapshot, { approve: "yes" });
    equal(resumed.state.paid, true, "paid after resume");
    equal(paid, [40], "paid once");
  });

  await check("a run deadline stops a slow workflow", async () => {
    const wf = new Workflow().node("slow", async () => {
      await sleep(3000);
      return {};
    }).start("slow").edge("slow", END);
    const started = performance.now();
    await rejects(wf.compile().run({}, { deadlineMs: 200 }), (e) => isCriogenError(e, "deadline_exceeded"), "deadline");
    assert(performance.now() - started < 1500, "stopped at the deadline");
  });

  await check("the gateway answers with a simulated model", async () => {
    const reply = await new Gateway().chat(new FakeModel(["hello from the gateway"]), "hi");
    equal(reply.text, "hello from the gateway", "text");
  });

  // A real provider protocol, through fetch, on another origin.
  const model = (path, name = "echo") => openaiCompatible(name, { baseUrl: `${provider}${path}`, apiKey: KEY });

  await check("a provider answers through fetch (OpenAI-compatible, another origin)", async () => {
    const before = (await stats()).requests;
    const result = await new Agent({ model: model("/v1") }).run("hello browser");
    equal(result.text, "you said: hello browser", "text");
    const after = await stats();
    assert(after.requests > before, "the provider was called");
    equal(after.auth[after.auth.length - 1], `Bearer ${KEY}`, "the key went to the provider only, as a header");
  });

  await check("a provider's streamed answer arrives in pieces (SSE)", async () => {
    const stream = new Gateway().stream(model("/v1"), "count");
    const chunks = [];
    for await (const chunk of stream) if (chunk.type === "text") chunks.push(chunk.text);
    const reply = await stream.reply();
    assert(chunks.length >= 3, `chunks: ${chunks.length}`);
    equal(reply.text, "one two three four", "joined");
  });

  await check("transient errors are retried (503, then 200)", async () => {
    const before = (await stats()).flaky;
    const result = await new Agent({ model: model("/flaky/v1"), modelRetry: { initialBackoffSecs: 0.05, jitter: false } }).run("again");
    equal(result.text, "you said: again", "answer after the retry");
    equal((await stats()).flaky - before, 2, "one failure, one retry");
  });

  await check("a redirect is refused and never followed", async () => {
    const before = (await stats()).stolen;
    const error = await rejects(new Agent({ model: model("/redirect/v1"), modelRetry: { maxAttempts: 1 } }).run("hi"), (e) => isCriogenError(e, "model_failed"), "redirect");
    assert(/redirect/i.test(error.message), `message: ${error.message}`);
    equal((await stats()).stolen - before, 0, "the redirect target never got the request");
  });

  await check("the API key never appears in an error", async () => {
    const error = await rejects(new Agent({ model: model("/leak/v1"), modelRetry: { maxAttempts: 1 } }).run("hi"), (e) => isCriogenError(e, "model_failed"), "401");
    assert(!error.message.includes(KEY), "the key is in the message");
    assert(!JSON.stringify(error.details).includes(KEY), "the key is in the details");
    assert(!String(error.stack).includes(KEY), "the key is in the stack");
  });

  await check("a provider that refuses browser calls gets a CORS hint", async () => {
    const error = await rejects(new Agent({ model: model("/nocors/v1"), modelRetry: { maxAttempts: 1 } }).run("hi"), (e) => isCriogenError(e, "model_failed"), "no CORS");
    assert(/CORS/.test(error.message), `message: ${error.message}`);
  });

  await check("a response over 64 MiB is refused", async () => {
    const error = await rejects(new Agent({ model: model("/huge/v1"), modelRetry: { maxAttempts: 1 } }).run("hi"), (e) => isCriogenError(e, "model_failed"), "huge");
    assert(/larger than the limit/.test(error.message), `message: ${error.message}`);
  });

  await check("a key is never sent over plain http to another machine", async () => {
    const before = (await stats()).requests;
    const insecure = openaiCompatible("echo", { baseUrl: "http://example.com/v1", apiKey: KEY });
    let error;
    try {
      await new Agent({ model: insecure, modelRetry: { maxAttempts: 1 } }).run("hi");
    } catch (e) {
      error = e;
    }
    assert(error, "it was allowed");
    assert(!String(error.message).includes(KEY), "the key is in the message");
    assert(/http/i.test(error.message), `message: ${error.message}`);
    equal((await stats()).requests, before, "nothing was sent");
  });

  await check("a JavaScript model answers several agents at once, by what each asks", async () => {
    const model = new FunctionModel(async (call) => {
      await sleep(50);
      const system = call.messages.find((m) => m.role === "system")?.content?.[0]?.text ?? "";
      return system.includes("judge") ? "judged" : "written";
    });
    const [a, b] = await Promise.all([new Agent({ model, instructions: "You judge." }).run("x"), new Agent({ model, instructions: "You write." }).run("y")]);
    equal([a.text, b.text], ["judged", "written"], "answers");
  });

  await check("Anthropic's API gets its browser-access header and the key", async () => {
    // run.mjs answers api.anthropic.com locally (Playwright), saying what it saw.
    const result = await new Agent({ model: anthropic("claude-haiku-5-5", { apiKey: KEY }), modelRetry: { maxAttempts: 1 } }).run("hi");
    equal(result.text, "browser-access=true key=ok", "what the API saw");
  });

  await check("server-only features say so", async () => {
    throws(() => new Storage({ url: "sqlite://threads.db" }), (e) => isCriogenError(e, "unsupported") && /servers/.test(e.message), "SQLite");
    throws(() => Telemetry.otlp("https://example.com/v1/traces"), (e) => isCriogenError(e, "unsupported"), "telemetry");
    throws(() => web.search(), (e) => isCriogenError(e, "unsupported"), "web tools");
  });

  await check("a Web Worker runs agents too", async () => {
    const worker = new Worker("/worker.js", { type: "module" });
    const answer = await new Promise((resolve, reject) => {
      worker.onmessage = (event) => resolve(event.data);
      worker.onerror = (event) => reject(new Error(event.message || "worker error"));
      worker.postMessage("go");
      setTimeout(() => reject(new Error("the worker did not answer")), 20000);
    });
    worker.terminate();
    assert(answer.ok, `worker: ${answer.error}`);
    equal(answer.text, "forty-two", "worker answer");
    equal(answer.toolCalls, 1, "worker tool call");
  });

  return results;
}
