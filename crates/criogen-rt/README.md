# criogen-rt

Tasks and timers for [Criogen](https://criogenai.com)'s engine, on every
platform it runs on (design 0021).

- **Servers** (any target with an operating system): this crate is Tokio.
  `spawn`, `JoinSet`, `sleep`, `timeout` and `Instant` are Tokio's own, so
  nothing changes there, including the paused clock the engine's tests use.
- **The browser and other JavaScript hosts** (Node, Deno, edge runtimes)
  running the engine as WebAssembly (`wasm32-unknown-unknown`): there is no
  Tokio runtime. Tasks run on the JavaScript event loop, timers are
  `setTimeout`, and the clock is `performance.now()`.

The API is the part of Tokio the engine uses, with the same behaviour, so the
engine is written once. In the browser a panic ends the WebAssembly instance
(`panic = abort`), so `JoinError` there only ever means "cancelled".

Internal to Criogen: other crates should keep using Tokio directly.
