// Web tools in TypeScript (design 0019): search and page reading, against a local server only.

const test = require("node:test");
const assert = require("node:assert/strict");
const http = require("node:http");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { Agent, FakeModel, FakeReply, specialists, web } = sdk;

const SECRET = "tvly-secret-key-123";
const seen = [];

const server = http.createServer((req, res) => {
  const send = (status, body, type = "application/json") => {
    const data = Buffer.isBuffer(body) ? body : Buffer.from(typeof body === "string" ? body : JSON.stringify(body));
    res.writeHead(status, { "content-type": type, "content-length": data.length });
    res.end(data);
  };
  let raw = "";
  req.on("data", (c) => (raw += c));
  req.on("end", () => {
    if (req.method === "POST") {
      const payload = JSON.parse(raw);
      seen.push(["POST", req.url, req.headers.authorization, payload]);
      if (req.headers.authorization !== `Bearer ${SECRET}`) return send(401, { error: `bad key ${req.headers.authorization}` });
      const results = Array.from({ length: 20 }, (_, i) => ({ title: `Result ${i}`, url: `https://example.com/${i}`, content: "x".repeat(2000) }));
      return send(200, { answer: "Solar is growing.", results });
    }
    seen.push(["GET", req.url, req.headers["x-subscription-token"], null]);
    if (req.url.startsWith("/res/v1/web/search")) {
      return send(200, { web: { results: [{ title: "Brave hit", url: "https://example.org", description: "d" }] } });
    }
    if (req.url === "/page") {
      return send(200, "<html><head><title>t</title><script>evil()</script></head><body><h1>Hello</h1><p>World &amp; more</p></body></html>", "text/html");
    }
    if (req.url === "/redirect") {
      res.writeHead(302, { location: "http://127.0.0.1:1/elsewhere" });
      return res.end();
    }
    if (req.url === "/big") return send(200, Buffer.alloc(600_000, "a"), "text/plain");
    send(404, {});
  });
});

let base;
test.before(async () => {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  base = `http://127.0.0.1:${server.address().port}`;
});
test.after(() => server.close());

async function call(toolObject, name, args) {
  const result = await new Agent({ model: new FakeModel([FakeReply.toolCall(name, args), "done"]), tools: [toolObject] }).run("go");
  return result.messages.filter((m) => m.role === "tool")[0];
}

test("tavily returns bounded results and sends the key only as a header", async () => {
  const out = await call(web.tavilySearch({ apiKey: SECRET, maxResults: 3, baseUrl: base }), "web_search", { query: "solar panels" });
  assert.equal(out.status, "success");
  const value = out.content[0].value;
  assert.equal(value.answer, "Solar is growing.");
  assert.equal(value.results.length, 3); // asked for 3 of 20
  assert.ok(value.results.every((r) => r.snippet.length <= 600));
  const [, , auth, payload] = seen[seen.length - 1];
  assert.equal(auth, `Bearer ${SECRET}`);
  assert.equal(payload.query, "solar panels");
  assert.ok(!JSON.stringify(out).includes(SECRET)); // never in what the model sees
});

test("a wrong key gives a status, not the response body", async () => {
  const out = await call(web.tavilySearch({ apiKey: "wrong-key", baseUrl: base }), "web_search", { query: "x" });
  assert.equal(out.status, "error");
  const text = JSON.stringify(out);
  assert.ok(text.includes("401") && !text.includes("wrong-key") && !text.includes("bad key"));
});

test("search without any key says what to set", async () => {
  const saved = { t: process.env.TAVILY_API_KEY, b: process.env.BRAVE_API_KEY };
  delete process.env.TAVILY_API_KEY;
  delete process.env.BRAVE_API_KEY;
  try {
    assert.throws(() => web.search(), /TAVILY_API_KEY/);
    const out = await call(web.tavilySearch(), "web_search", { query: "x" });
    assert.equal(out.status, "error");
    assert.ok(JSON.stringify(out).includes("TAVILY_API_KEY"));
    assert.deepEqual(web.defaultTools(), []); // a researcher still works, just without the internet
  } finally {
    if (saved.t !== undefined) process.env.TAVILY_API_KEY = saved.t;
    if (saved.b !== undefined) process.env.BRAVE_API_KEY = saved.b;
  }
});

test("search picks the provider from the environment", async () => {
  const saved = { t: process.env.TAVILY_API_KEY, b: process.env.BRAVE_API_KEY };
  delete process.env.TAVILY_API_KEY;
  process.env.BRAVE_API_KEY = "brave-key";
  try {
    assert.equal(web.search().name, "web_search");
    const out = await call(web.braveSearch({ baseUrl: base }), "web_search", { query: "rain" });
    assert.equal(out.content[0].value.results[0].title, "Brave hit");
    assert.equal(seen[seen.length - 1][2], "brave-key");
    process.env.TAVILY_API_KEY = SECRET;
    assert.equal(web.defaultTools().length, 2); // search + read_page
  } finally {
    for (const [name, value] of [["TAVILY_API_KEY", saved.t], ["BRAVE_API_KEY", saved.b]]) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  }
});

test("credentials do not go over plain http to another machine", () => {
  assert.throws(() => web.tavilySearch({ apiKey: "k", baseUrl: "http://search.example.com" }), /https/);
});

test("fetch reads text, skips scripts and refuses private addresses by default", async () => {
  const refused = await call(web.fetch(), "read_page", { url: `${base}/page` });
  assert.equal(refused.status, "error");
  assert.ok(JSON.stringify(refused).includes("private network"));
  const ok = await call(web.fetch({ allowPrivate: true }), "read_page", { url: `${base}/page` });
  const value = ok.content[0].value;
  assert.ok(value.text.includes("Hello") && value.text.includes("World & more") && !value.text.includes("evil"));
  const bad = await call(web.fetch({ allowPrivate: true }), "read_page", { url: "file:///etc/passwd" });
  assert.equal(bad.status, "error");
});

test("fetch does not follow redirects and bounds what it reads", async () => {
  const redirected = await call(web.fetch({ allowPrivate: true }), "read_page", { url: `${base}/redirect` });
  assert.equal(redirected.status, "error");
  assert.ok(JSON.stringify(redirected).includes("reading the page failed: HTTP 302 (redirects are not followed)"));
  const big = await call(web.fetch({ allowPrivate: true }), "read_page", { url: `${base}/big` });
  assert.equal(big.status, "error");
  assert.ok(JSON.stringify(big).includes("reading the page failed: the response is larger than 500 KB"));
  assert.ok(!JSON.stringify([redirected, big]).includes("search failed"));
});

test("fetch refuses private IPv4, IPv6 and mapped addresses by name", async () => {
  for (const url of ["http://10.0.0.5/", "http://192.168.1.1/", "http://169.254.169.254/latest/meta-data", "http://[::1]/", "http://[::ffff:127.0.0.1]/", "http://[fd00::1]/"]) {
    const out = await call(web.fetch(), "read_page", { url });
    assert.equal(out.status, "error", url);
    assert.ok(JSON.stringify(out).includes("private network"), url);
  }
});

test("the researcher gets web tools only when a key is set", async () => {
  const saved = { t: process.env.TAVILY_API_KEY, b: process.env.BRAVE_API_KEY };
  process.env.TAVILY_API_KEY = SECRET;
  try {
    assert.equal((await specialists.researcher.build(new FakeModel(["ok"])).run("x")).text, "ok");
    delete process.env.TAVILY_API_KEY;
    delete process.env.BRAVE_API_KEY;
    assert.equal((await specialists.researcher.build(new FakeModel(["ok"])).run("x")).text, "ok");
    assert.ok(specialists.researcher.extend("r2", { add: "Be brief." }).settings.defaultTools); // extend keeps the default
  } finally {
    for (const [name, value] of [["TAVILY_API_KEY", saved.t], ["BRAVE_API_KEY", saved.b]]) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  }
});
