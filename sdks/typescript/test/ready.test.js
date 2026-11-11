// ready() and lazy engine loading: importing criogen never throws; the first
// use on a system where the engine cannot load throws NativeUnavailableError.

const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

// CRIOGEN_ENGINE=wasm runs this suite on the browser's engine (design 0021);
// what needs a server is skipped there, with the reason.
const serverOnly = (why) => (process.env.CRIOGEN_ENGINE === "wasm" ? `server only: ${why}` : false);

const SDK = process.env.CRIOGEN_SDK || path.join(__dirname, "..");
const { ready, FakeModel, kaggle } = require(SDK);

test("ready() reports the engine, and a model that answers", async () => {
  const bare = await ready();
  assert.equal(bare.ok, true);
  assert.equal(bare.engine.ok, true);
  assert.equal(bare.engine.platform, process.platform);
  assert.equal(bare.model, undefined);

  const withModel = await ready({ model: new FakeModel(["OK"]) });
  assert.equal(withModel.ok, true);
  assert.equal(withModel.model.ok, true);
  assert.ok(withModel.model.latencyMs >= 0);
  assert.equal(withModel.model.model, "FakeModel");
});

test("ready() reports a model it cannot use, and never throws", async () => {
  const saved = { ...process.env };
  try {
    for (const name of ["MODEL_PROXY_URL", "MODEL_PROXY_API_KEY", "KAGGLE_API_TOKEN"]) delete process.env[name];
    process.env.KAGGLE_CONFIG_DIR = path.join(__dirname, "no-such-dir");
    const report = await ready({ model: kaggle("google/gemini-3.6-flash") });
    assert.equal(report.ok, false);
    assert.equal(report.engine.ok, true);
    assert.equal(report.model.ok, false);
    assert.match(report.model.error, /KAGGLE_API_TOKEN/);
  } finally {
    for (const k of Object.keys(process.env)) if (!(k in saved)) delete process.env[k];
    Object.assign(process.env, saved);
  }
});

test("an engine that cannot load: import works, first use throws a typed error, ready() reports it", { skip: serverOnly("loading the Node addon") }, () => {
  // A child process where loading any criogen .node file fails as on a system with an old glibc.
  const script = `
    const Module = require("module");
    const original = Module._load;
    Module._load = function (request, ...rest) {
      if (/criogen\\.node$/.test(request) || /^criogen-(linux|darwin|win32|windows)/.test(request)) {
        const e = new Error("/lib64/libc.so.6: version 'GLIBC_2.38' not found");
        e.code = "ERR_DLOPEN_FAILED";
        throw e;
      }
      return original.call(this, request, ...rest);
    };
    const sdk = require(${JSON.stringify(SDK)});           // must not throw
    let error;
    try { new sdk.Agent({ model: new sdk.FakeModel(["x"]) }); } catch (e) { error = e; }
    let again;
    try { new sdk.Agent({ model: new sdk.FakeModel(["x"]) }); } catch (e) { again = e; }
    sdk.ready().then((report) => {
      process.stdout.write(JSON.stringify({
        name: error && error.name, code: error && error.code, details: error && error.details,
        message: error && error.message, sameError: error === again,
        isClass: error instanceof sdk.NativeUnavailableError, report,
      }));
    });
  `;
  const out = JSON.parse(execFileSync(process.execPath, ["-e", script], { encoding: "utf8" }));
  assert.equal(out.name, "NativeUnavailableError");
  assert.equal(out.isClass, true);
  assert.equal(out.code, "native_unavailable");
  assert.equal(out.details.platform, process.platform);
  assert.equal(out.details.arch, process.arch);
  assert.match(out.message, /GLIBC_2\.38/);
  assert.match(out.message, /glibc 2\.28 or newer/);
  assert.equal(out.sameError, true);
  assert.equal(out.report.ok, false);
  assert.equal(out.report.engine.ok, false);
  assert.equal(out.report.engine.code, "native_unavailable");
});
