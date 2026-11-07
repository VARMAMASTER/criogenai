// Collections in TypeScript (design 0025): documents searched by meaning, in both engines.

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { Agent, Collection, Embedder, FakeModel, FakeReply, Storage, isCriogenError } = sdk;
const wasm = process.env.CRIOGEN_ENGINE === "wasm";

const DOCS = [
  { id: "leave", text: "Annual leave is 25 days a year.", metadata: { section: "hr" } },
  { id: "expenses", text: "Expenses are paid within 30 days of the claim.", metadata: { section: "finance" } },
  { id: "parking", text: "Parking permits are issued by the front desk.", metadata: { section: "facilities" } },
];
const embedder = () => Embedder.fake({ synonyms: { vacation: "leave" } });

test("documents are found by meaning, filtered and removed", async () => {
  const docs = await Collection.open("handbook", { embedder: embedder() });
  assert.equal(await docs.add(DOCS), 3);
  const hits = await docs.search("how many vacation days", { k: 2 });
  assert.equal(hits[0].id, "leave");
  assert.deepEqual(hits[0].metadata, { section: "hr" });
  assert.ok(hits[0].score > hits[1].score);
  const only = await docs.search("days", { k: 3, where: { section: "finance" } });
  assert.deepEqual(only.map((h) => h.id), ["expenses"]);
  assert.equal(await docs.remove("leave"), true);
  assert.equal(await docs.remove("leave"), false);
  assert.equal(await docs.count(), 2);
  const hnsw = await Collection.open("h", { embedder: embedder(), index: "hnsw" });
  await hnsw.add(DOCS);
  assert.equal((await hnsw.search("vacation", { k: 1 }))[0].id, "leave");
});

test("a collection survives reopening a database", { skip: wasm && "databases need a server" }, async () => {
  const file = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "criogen-coll-")), "docs.db");
  let storage = new Storage({ url: `sqlite://${file}` });
  const docs = await Collection.open("handbook", { embedder: embedder(), storage });
  await docs.add(DOCS);
  await storage.close();
  storage = new Storage({ url: `sqlite://${file}` });
  const again = await Collection.open("handbook", { embedder: embedder(), storage });
  assert.equal(await again.count(), 3);
  assert.equal((await again.search("vacation", { k: 1 }))[0].id, "leave");
  await storage.close();
});

test("an agent searches through the tool, and errors have codes", async () => {
  const docs = await Collection.open("handbook", { embedder: embedder() });
  await docs.add(DOCS);
  const agent = new Agent({
    model: new FakeModel([FakeReply.toolCall("search_handbook", { query: "vacation days", k: 1 }), "25 days."]),
    tools: [docs.asTool("search_handbook", "Search the staff handbook.")],
  });
  const result = await agent.run("How much leave?");
  const tool = result.messages.find((m) => m.role === "tool");
  assert.equal(tool.content[0].value[0].id, "leave");

  const failing = await Collection.open("f", { embedder: Embedder.fake({ failing: true }) });
  await assert.rejects(failing.add([{ id: "a", text: "x" }]), (e) => isCriogenError(e, "model_failed"));
  await assert.rejects(docs.add([{ id: "", text: "x" }]), (e) => isCriogenError(e, "invalid_value"));
});
