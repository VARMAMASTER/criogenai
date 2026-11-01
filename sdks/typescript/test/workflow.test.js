// Workflows in TypeScript (design 0015), run by the Rust engine. Simulated
// models only. Runs against the source build, or the installed package when
// CRIOGEN_SDK names it (scripts/test-typescript.sh).

const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { END, Agent, CriogenError, FakeModel, Storage, ValidationError, Workflow, goto, suspend } = sdk;

test("a linear workflow with sync and async nodes", async () => {
  const wf = new Workflow()
    .node("clean", (state) => ({ text: state.text.trim().toLowerCase() }))
    .node("count", async (state, ctx) => {
      await new Promise((r) => setTimeout(r, 10));
      return { words: state.text.split(" ").length, countedAt: ctx.step };
    })
    .start("clean")
    .edge("clean", "count")
    .edge("count", END);
  const result = await wf.compile().run({ text: "  Hello Big World " });
  assert.equal(result.state.text, "hello big world");
  assert.equal(result.state.words, 3);
  assert.equal(result.state.countedAt, 1);
  assert.equal(result.steps, 2);
  assert.equal(result.suspended, false);
});

test("routes, parallel nodes, joins and reducers", async () => {
  const wf = new Workflow({ reducers: { log: "append" } })
    .node("classify", () => ({ log: ["classify"] }))
    .node("fan", () => undefined)
    .node("searchWeb", () => ({ log: ["web"], web: 1 }))
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
  assert.equal(result.state.answer, 3);
  assert.equal(result.state.log.length, 3);
  assert.equal(result.state.log[0], "classify");
  assert.equal((await app.run({ question: "" })).state.answer, null);
});

test("goto chooses the next node", async () => {
  const app = new Workflow()
    .node("check", (s) => goto(s.n > 10 ? "big" : "small", { checked: true }))
    .node("big", () => ({ size: "big" }))
    .node("small", () => ({ size: "small" }))
    .start("check")
    .goto("check", ["big", "small"])
    .edge("big", END)
    .edge("small", END)
    .compile();
  assert.deepEqual((await app.run({ n: 50 })).state, { n: 50, checked: true, size: "big" });
  assert.equal((await app.run({ n: 1 })).state.size, "small");
});

function approvalWorkflow() {
  const paid = [];
  const wf = new Workflow()
    .node("approve", (state, ctx) => {
      if (ctx.resume === undefined) return suspend({ refund: state.amount }, { status: "waiting" });
      return { approved: ctx.resume === "yes", status: "decided" };
    })
    .node("pay", (state) => {
      if (state.approved) paid.push(state.amount);
      return { paid: state.approved };
    })
    .start("approve")
    .edge("approve", "pay")
    .edge("pay", END);
  return { wf, paid };
}

test("a node pauses for a person and resumes", async () => {
  const { wf, paid } = approvalWorkflow();
  const app = wf.compile();
  const result = await app.run({ amount: 40 });
  assert.equal(result.suspended, true);
  assert.equal(result.interrupts[0].node, "approve");
  assert.deepEqual(result.interrupts[0].payload, { refund: 40 });
  assert.equal(result.state.status, "waiting");
  const resumed = await app.resume(result.snapshot, { approve: "yes" });
  assert.equal(resumed.state.paid, true);
  assert.deepEqual(paid, [40]);
  await assert.rejects(app.resume(result.snapshot, { pay: "yes" }), (e) => e instanceof CriogenError && e.code === "resume_invalid");
});

test("threads keep paused runs and another handle resumes", async () => {
  const storage = new Storage({ audit: true });
  const { wf, paid } = approvalWorkflow();
  const thread = wf.compile({ storage }).thread("refund-1", { actor: "user:ada" });
  let turn = await thread.run({ amount: 25 });
  assert.ok(turn.suspended && turn.version > 0);
  assert.deepEqual((await thread.interrupts())[0].payload, { refund: 25 });
  await assert.rejects(thread.run({ amount: 1 }), (e) => e.code === "thread_suspended");
  // Another compiled copy (another process, in real life).
  const other = wf.compile({ storage }).thread("refund-1", { actor: "user:boss" });
  turn = await other.resume({ approve: "yes" });
  assert.equal(turn.state.paid, true);
  assert.deepEqual(paid, [25]);
  assert.equal((await thread.state()).status, "decided");
  const history = await thread.history();
  assert.equal(history[0].status, "completed");
  const records = await storage.auditRecords({ threadId: "refund-1" });
  assert.deepEqual(records.filter((r) => r.action === "run.resumed").map((r) => r.actor), ["user:boss"]);
  assert.equal(await thread.delete(), true);
});

test("a failing node keeps what it threw and a thread retries it", async () => {
  class MailDown extends Error {}
  const calls = { prepare: 0, send: 0 };
  let down = true;
  const app = new Workflow()
    .node("prepare", () => {
      calls.prepare++;
      return { prepared: true };
    })
    .node("send", () => {
      calls.send++;
      if (down) throw new MailDown("mail server down");
      return { sent: true };
    })
    .start("prepare")
    .edge("prepare", "send")
    .edge("send", END)
    .compile({ storage: new Storage() });
  const thread = app.thread("mail-1");
  await assert.rejects(thread.run({}), (e) => {
    assert.equal(e.code, "node_failed");
    assert.equal(e.details.node, "send");
    assert.ok(e.cause instanceof MailDown);
    return true;
  });
  down = false;
  const turn = await thread.retry();
  assert.equal(turn.state.sent, true);
  assert.deepEqual(calls, { prepare: 1, send: 2 });
});

test("an agent as a node", async () => {
  const agent = new Agent({ model: new FakeModel(["Refund approved."]) });
  // Messages merge by id (upsertById), so each one carries an id.
  const question = { id: "m1", role: "user", content: [{ type: "text", text: "Refund?" }] };
  const result = await new Workflow({ reducers: { messages: "upsertById" } })
    .node("ask", () => ({ messages: [question] }))
    .addNode("agent", agent)
    .start("ask")
    .edge("ask", "agent")
    .edge("agent", END)
    .compile()
    .run({});
  assert.deepEqual(result.state.messages.map((m) => m.role), ["user", "assistant"]);
  assert.equal(result.state.messages.at(-1).content[0].text, "Refund approved.");
});

test("compile reports every problem at once", () => {
  const wf = new Workflow().node("a", () => undefined).node("orphan", () => undefined).edge("a", "missing");
  assert.throws(
    () => wf.compile(),
    (e) => {
      assert.ok(e instanceof ValidationError);
      assert.ok(e.details.issues.length >= 3, JSON.stringify(e.details.issues));
      assert.ok(e.details.issues.some((i) => i.includes("missing")));
      return true;
    },
  );
  assert.throws(() => new Workflow().node("a", () => undefined).node("a", () => undefined), /already exists/);
});

test("a node returning the wrong thing fails clearly; deadlines and AbortSignal stop a run", async () => {
  const bad = new Workflow().node("bad", () => 42).start("bad").edge("bad", END).compile();
  await assert.rejects(bad.run({}), (e) => e.code === "node_failed" && e.cause instanceof TypeError);

  const slow = new Workflow()
    .node("wait", () => new Promise((r) => setTimeout(r, 10_000).unref()))
    .start("wait")
    .edge("wait", END)
    .compile();
  await assert.rejects(slow.run({}, { deadlineMs: 200 }), (e) => e.code === "deadline_exceeded");
  const controller = new AbortController();
  setTimeout(() => controller.abort(), 100);
  await assert.rejects(slow.run({}, { signal: controller.signal }), (e) => e.code === "cancelled");
});
