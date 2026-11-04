// API keys from the environment and a .env file (design 0012 §2).

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { loadEnv, parseEnv, envStatus, keyName, openai } = sdk;
const { KEYS } = require(path.join(process.env.CRIOGEN_SDK || path.join(__dirname, ".."), "dist", "models.js"));

const NAMES = [...Object.values(KEYS), "MY_OTHER_VAR"];
const saved = Object.fromEntries(NAMES.map((n) => [n, process.env[n]]));
function clean() {
  for (const n of NAMES) delete process.env[n];
}
function restore() {
  for (const n of NAMES) {
    if (saved[n] === undefined) delete process.env[n];
    else process.env[n] = saved[n];
  }
}
function tmpEnv(text) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "criogen-env-"));
  const file = path.join(dir, ".env");
  fs.writeFileSync(file, text);
  return file;
}

test("parse handles comments, quotes, export and inline comments", () => {
  const values = parseEnv(
    [
      "# keys",
      "OPENAI_API_KEY=sk-one",
      "export GEMINI_API_KEY = 'g-two'",
      'ANTHROPIC_API_KEY="a three"  # the Claude key',
      "MY_OTHER_VAR=plain # trailing",
      "",
      "EMPTY=",
    ].join("\r\n"),
  );
  assert.deepEqual(values, {
    OPENAI_API_KEY: "sk-one",
    GEMINI_API_KEY: "g-two",
    ANTHROPIC_API_KEY: "a three",
    MY_OTHER_VAR: "plain",
    EMPTY: "",
  });
});

test("a bad line names the line but never shows its content", () => {
  assert.throws(
    () => parseEnv("OPENAI_API_KEY=ok\nthis is secret-ish text\n"),
    (e) => /line 2/.test(e.message) && !/secret/.test(e.message),
  );
  assert.throws(() => parseEnv("A='oops\n"), /not closed/);
  assert.throws(() => parseEnv("A='x' y\n"), /after a quoted value/);
});

test("loadEnv sets missing variables and returns only names", () => {
  clean();
  try {
    const file = tmpEnv("OPENAI_API_KEY=sk-from-file\nGEMINI_API_KEY=g-from-file\n");
    process.env.GEMINI_API_KEY = "already-set";
    assert.deepEqual(loadEnv(file), ["OPENAI_API_KEY"]);
    assert.equal(process.env.OPENAI_API_KEY, "sk-from-file");
    assert.equal(process.env.GEMINI_API_KEY, "already-set");
    assert.deepEqual(loadEnv(file, { override: true }), ["GEMINI_API_KEY", "OPENAI_API_KEY"]);
    assert.equal(process.env.GEMINI_API_KEY, "g-from-file");
  } finally {
    restore();
  }
});

test("a missing file is fine unless required; a huge file is refused", () => {
  const missing = path.join(os.tmpdir(), "criogen-nope", ".env");
  assert.deepEqual(loadEnv(missing), []);
  assert.throws(() => loadEnv(missing, { required: true }), /ENOENT/);
  const big = tmpEnv("A=" + "x".repeat(1_100_000));
  assert.throws(() => loadEnv(big), /not an environment file/);
});

test("several providers side by side, and a missing key says which variable to set", () => {
  clean();
  try {
    loadEnv(tmpEnv("OPENAI_API_KEY=o\nANTHROPIC_API_KEY=a\n"));
    assert.deepEqual(
      envStatus(),
      Object.fromEntries(Object.keys(KEYS).map((name) => [name, name === "openai" || name === "anthropic"])),
    );
    assert.equal(keyName("gemini"), "GEMINI_API_KEY");
    assert.throws(() => keyName("nope"), /unknown provider/);
    delete process.env.OPENAI_API_KEY;
    process.env.ANTHROPIC_API_KEY = "sk-ant-very-secret";
    assert.throws(
      () => openai("gpt-5-mini"),
      (e) => /OPENAI_API_KEY/.test(e.message) && /\.env/.test(e.message) && !/very-secret/.test(e.message),
    );
    // a custom baseUrl (a gateway or local server) may need no key
    assert.doesNotThrow(() => openai("gpt-5-mini", { baseUrl: "http://127.0.0.1:9" }));
  } finally {
    restore();
  }
});
