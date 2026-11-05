# 0021: The engine in the browser (WebAssembly)

Status: approved by the user's request (2026-11-05: "lets go build it", after
the plan and the performance trade-offs were discussed). Built in Rust
(`criogen-rt`, `criogen-js`, the providers' `fetch` transport, the
`criogen-web` binding) and in the TypeScript SDK (`criogen/web`). Servers
are unchanged: the same crates build there exactly as before.

## Why

The website's playground ran a JavaScript copy of each pattern's control
flow, because the engine could not run in a page. That broke rule 3 (engine
semantics live in Rust only) in spirit, and the page had to say so. More
important for users: Criogen ran only where the Node addon or the Python
wheel loads. With the same engine compiled to WebAssembly:

- an agent can run in a visitor's browser (or a Web Worker): no server work
  per run, nothing to scale for the agent loop itself;
- the playground runs the code it shows, on the real engine;
- the TypeScript API is the same on servers and in the browser: one bridge
  (`criogen-js`) under both, one test suite checking both.

Servers stay the right place for long or durable runs, secrets, databases,
audit trails and patient data; see *Limits*.

## Design

### 1. Tasks and timers: `criogen-rt`

The engine used Tokio for tasks (`spawn`, `JoinSet`), timers (`sleep`,
`timeout`, `timeout_at`, `Instant`) and the wall clock. In a browser there
is no Tokio runtime (nothing may block the page's thread), and
`std::time::Instant::now()` / `SystemTime::now()` panic on
`wasm32-unknown-unknown`.

`criogen-rt` is the part of Tokio's API the engine uses:

- On servers (every target with an operating system) it *re-exports Tokio*:
  same types, same behaviour, the paused test clock still works. Nothing
  changes there.
- On `wasm32-unknown-unknown` it implements the same API on the JavaScript
  event loop: tasks through `wasm-bindgen-futures` (abort drops the future
  at its next turn; dropping a `JoinSet` aborts its tasks; a dropped
  `JoinHandle` detaches), timers through `setTimeout` (cleared when dropped,
  re-armed when a wait is longer than `setTimeout` allows, never ending
  early), the clock through `performance.now()` (never going back, with
  `Date.now()` as a fallback), and `system_now()` through `Date.now()`.
  Arithmetic on `Instant` saturates: in the browser a panic would end the
  instance.
- `RealInstant` is the real clock in both worlds (on servers
  `std::time::Instant`, unaffected by the paused test clock), for durations
  reported to people (a tool call's `duration_ms`).

Engine crates (`core`, `agent`, `providers`, `storage`) call `criogen_rt`
instead of `tokio::spawn` / `tokio::time`; they keep `tokio::sync`, which
needs no runtime. The Postgres code, gateway and telemetry keep Tokio
directly (servers only).

JavaScript values are never `Send`; the engine's futures are. WebAssembly
here runs on one thread, so `send_wrapper::SendWrapper` holds JavaScript
values in `Send` futures (it panics if one ever moved to another thread),
and the workspace keeps `unsafe_code = "forbid"`.

A panic in the browser ends the WebAssembly instance (`panic = abort` is the
only strategy there): `JoinError` only ever means "cancelled", and Rust
tool panics are not caught (in the browser tools are JavaScript, whose
throws are caught as before). The binding logs a panic to the console.

### 2. HTTP: a `fetch` backend

`Transport` keeps its API and its shared logic: four timeouts, refusing
redirects, the size limit, classifying errors, redacting keys. Only moving
the bytes is per platform: reqwest on servers, the host's global `fetch` in
JavaScript hosts (browsers, workers, Node 18+, Deno). The `fetch` backend:

- uses `redirect: "manual"` and refuses the result (an opaque redirect in
  browsers, a 3xx elsewhere): the key never follows a redirect (rule 10);
- refuses a chunk larger than what is left of `max_response_bytes` before
  copying it into WebAssembly memory (rule 11);
- aborts the request (`AbortController`) when the future or the body is
  dropped: a timeout or a cancelled run stops the download, and the provider
  stops generating;
- reports a failed `fetch` as `network` with the hint that the provider may
  not accept calls from a browser page (CORS), which a browser cannot tell
  apart from a network error.

The insecure-http rule (no key over plain `http` to another machine) is
checked before any transport, so it holds here too.

### 3. One bridge for both engines: `criogen-js`

The Node addon's logic (configurations, envelopes, results, errors, the
cancel registry, agents by handle, workflows, the gateway) moved from
`sdks/typescript/src/lib.rs` into the workspace crate `criogen-js`. A
JavaScript function is a `JsFunction` (request JSON in, a future of an
envelope out), a failure a `Failure` (`{code, message, details}`). The
bindings only adapt calling conventions:

- `sdks/typescript/src/lib.rs` (napi-rs): thread-safe functions, napi
  Promises, telemetry (servers only);
- `sdks/typescript/web/src/lib.rs` (wasm-bindgen): `js_sys::Function`,
  `future_to_promise`, in-memory storage only.

Both export the same JavaScript interface (`Addon` in `lib/native.ts`), so
the TypeScript layer is shared unchanged, and the TypeScript test suite runs
on both engines (`CRIOGEN_ENGINE=wasm`).

### 4. The TypeScript entry: `criogen/web`

`lib/native.ts` is now an engine registry: the Node loader
(`lib/native-node.ts`, the default) or the WebAssembly loader
(`lib/browser/engine.ts`, chosen by the browser entry). The browser entry
(`lib/browser/index.ts`) exports the same API as `criogen` except what needs
Node (`loadEnv` and friends); web tools are there but say they run on
servers. Environment variables are read through `host-env.ts` (a browser has
none: keys are passed as `apiKey`).

WebAssembly compiles asynchronously, so in the browser `await ready()` loads
the engine once, before anything else; a use before it throws
`native_unavailable` saying so. `ready({ wasm })` takes the engine's
location when it is not next to the script (a URL, a `Response`, bytes, or a
compiled `WebAssembly.Module`).

The package ships `dist-web/criogen.js` (one ES module: the TypeScript layer
and the glue), `dist-web/criogen_bg.wasm` and `dist-web/types`, exported as
`criogen/web`. `scripts/build-web.mjs` builds them (cargo, wasm-bindgen at
the exact version of the crate's lockfile, `wasm-opt -Os`, esbuild).

### 5. Errors

One new stable code, `unsupported` (family `other`): the feature needs a
server, and the engine runs as WebAssembly. Raised for a database URL,
encryption keys, telemetry export and web tools in the browser.

### 6. Sizes

The engine is about 1.45 MB of WebAssembly (about 550 KB compressed with
gzip), the library about 107 KB (27 KB gzipped), measured by
`build-web.mjs` at each build. It is fetched when `ready()` runs, then
cached by the browser.

### 7. Models written in JavaScript: `FunctionModel`

A model whose every call goes to a JavaScript function, on both engines:
`new FunctionModel(async (call) => answer)`. The function gets the request
as the engine sends it to a provider (`ModelRequest`: messages, tools,
settings, output) and answers with text, tool calls, an exact message, or a
classified error (`{ error: { kind: "rate_limited", … } }` is retried like a
provider's); a throw fails the call with `model_failed`. Functions are
registered with the engine by handle (`registerFunction`, released when the
TypeScript object is collected), so a `FunctionModel` goes anywhere a model
does: agents, fallbacks, the gateway, agents inside workflows.

Uses: a model running in the page (WebGPU, a browser's built-in model), a
company's own client, and scripted replies chosen by what the request says
(tests and demos where several agents share one model and run at once,
which an in-order script cannot answer). The playground uses it.

`Gateway.chat` takes `tools` (tool specs, as `call.tools`), so a
`FunctionModel` can pass an agent's request on to a real model through the
engine and return its message.

## Limits (by necessity)

- **One thread.** Tasks are concurrent, not parallel: model and tool calls
  overlap (they wait), CPU-heavy Rust work does not spread over cores.
- **Storage is in memory.** A thread lives as long as the page; databases,
  encryption at rest and the audit trail's durable backends need a server.
- **Background tabs.** Browsers slow timers in background tabs and phones
  may pause them: a long run can stall. Short interactive runs are the fit.
- **Keys.** A key used in a page is visible to whoever uses the page. For
  the playground the visitor's own key stays in their tab (never sent to
  our servers). In a product, point models at your own server or a Criogen
  gateway with per-user virtual keys.
- **CORS.** The provider must accept calls from a browser page (OpenAI,
  Anthropic with its direct-browser header, Gemini's OpenAI-compatible
  endpoint and OpenRouter do; a local Ollama needs `OLLAMA_ORIGINS`).
- **Edge runtimes** (Cloudflare Workers, Vercel Edge) are not tested yet;
  they load WebAssembly differently (`ready({ wasm: module })` is the hook).

## Performance

Criogen's own work per run is a fraction of a millisecond natively
(docs/benchmarks.md); a model call takes 0.3–5 s. WebAssembly is somewhat
slower than native and single-threaded, which matters little next to the
model. The difference users can feel is the network: each model call from a
browser pays the visitor's round trip (and sometimes a CORS preflight),
where a server near the provider pays its own, shorter one. No browser
numbers are claimed until they are measured and published separately
(rule 6).

## Tests

- `criogen-rt` as WebAssembly on a JavaScript event loop
  (`wasm-bindgen-test-runner` on Node): tasks, aborts, join order,
  concurrency (three 300 ms waits take about 300 ms), timers that never end
  early, timeouts, the clock.
- The whole TypeScript suite on the WebAssembly engine
  (`CRIOGEN_ENGINE=wasm node --test`); tests that need a server (SQLite,
  encryption, telemetry, Kaggle's credential files, loading the addon) are
  skipped there with the reason.
- Real browsers (Chromium, Firefox, WebKit) through Playwright
  (`test/browser`): tools, concurrent tool calls and 50 concurrent nodes
  (timed), timeouts, `AbortSignal`, streaming and leaving a stream early,
  approvals, typed output, hooks, tool exceptions, threads and rewind,
  workflows (routes, joins, goto, suspend/resume, deadlines), the gateway,
  and a provider on another origin through `fetch`: JSON and SSE, a retried
  503, a refused redirect that is never followed, a 401 echoing the key
  (the key appears nowhere in the error), a server without CORS, a body over
  64 MiB, plain http to another machine; a Web Worker.
- The package (rule 9): `npm pack`, installed into a new project, an app
  importing `criogen/web` bundled by esbuild and run in Chromium, then the
  browser tests against the installed files.
- `scripts/test-web.sh` runs all of it; `ci-local.sh` step `web`.
