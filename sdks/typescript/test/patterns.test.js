// Patterns and specialists in TypeScript (design 0019). Simulated models only.
// Runs against the source build, or the installed package when AGENTBUILD_SDK
// names it (scripts/test-typescript.sh).

const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");

const sdk = require(process.env.AGENTBUILD_SDK || path.join(__dirname, ".."));
const { Agent, END, FakeModel, FakeReply, LimitExceededError, Storage, Workflow, patterns, specialists, tool } = sdk;

const agent = (replies, options) => new Agent({ model: new FakeModel(replies, options) });

test("sequential passes text along and traces every step", async () => {
  const seen = [];
  const shout = function shout(text) {
    seen.push(text);
    return text.toUpperCase();
  };
  const flow = patterns.sequential([["research", agent(["facts about solar"])], shout, ["write", agent(["A short article."])]]);
  const result = await flow.run("solar panels");
  assert.equal(result.output, "A short article.");
  assert.deepEqual(seen, ["facts about solar"]);
  assert.deepEqual(result.trace.map((t) => t.step), ["research", "shout", "write"]);
  assert.equal(result.stopReason, "done");
  assert.equal(String(result), "A short article.");
  assert.match(result.explain(), /1\. research/);
});

test("steps can be registered specialist names with a model argument", async () => {
  const flow = patterns.sequential(["researcher", "writer"], { model: new FakeModel(["same answer"]) });
  const result = await flow.run("topic");
  assert.equal(result.output, "same answer");
  assert.deepEqual(result.trace.map((t) => t.step), ["researcher", "writer"]);
});

test("a specialist without a model explains what to do", () => {
  const saved = process.env.AGENTBUILD_MODEL;
  delete process.env.AGENTBUILD_MODEL;
  try {
    assert.throws(() => patterns.sequential(["researcher"]), /AGENTBUILD_MODEL/);
    assert.throws(() => patterns.sequential(["no-such-specialist"], { model: new FakeModel(["x"]) }), /registered/);
    assert.throws(() => patterns.sequential([42]), TypeError);
  } finally {
    if (saved !== undefined) process.env.AGENTBUILD_MODEL = saved;
  }
});

test("parallel runs branches together and combines in order", async () => {
  const branches = { sentiment: agent(["positive"]), keywords: agent(["solar, cost"]), urgency: agent(["low"]) };
  const flow = patterns.parallel(branches, { synthesizer: ["summary", agent(["Positive, about solar, low urgency."])] });
  const result = await flow.run("I love my new solar panels");
  assert.equal(result.output, "Positive, about solar, low urgency.");
  assert.deepEqual(new Set(result.trace.map((t) => t.step)), new Set(["sentiment", "keywords", "urgency", "summary (synthesis)"]));

  const plain = await patterns.parallel({ a: agent(["one"]), b: agent(["two"]) }).run("x");
  assert.equal(plain.output, "## a\none\n\n## b\ntwo"); // branch order, whatever finished first
});

test("parallel branches really overlap", async () => {
  const slow = Object.fromEntries([0, 1, 2, 3].map((i) => [`b${i}`, agent(["done"], { delayMs: 400 })]));
  const started = Date.now();
  await patterns.parallel(slow).run("go");
  assert.ok(Date.now() - started < 1200, "four 400 ms branches, not 1600 ms");
});

test("loop stops on a condition or at the cap", async () => {
  let n = 0;
  const step = (text) => {
    n += 1;
    return `${text}+`;
  };
  const done = await patterns.loop([step], (text, round) => round === 3, { maxIterations: 10 }).run("x");
  assert.equal(done.output, "x+++");
  assert.equal(done.stopReason, "condition");
  assert.equal(n, 3);

  n = 0;
  const capped = await patterns.loop([step], () => false, { maxIterations: 4 }).run("x");
  assert.equal(capped.stopReason, "max_iterations");
  assert.equal(n, 4);

  n = 0;
  await patterns.loop([step], () => false, { maxIterations: 10_000 }).run("x");
  assert.equal(n, patterns.CEILING); // a hard ceiling whatever is asked
  assert.throws(() => patterns.loop([step], () => true, { maxIterations: 0 }), RangeError);
});

test("review and critique revises until approved", async () => {
  const result = await patterns
    .reviewAndCritique(agent(["draft one", "draft two", "draft three"]), agent(["REVISE: add a number", "REVISE: shorter", "APPROVED"]), { maxRounds: 5 })
    .run("write");
  assert.equal(result.output, "draft three");
  assert.equal(result.stopReason, "approved");
  assert.deepEqual(result.trace.slice(0, 2).map((t) => t.step), ["step1 (draft 1)", "step2"]);
  assert.equal(result.trace.length, 6);
});

test("review and critique keeps the last draft when never approved", async () => {
  const result = await patterns
    .reviewAndCritique(agent(["d1", "d2", "d3"]), agent(["REVISE: a", "REVISE: b", "REVISE: c"]), { maxRounds: 3 })
    .run("write");
  assert.equal(result.stopReason, "max_rounds");
  assert.equal(result.output, "d3");
});

test("review feedback reaches the generator", async () => {
  const seen = [];
  const generator = (text) => {
    seen.push(text);
    return `draft ${seen.length}`;
  };
  await patterns.reviewAndCritique(generator, agent(["REVISE: add a citation", "APPROVED"])).run("write");
  assert.match(seen[1], /add a citation/);
  assert.match(seen[1], /draft 1/);
});

test("iterative refinement stops at the threshold", async () => {
  const result = await patterns.iterativeRefinement((t) => `${t}!`, (t) => t.length / 5, { threshold: 1.0, maxIterations: 9 }).run("ab");
  assert.equal(result.output, "ab!!!");
  assert.equal(result.stopReason, "condition");
});

test("swarm hands off until final and is bounded", async () => {
  const team = {
    market: agent(["Demand is high.\nHANDOFF: engineering"]),
    engineering: agent(["Feasible in 3 months.\nHANDOFF: finance"]),
    finance: agent(["Budget fits.\nFINAL"]),
  };
  const result = await patterns.swarm(team, { start: "market" }).run("Should we build it?");
  assert.equal(result.stopReason, "final");
  assert.deepEqual(result.trace.map((t) => t.step), ["market", "engineering", "finance"]);

  const ping = { a: agent(Array(10).fill("x\nHANDOFF: b")), b: agent(Array(10).fill("y\nHANDOFF: a")) };
  const endless = await patterns.swarm(ping, { maxHandoffs: 3 }).run("go");
  assert.equal(endless.stopReason, "max_handoffs");
  assert.ok(endless.trace.length <= 5);

  const stray = await patterns.swarm({ a: agent(["x\nHANDOFF: nobody"]), b: agent(["y"]) }).run("go");
  assert.equal(stray.stopReason, "unknown_handoff"); // model output is data, not trusted

  assert.throws(() => patterns.swarm({ only: agent(["x"]) }), /at least two/);
});

test("a swarm dispatcher chooses the first agent", async () => {
  const team = { a: agent(["a did it\nFINAL"]), b: agent(["b did it\nFINAL"]) };
  const result = await patterns.swarm(team, { start: agent(["b"]) }).run("go");
  assert.equal(result.trace[1].step, "b");
  assert.match(result.output, /^b did it/);
});

test("human review pauses and resumes with a decision", async () => {
  const flow = patterns.humanReview(agent(["Release 120 rows."]));
  const paused = await flow.run("Release the dataset?");
  assert.equal(paused.suspended, true);
  assert.equal(paused.stopReason, "suspended");
  assert.equal(paused.waitingFor.draft, "Release 120 rows.");
  assert.equal((await flow.resume(paused, "approve")).output, "Release 120 rows.");

  const edited = await flow.resume(await flow.run("again"), { decision: "edit", text: "Release 100 rows." });
  assert.equal(edited.output, "Release 100 rows.");
  assert.equal(edited.stopReason, "approved");

  const rejected = await flow.resume(await flow.run("third"), { decision: "reject" });
  assert.equal(rejected.stopReason, "rejected");
  assert.equal(rejected.output, "");
  await assert.rejects(flow.resume(rejected, "approve"), /waiting for a person/);
});

test("human review is durable across compiled copies", async () => {
  const storage = new Storage({ audit: true });
  const first = patterns.humanReview(agent(["draft"])).compile(storage).thread("t-1", { actor: "user:ada" });
  const turn = await first.run({ input: "go" });
  assert.equal(turn.suspended, true);
  const other = patterns.humanReview(agent(["never used"])).compile(storage).thread("t-1", { actor: "user:boss" });
  const done = await other.resume({ review: "approve" });
  assert.equal(done.state.output, "draft");
  const records = await storage.auditRecords({ threadId: "t-1" });
  const actors = new Set(records.map((r) => r.actor));
  assert.ok(actors.has("user:ada") && actors.has("user:boss"));
  await storage.close();
});

test("custom wraps any workflow", async () => {
  const wf = new Workflow().node("only", (s) => ({ output: s.input.toUpperCase() })).start("only").edge("only", END);
  assert.equal((await patterns.custom(wf).run("hello")).output, "HELLO");
});

test("react caps model calls", async () => {
  const poke = tool({ name: "poke", description: "Pokes.", input: { type: "object", properties: {} }, run: () => "again" });
  const looping = patterns.react(new FakeModel(Array(20).fill(FakeReply.toolCall("poke", {}))), [poke], { maxSteps: 3 });
  await assert.rejects(looping.run("go"), LimitExceededError);
});

test("a coordinator routes to a specialist and nests as a step", async () => {
  const orders = new Agent({ model: new FakeModel(["Order A-1 shipped."]) });
  const triage = patterns.coordinator(
    new FakeModel([FakeReply.toolCall("orders", { input: "where is A-1" }), "It shipped."]),
    { orders: [orders, "order status questions"] },
  );
  assert.equal((await triage.run("where is A-1")).text, "It shipped.");
  const flow = patterns.sequential([["triage", new Agent({ model: new FakeModel(["ok"]) })]]);
  assert.equal((await flow.run("x")).output, "ok");
});

test("a coordinator checks its specialists", () => {
  assert.throws(() => patterns.coordinator(new FakeModel([]), {}), /at least one/);
  assert.throws(() => patterns.coordinator(new FakeModel([]), { x: 5 }), TypeError);
});

test("hierarchical builds coordinators of coordinators", () => {
  const root = patterns.hierarchical(new FakeModel(["done"]), {
    research: { _description: "gathers facts", web: new Agent({ model: new FakeModel(["web facts"]) }) },
    writing: new Agent({ model: new FakeModel(["x"]) }),
  });
  assert.ok(root instanceof Agent);
});

// ---------------------------------------------------------------- one-liners

test("quick builds a pattern from one line", async () => {
  const m = new FakeModel(["ok"]);
  assert.deepEqual((await patterns.quick("researcher > writer", { model: m }).run("x")).trace.map((t) => t.step), ["researcher", "writer"]);
  assert.deepEqual(new Set((await patterns.quick("researcher | critic", { model: m }).run("x")).trace.map((t) => t.step)), new Set(["researcher", "critic"]));
  assert.equal((await patterns.quick("writer <-> critic", { model: new FakeModel(["APPROVED"]) }).run("x")).stopReason, "approved");
  assert.equal((await patterns.quick("summarizer", { model: m }).run("x")).output, "ok");
});

test("quick explains what it cannot read", () => {
  const m = new FakeModel(["ok"]);
  assert.throws(() => patterns.quick("researcher > writer | critic", { model: m }), /one kind of connector/);
  assert.throws(() => patterns.quick("researcher >", { model: m }), /empty/);
  assert.throws(() => patterns.quick("researcher > nobody", { model: m }), /registered/);
  assert.throws(() => patterns.quick("writer <-> critic <-> researcher", { model: m }), /exactly two/);
});

// ---------------------------------------------------------------- specialists

test("built-in specialists are registered and build agents", async () => {
  const names = specialists.available();
  for (const expected of ["researcher", "writer", "critic", "classifier", "planner", "support_agent", "fact_checker", "code_reviewer"]) {
    assert.ok(names.includes(expected), expected);
  }
  const built = specialists.researcher.build(new FakeModel(["ok"]));
  assert.equal((await built.run("x")).text, "ok");
});

test("extend makes a new recipe and leaves the original alone", () => {
  const original = specialists.researcher.instructions;
  const careful = specialists.researcher.extend("careful-researcher", { add: "Always give two sources." });
  assert.equal(careful.name, "careful-researcher");
  assert.ok(careful.instructions.endsWith("Always give two sources.") && careful.instructions.startsWith(original));
  assert.equal(specialists.researcher.instructions, original);

  const search = tool({ name: "search", description: "Searches.", input: { type: "object", properties: {} }, run: () => "result" });
  const withTools = careful.extend("searcher", { tools: [search] });
  assert.equal(withTools.settings.tools.length, 1);
  assert.equal((careful.settings.tools ?? []).length, 0);
  assert.equal(specialists.writer.extend("poet", { instructions: "Write only poems." }).instructions, "Write only poems.");
});

test("register, get and name rules", async () => {
  const mine = new specialists.Specialist("my-triage", "Triage for my shop", "Be quick.");
  specialists.register(mine);
  try {
    assert.equal(specialists.get("my-triage"), mine);
    assert.throws(() => specialists.register(new specialists.Specialist("my-triage", "d", "i")), /already registered/);
    specialists.register(new specialists.Specialist("my-triage", "d2", "i2"), { replace: true });
    const flow = patterns.sequential(["my-triage"], { model: new FakeModel(["done"]) });
    assert.equal((await flow.run("x")).output, "done"); // a registered name works as a step
  } finally {
    specialists.unregister("my-triage");
  }
  assert.throws(() => specialists.get("my-triage"));
  assert.throws(() => new specialists.Specialist("bad name!", "d", "i"), TypeError);
  assert.throws(() => new specialists.Specialist("fine", "  ", "i"), TypeError);
});
